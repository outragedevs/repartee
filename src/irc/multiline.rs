//! Pure `IRCv3` `draft/multiline` logic: cap-value parsing, outbound
//! partitioning + wire framing, and inbound reassembly. No I/O, no `AppState`.
//!
//! See <https://ircv3.net/specs/extensions/multiline>.

// TEMPORARY (remove in Phase 4 once partition/multiline_frames are wired into
// the outbound path): these pure fns are consumed by later phases; until then
// the binary sees them as dead.
#![allow(dead_code)]

use crate::irc::{MESSAGE_MAX_BYTES, MULTILINE_DEFAULT_MAX_LINES};

/// Server-advertised per-batch limits from the `draft/multiline` cap value.
///
/// `max_bytes` is the TOTAL combined payload of one batch — the sum of every
/// line's content bytes plus one byte for each joining `\n`. It is NOT a
/// per-line cap; the per-PRIVMSG wire cap is the separate [`MESSAGE_MAX_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultilineLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
}

/// One physical PRIVMSG line within a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireLine {
    pub text: String,
    /// `true` ⇒ carries `draft/multiline-concat` (join to the previous line
    /// with no separator). `false` ⇒ a logical-line boundary (join with `\n`).
    pub concat: bool,
}

/// Parse the `draft/multiline` cap value into limits. `None` ⇒ unusable
/// (no `max-bytes`, or `max-bytes` below one full wire line).
///
/// Deviation from lurker (intentional): lurker defaults `max-bytes` to 4096
/// when the value is empty; we return `None` because the spec marks `max-bytes`
/// REQUIRED. Real servers always advertise it, so practical impact is nil — do
/// not "fix" this back to a lenient default.
#[must_use]
pub fn parse_limits(cap_value: Option<&str>) -> Option<MultilineLimits> {
    let value = cap_value?;
    let mut max_bytes: Option<usize> = None;
    let mut max_lines: Option<usize> = None;
    for token in value.split(',') {
        let Some((k, v)) = token.split_once('=') else {
            continue;
        };
        match k {
            "max-bytes" => max_bytes = v.parse().ok(),
            "max-lines" => max_lines = v.parse().ok(),
            _ => {}
        }
    }
    let max_bytes = max_bytes.filter(|&b| b >= MESSAGE_MAX_BYTES)?;
    Some(MultilineLimits {
        max_bytes,
        max_lines: max_lines
            .filter(|&n| n > 0)
            .unwrap_or(MULTILINE_DEFAULT_MAX_LINES),
    })
}

/// True if `text` must be sent as multiline: it contains `\n`, or its byte
/// length exceeds the per-PRIVMSG cap ([`MESSAGE_MAX_BYTES`]) — a single
/// over-long line that benefits from seamless concat reassembly.
#[must_use]
pub fn needs_multiline(text: &str) -> bool {
    text.contains('\n') || text.len() > MESSAGE_MAX_BYTES
}

/// Reassemble one batch's collected lines into a single logical string: join
/// with `\n` unless a line carries `concat` (then no separator).
#[must_use]
pub fn reassemble(lines: &[WireLine]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 && !line.concat {
            out.push('\n');
        }
        out.push_str(&line.text);
    }
    out
}

/// Split `line` at UTF-8 char boundaries so the pieces concatenated with NO
/// separator reproduce `line` exactly — required for `draft/multiline-concat`,
/// where the receiver rejoins continuation pieces seamlessly. (Unlike
/// `split_irc_message`, which trims whitespace at word boundaries and would make
/// concat-rejoin lossy.)
fn split_lossless(line: &str, max_bytes: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if !cur.is_empty() && cur.len() + ch.len_utf8() > max_bytes {
            pieces.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    pieces
}

/// Partition `text` into one or more batches. `None` ⇒ cannot be represented as
/// multiline (caller must fall back to legacy per-line sending).
///
/// Algorithm: split on user `\n` into logical lines; split any over-long logical
/// line into byte-exact concat pieces; pack lines into batches, opening a new
/// batch when adding a line would exceed `max_lines` (count) or `max_bytes`
/// (cumulative content bytes + 1 per joining `\n`). A single logical line whose
/// pieces alone exceed either limit ⇒ `None`. All-blank input ⇒ `None`.
#[must_use]
pub fn partition(text: &str, limits: &MultilineLimits) -> Option<Vec<Vec<WireLine>>> {
    let logical: Vec<Vec<WireLine>> = text
        .split('\n')
        .map(|line| {
            if line.len() <= MESSAGE_MAX_BYTES {
                vec![WireLine {
                    text: line.to_string(),
                    concat: false,
                }]
            } else {
                split_lossless(line, MESSAGE_MAX_BYTES)
                    .into_iter()
                    .enumerate()
                    .map(|(i, piece)| WireLine {
                        text: piece,
                        concat: i > 0,
                    })
                    .collect()
            }
        })
        .collect();

    if logical
        .iter()
        .all(|pieces| pieces.iter().all(|w| w.text.is_empty()))
    {
        return None; // all-blank message (spec forbids)
    }

    let mut batches: Vec<Vec<WireLine>> = Vec::new();
    let mut cur: Vec<WireLine> = Vec::new();
    let mut cur_bytes = 0usize;

    for pieces in logical {
        let add_lines = pieces.len();
        let add_bytes: usize = pieces.iter().map(|w| w.text.len()).sum();
        // A single logical line that can never fit one batch ⇒ unrepresentable.
        if add_lines > limits.max_lines || add_bytes > limits.max_bytes {
            return None;
        }
        let join_byte = usize::from(!cur.is_empty());
        let would_lines = cur.len() + add_lines;
        let would_bytes = cur_bytes + join_byte + add_bytes;
        if !cur.is_empty() && (would_lines > limits.max_lines || would_bytes > limits.max_bytes) {
            batches.push(std::mem::take(&mut cur));
            cur_bytes = 0;
        }
        let join_byte = usize::from(!cur.is_empty());
        cur_bytes += join_byte + add_bytes;
        cur.extend(pieces);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    Some(batches)
}

/// Build the wire messages for one `draft/multiline` batch:
/// `BATCH +ref draft/multiline <target>`, one `@batch=ref` PRIVMSG per line
/// (continuations also tagged `draft/multiline-concat`), then `BATCH -ref`.
#[must_use]
pub fn multiline_frames(target: &str, batch_ref: &str, batch: &[WireLine]) -> Vec<irc::proto::Message> {
    let mut msgs: Vec<irc::proto::Message> = Vec::with_capacity(batch.len() + 2);
    msgs.push(irc::proto::Message::from(irc::proto::Command::BATCH(
        format!("+{batch_ref}"),
        Some(irc::proto::command::BatchSubCommand::CUSTOM(
            "draft/multiline".to_string(),
        )),
        Some(vec![target.to_string()]),
    )));
    for line in batch {
        let mut tags = vec![irc::proto::message::Tag(
            "batch".to_string(),
            Some(batch_ref.to_string()),
        )];
        if line.concat {
            tags.push(irc::proto::message::Tag(
                "draft/multiline-concat".to_string(),
                None,
            ));
        }
        msgs.push(irc::proto::Message {
            tags: Some(tags),
            prefix: None,
            command: irc::proto::Command::PRIVMSG(target.to_string(), line.text.clone()),
        });
    }
    msgs.push(irc::proto::Message::from(irc::proto::Command::BATCH(
        format!("-{batch_ref}"),
        None,
        None,
    )));
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim(b: usize, l: usize) -> MultilineLimits {
        MultilineLimits {
            max_bytes: b,
            max_lines: l,
        }
    }

    #[test]
    fn parse_limits_full() {
        let l = parse_limits(Some("max-bytes=4096,max-lines=24")).unwrap();
        assert_eq!(
            l,
            MultilineLimits {
                max_bytes: 4096,
                max_lines: 24
            }
        );
    }
    #[test]
    fn parse_limits_missing_max_lines_uses_default() {
        let l = parse_limits(Some("max-bytes=8192")).unwrap();
        assert_eq!(l.max_bytes, 8192);
        assert_eq!(l.max_lines, MULTILINE_DEFAULT_MAX_LINES);
    }
    #[test]
    fn parse_limits_rejects_small_or_absent_max_bytes() {
        assert!(parse_limits(Some("max-lines=10")).is_none());
        assert!(parse_limits(Some("max-bytes=100,max-lines=10")).is_none()); // < 350
        assert!(parse_limits(Some("")).is_none());
        assert!(parse_limits(None).is_none());
    }
    #[test]
    fn parse_limits_ignores_unknown_and_garbage() {
        let l = parse_limits(Some("max-bytes=4096,foo=bar,bareflag,max-lines=zzz")).unwrap();
        assert_eq!(l.max_bytes, 4096);
        assert_eq!(l.max_lines, MULTILINE_DEFAULT_MAX_LINES);
    }

    #[test]
    fn needs_multiline_triggers() {
        assert!(needs_multiline("a\nb"));
        assert!(needs_multiline(&"x".repeat(MESSAGE_MAX_BYTES + 1)));
        assert!(!needs_multiline("short single line"));
    }

    #[test]
    fn reassemble_newline_and_concat() {
        let lines = vec![
            WireLine {
                text: "hello".into(),
                concat: false,
            },
            WireLine {
                text: "world".into(),
                concat: false,
            },
            WireLine {
                text: "!!!".into(),
                concat: true,
            },
        ];
        assert_eq!(reassemble(&lines), "hello\nworld!!!");
    }
    #[test]
    fn reassemble_single() {
        assert_eq!(
            reassemble(&[WireLine {
                text: "x".into(),
                concat: false
            }]),
            "x"
        );
    }

    #[test]
    fn partition_simple_three_lines() {
        let b = partition("a\nb\nc", &lim(4096, 24)).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(
            b[0],
            vec![
                WireLine {
                    text: "a".into(),
                    concat: false
                },
                WireLine {
                    text: "b".into(),
                    concat: false
                },
                WireLine {
                    text: "c".into(),
                    concat: false
                },
            ]
        );
    }
    #[test]
    fn partition_long_line_gets_concat() {
        let long = "x".repeat(MESSAGE_MAX_BYTES + 10);
        let b = partition(&long, &lim(4096, 24)).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].len(), 2);
        assert!(!b[0][0].concat);
        assert!(b[0][1].concat);
    }
    #[test]
    fn partition_overflows_max_lines_into_multiple_batches() {
        let text = (0..5).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let b = partition(&text, &lim(4096, 2)).unwrap();
        assert_eq!(b.len(), 3); // 2 + 2 + 1
        assert_eq!(b[0].len(), 2);
        assert_eq!(b[2].len(), 1);
    }
    #[test]
    fn partition_roundtrip_lossless() {
        let text = "alpha\nbeta gamma\n\ndelta";
        let b = partition(text, &lim(4096, 24)).unwrap();
        let rejoined = b
            .iter()
            .map(|batch| reassemble(batch))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rejoined, text);
    }
    #[test]
    fn partition_roundtrip_exercises_concat_split() {
        let long_line = "word ".repeat(120); // ~600 bytes, > 350, many spaces
        let text = format!("intro\n{long_line}\noutro");
        let b = partition(&text, &lim(8192, 24)).unwrap();
        let rejoined = b
            .iter()
            .map(|batch| reassemble(batch))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rejoined, text);
        assert!(b.iter().flatten().any(|w| w.concat));
    }
    #[test]
    fn partition_unrepresentable_single_line_returns_none() {
        let long = "y".repeat(MESSAGE_MAX_BYTES * 5);
        assert!(partition(&long, &lim(MESSAGE_MAX_BYTES + 1, 24)).is_none());
    }
    #[test]
    fn partition_all_blank_returns_none() {
        assert!(partition("\n\n", &lim(4096, 24)).is_none());
    }

    #[test]
    fn frames_have_batch_open_tagged_lines_close() {
        // This proto only emits the trailing ':' when the last param is empty,
        // contains a space, or starts with ':'. Use multi-word line text so the
        // ':' appears and is asserted realistically.
        let batch = vec![
            WireLine {
                text: "hello world".into(),
                concat: false,
            },
            WireLine {
                text: "more text".into(),
                concat: true,
            },
        ];
        let msgs = multiline_frames("#chan", "ml1", &batch);
        let wire: Vec<String> = msgs.iter().map(std::string::ToString::to_string).collect();
        assert_eq!(wire[0], "BATCH +ml1 draft/multiline #chan\r\n");
        assert!(wire[1].starts_with("@batch=ml1 ") && wire[1].contains("PRIVMSG #chan :hello world"));
        assert!(
            wire[2].contains("draft/multiline-concat") && wire[2].contains("PRIVMSG #chan :more text")
        );
        assert_eq!(wire[3], "BATCH -ml1\r\n");
    }
}
