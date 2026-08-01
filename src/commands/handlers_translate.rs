//! `/translate` — per-buffer control of near-real-time translation.
//!
//! Subcommand names deliberately mirror the user's existing `WeeChat` and
//! irssi translate scripts (`addin`, `delin`, `addout`, `delout`, `list`),
//! so the muscle memory carries over.

use crate::app::App;
use crate::commands::helpers::add_local_event;
use crate::commands::types::{C_DIM, C_ERR, C_RST};
use crate::config::TranslateBufferConfig;
use crate::state::buffer::make_buffer_id;

/// Which direction a subcommand touches.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    In,
    Out,
}

pub fn cmd_translate(app: &mut App, args: &[String]) {
    let Some(sub) = args.first().map(|s| s.to_lowercase()) else {
        usage(app);
        return;
    };
    match sub.as_str() {
        "list" => list(app),
        "status" => status(app),
        "addin" => add(app, args.get(1..).unwrap_or_default(), Dir::In),
        "addout" => add(app, args.get(1..).unwrap_or_default(), Dir::Out),
        "delin" => del(app, args.get(1..).unwrap_or_default(), Dir::In),
        "delout" => del(app, args.get(1..).unwrap_or_default(), Dir::Out),
        other => {
            add_local_event(app, &format!("{C_ERR}translate: unknown subcommand '{other}'{C_RST}"));
            usage(app);
        }
    }
}

fn usage(app: &mut App) {
    for line in [
        "Usage: /translate list",
        "       /translate status",
        "       /translate addin  <#channel|nick> [lang] [my-lang]",
        "       /translate delin  <#channel|nick>",
        "       /translate addout <#channel|nick> <lang> [my-lang]",
        "       /translate delout <#channel|nick>",
    ] {
        add_local_event(app, line);
    }
    add_local_event(
        app,
        &format!(
            "{C_DIM}<lang> is the language the CHANNEL speaks; [my-lang] overrides \
             translate.my_lang for this buffer only.{C_RST}"
        ),
    );
}

/// Resolve a user-typed target to a buffer id on the active connection.
///
/// Returns `None` (after telling the user) when there is no active
/// connection — a buffer id is meaningless without one.
fn resolve_target(app: &mut App, target: &str) -> Option<(String, String)> {
    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, &format!("{C_ERR}translate: no active connection{C_RST}"));
        return None;
    };
    // The startup Status buffer reports the placeholder connection rather
    // than none at all, so without this `/translate addin #chan de` typed
    // there reports success and persists `_default/#chan` — a mapping no
    // real connection can ever match, and one the user has no reason to
    // suspect is inert.
    if conn_id == App::DEFAULT_CONN_ID || !app.state.connections.contains_key(&conn_id) {
        add_local_event(
            app,
            &format!(
                "{C_ERR}translate: not on a server — switch to a connected \
                 buffer first{C_RST}"
            ),
        );
        return None;
    }
    let buffer_id = make_buffer_id(&conn_id, target);
    Some((conn_id, buffer_id))
}

fn add(app: &mut App, args: &[String], dir: Dir) {
    let Some(target) = args.first() else {
        add_local_event(
            app,
            &format!("{C_ERR}translate: name a channel or nick{C_RST}"),
        );
        return;
    };
    let given_lang = args.get(1).map(|s| s.trim().to_lowercase());
    let given_my_lang = args.get(2).map(|s| s.trim().to_lowercase());
    let Some((conn_id, buffer_id)) = resolve_target(app, target) else {
        return;
    };

    // Translating an E2E conversation would hand its plaintext to a
    // third-party provider, destroying the guarantee E2E exists to provide.
    // There is no override.
    //
    // This refusal is a convenience, not the enforcement: the real gate runs
    // where the request is built, in both directions, so turning E2E on
    // AFTER enabling translation is equally covered. Fail-closed predicate,
    // never the advisory one.
    if app.state.e2e_possible_for_target(&conn_id, target) {
        add_local_event(
            app,
            &format!(
                "{C_ERR}translate: refused — {target} is end-to-end encrypted.{C_RST}"
            ),
        );
        add_local_event(
            app,
            &format!(
                "{C_DIM}Translating would send its plaintext to a third-party \
                 provider. This cannot be overridden.{C_RST}"
            ),
        );
        return;
    }

    let given_lang = given_lang.filter(|l| !l.is_empty());
    let given_my_lang = given_my_lang.filter(|l| !l.is_empty());

    // Outgoing needs to know which language to WRITE in, and that cannot be
    // detected — there is nothing to detect it from. Refuse rather than
    // enable a direction that could only ever fall through untranslated.
    // Incoming is different: with no language the broker detects the
    // source, which is the common case.
    if dir == Dir::Out
        && given_lang.is_none()
        && app
            .config
            .translate
            .buffers
            .get(&buffer_id)
            .and_then(|c| c.lang.as_ref())
            .is_none()
    {
        add_local_event(
            app,
            &format!(
                "{C_ERR}translate: addout needs the language {target} is written \
                 in — e.g. /translate addout {target} de{C_RST}"
            ),
        );
        return;
    }

    let entry = app
        .config
        .translate
        .buffers
        .entry(buffer_id)
        .or_default();
    match dir {
        Dir::In => entry.incoming = true,
        Dir::Out => entry.outgoing = true,
    }
    if let Some(lang) = given_lang {
        entry.lang = Some(lang);
    }
    if let Some(mine) = given_my_lang {
        entry.my_lang = Some(mine);
    }
    app.sync_translate_from_config();
    persist(app);

    let what = if dir == Dir::In { "incoming" } else { "outgoing" };
    add_local_event(app, &format!("translate: {what} enabled for {target}"));
    if !app.config.translate.enabled {
        add_local_event(
            app,
            &format!(
                "{C_DIM}translate.enabled is off — nothing is translated until \
                 you /set translate.enabled true{C_RST}"
            ),
        );
    }
}

fn del(app: &mut App, args: &[String], dir: Dir) {
    let Some(target) = args.first() else {
        add_local_event(
            app,
            &format!("{C_ERR}translate: name a channel or nick{C_RST}"),
        );
        return;
    };
    let Some((_, buffer_id)) = resolve_target(app, target) else {
        return;
    };
    let Some(entry) = app.config.translate.buffers.get_mut(&buffer_id) else {
        add_local_event(
            app,
            &format!("translate: {target} was not enabled for translation"),
        );
        return;
    };
    match dir {
        Dir::In => entry.incoming = false,
        Dir::Out => entry.outgoing = false,
    }
    if !entry.incoming && !entry.outgoing {
        app.config.translate.buffers.remove(&buffer_id);
    }
    app.sync_translate_from_config();
    persist(app);
    match dir {
        // Lines already in flight must be released, not dropped: the user has
        // seen them arrive on the network, and losing them silently would be
        // worse than showing them untranslated. Only the incoming ones,
        // though — a full flush would also drop the reservations held by
        // outgoing sends that are still in flight, and those are a direction
        // this command did not touch.
        Dir::In => app.flush_pending_translations(&buffer_id),
        // Nothing to flush. The queue holds no outgoing WORK, only the
        // reservations of sends already dispatched, whose outcomes are still
        // coming and will fill them. Releasing those would misplace the very
        // echoes the reservations exist to position — and forcing the
        // buffer's incoming lines out as timeouts, which is what the full
        // flush did here, punishes a direction that is still enabled.
        Dir::Out => {}
    }

    let what = if dir == Dir::In { "incoming" } else { "outgoing" };
    add_local_event(app, &format!("translate: {what} disabled for {target}"));
}

/// Write the updated per-buffer map to disk.
///
/// These commands ARE the documented way to manage a persisted map, so
/// without this every `addin`/`addout`/`delin`/`delout` silently evaporated
/// on the next restart or `/reload`. A write failure is surfaced rather than
/// swallowed: the user needs to know the setting only holds for this session.
fn persist(app: &mut App) {
    let path = app.config_path.clone();
    if let Err(e) = crate::config::save_config(&path, &app.config) {
        add_local_event(
            app,
            &format!(
                "{C_ERR}translate: setting applied but NOT saved: {e}{C_RST}"
            ),
        );
    }
}

fn list(app: &mut App) {
    if app.config.translate.buffers.is_empty() {
        add_local_event(app, "translate: no buffers configured");
        return;
    }
    add_local_event(
        app,
        &format!(
            "translate: my_lang={} master={}",
            app.config.translate.my_lang,
            if app.config.translate.enabled {
                "on"
            } else {
                "off"
            }
        ),
    );
    let my_lang = app.config.translate.my_lang.clone();
    let mut rows: Vec<(String, TranslateBufferConfig)> = app
        .config
        .translate
        .buffers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (buffer_id, cfg) in rows {
        // Print the actual arrows per direction rather than a bare language
        // list. Which language is the source and which the target is exactly
        // what is easy to get backwards, so it should be readable at a
        // glance instead of inferred.
        let pair = crate::translate::resolve_langs(Some(&cfg), &my_lang);
        let mut dirs = Vec::new();
        if cfg.incoming {
            let (src, dst) = pair.incoming();
            dirs.push(format!(
                "in: {}→{dst}",
                src.as_deref().unwrap_or("auto")
            ));
        }
        if cfg.outgoing {
            match pair.outgoing() {
                Some((src, dst)) => {
                    dirs.push(format!("out: {}→{dst}", src.as_deref().unwrap_or("auto")));
                }
                // Reachable if the language was removed from config.toml by
                // hand after `addout` set it. Say so rather than printing a
                // direction that cannot run.
                None => dirs.push("out: NO LANGUAGE SET".to_string()),
            }
        }
        add_local_event(app, &format!("  {buffer_id}  {}", dirs.join("   ")));
    }
}

fn status(app: &mut App) {
    let active = if app.state.translate_active {
        "active"
    } else if app.config.translate.enabled {
        "enabled but no backend (restart required)"
    } else {
        "off"
    };
    add_local_event(app, &format!("translate: {active}"));
    if app.state.translate_queues.is_empty() {
        add_local_event(app, "  no lines in flight");
        return;
    }
    let mut rows: Vec<(String, usize, usize)> = app
        .state
        .translate_queues
        .iter()
        .map(|(id, q)| (id.clone(), q.pending_len(), q.len()))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (buffer_id, pending, total) in rows {
        add_local_event(
            app,
            &format!("  {buffer_id}  {pending} in flight, {total} queued"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input::submit_typing_tests::test_app;
    use crate::state::buffer::{Buffer, BufferType};

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    fn app_with_channel() -> App {
        let mut app = test_app();
        // A real connection, not just a buffer: `resolve_target` requires
        // one, because the startup Status buffer otherwise persists a
        // `_default/...` mapping that can never match a live connection.
        let mut conn = crate::state::events::tests::make_test_connection();
        conn.id = "test".to_string();
        conn.label = "TestServer".to_string();
        app.state.add_connection(conn);
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        app.state.set_active_buffer("test/#dupa");
        app
    }

    fn last_event(app: &App) -> String {
        app.state
            .buffers
            .values()
            .flat_map(|b| b.messages.iter())
            .last()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn addin_enables_incoming_only() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addin", "#dupa", "de"]));
        let cfg = &app.config.translate.buffers["test/#dupa"];
        assert!(cfg.incoming);
        assert!(!cfg.outgoing, "addin must not enable the outgoing side");
        assert_eq!(cfg.lang.as_deref(), Some("de"));
        assert!(
            app.state.translate_buffers.contains_key("test/#dupa"),
            "the state mirror is synced immediately"
        );
    }

    #[test]
    fn addout_enables_outgoing_only() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addout", "#dupa", "de"]));
        let cfg = &app.config.translate.buffers["test/#dupa"];
        assert!(cfg.outgoing);
        assert!(!cfg.incoming);
        assert_eq!(cfg.lang.as_deref(), Some("de"));
    }

    #[test]
    fn a_change_is_written_to_the_configured_path() {
        // These commands ARE the documented way to manage a persisted map,
        // so a change that only lives in memory evaporates on restart.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        let mut app = app_with_channel();
        app.config_path.clone_from(&path);

        cmd_translate(&mut app, &args(&["addin", "#dupa", "de"]));

        let written = std::fs::read_to_string(&path).expect("the config was saved");
        assert!(
            written.contains("[translate.buffers"),
            "the per-buffer map is in the file: {written}"
        );
        assert!(written.contains("test/#dupa"), "including this buffer");
    }

    #[test]
    fn tests_never_write_the_real_config_path() {
        // Guard against the hazard directly: a handler that saves
        // unconditionally would otherwise clobber the developer's own
        // ~/.repartee/config.toml during `cargo test`.
        let app = app_with_channel();
        assert_ne!(
            app.config_path,
            crate::constants::config_path(),
            "the test App must never point at the real config"
        );
    }

    #[test]
    fn the_default_status_buffer_cannot_be_configured() {
        // `active_conn_id()` reports the placeholder connection there rather
        // than none, so without an explicit check this reported success and
        // persisted a `_default/...` mapping no connection can ever match.
        let mut app = test_app();
        cmd_translate(&mut app, &args(&["addin", "#chan", "de"]));
        assert!(
            app.config.translate.buffers.is_empty(),
            "nothing may be persisted from the Status buffer"
        );
    }

    #[test]
    fn addout_without_a_language_is_refused() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addout", "#dupa"]));
        assert!(
            last_event(&app).contains("needs the language"),
            "got: {}",
            last_event(&app)
        );
        assert!(
            !app.config.translate.buffers.contains_key("test/#dupa"),
            "a direction that could never work must not be enabled"
        );
    }

    #[test]
    fn addout_reuses_the_language_addin_already_set() {
        // The language belongs to the buffer, not to a direction, so naming
        // it once is enough.
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addin", "#dupa", "de"]));
        cmd_translate(&mut app, &args(&["addout", "#dupa"]));
        let cfg = &app.config.translate.buffers["test/#dupa"];
        assert!(cfg.incoming && cfg.outgoing);
        assert_eq!(cfg.lang.as_deref(), Some("de"));
    }

    #[test]
    fn delin_leaves_the_outgoing_side_alone() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addin", "#dupa"]));
        cmd_translate(&mut app, &args(&["addout", "#dupa", "de"]));
        cmd_translate(&mut app, &args(&["delin", "#dupa"]));
        let cfg = &app.config.translate.buffers["test/#dupa"];
        assert!(!cfg.incoming);
        assert!(cfg.outgoing);
    }

    #[test]
    fn removing_both_directions_drops_the_entry() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addin", "#dupa"]));
        cmd_translate(&mut app, &args(&["delin", "#dupa"]));
        assert!(!app.config.translate.buffers.contains_key("test/#dupa"));
        assert!(!app.state.translate_buffers.contains_key("test/#dupa"));
    }

    #[test]
    fn delin_on_an_unconfigured_buffer_says_so() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["delin", "#dupa"]));
        assert!(last_event(&app).contains("was not enabled"));
    }

    #[test]
    fn an_unknown_subcommand_reports_and_shows_usage() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["frobnicate"]));
        let texts: Vec<String> = app.state.buffers["test/#dupa"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(texts.iter().any(|t| t.contains("unknown subcommand")));
        assert!(texts.iter().any(|t| t.contains("Usage: /translate list")));
    }

    #[test]
    fn addin_without_a_target_is_rejected() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addin"]));
        assert!(last_event(&app).contains("name a channel or nick"));
        assert!(app.config.translate.buffers.is_empty());
    }

    #[test]
    fn list_reports_nothing_when_unconfigured() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["list"]));
        assert!(last_event(&app).contains("no buffers configured"));
    }

    #[test]
    fn list_spells_out_the_direction_of_each_translation() {
        // Which language is source and which is target is exactly what is
        // easy to get backwards, so `list` prints arrows rather than a bare
        // language and leaves the reader to infer.
        let mut app = app_with_channel();
        app.config.translate.my_lang = "pl".to_string();
        cmd_translate(&mut app, &args(&["addin", "#dupa", "de"]));
        cmd_translate(&mut app, &args(&["addout", "#dupa"]));
        cmd_translate(&mut app, &args(&["list"]));
        let texts: Vec<String> = app.state.buffers["test/#dupa"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("in: de→pl") && t.contains("out: pl→de")),
            "list output: {texts:?}"
        );
    }

    #[test]
    fn list_reports_an_outgoing_direction_that_cannot_run() {
        // Reachable by editing config.toml by hand. Printing a direction
        // that will never translate anything, with no hint, would be worse.
        let mut app = app_with_channel();
        app.config.translate.buffers.insert(
            "test/#dupa".to_string(),
            crate::config::TranslateBufferConfig {
                incoming: false,
                outgoing: true,
                lang: None,
                my_lang: None,
            },
        );
        cmd_translate(&mut app, &args(&["list"]));
        assert!(
            last_event(&app).contains("NO LANGUAGE SET"),
            "got: {}",
            last_event(&app)
        );
    }

    #[test]
    fn a_third_argument_overrides_our_language_for_that_buffer() {
        let mut app = app_with_channel();
        app.config.translate.my_lang = "pl".to_string();
        cmd_translate(&mut app, &args(&["addin", "#dupa", "zh", "en"]));
        let cfg = &app.config.translate.buffers["test/#dupa"];
        assert_eq!(cfg.lang.as_deref(), Some("zh"));
        assert_eq!(
            cfg.my_lang.as_deref(),
            Some("en"),
            "this buffer is read in English while the rest stay Polish"
        );
        assert_eq!(
            app.config.translate.my_lang, "pl",
            "the global default is untouched"
        );
    }

    #[test]
    fn status_reports_off_when_the_master_switch_is_down() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["status"]));
        let texts: Vec<String> = app.state.buffers["test/#dupa"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(texts.iter().any(|t| t.contains("translate: off")));
        assert!(texts.iter().any(|t| t.contains("no lines in flight")));
    }

    #[test]
    fn enabling_a_buffer_while_the_master_switch_is_off_warns() {
        let mut app = app_with_channel();
        cmd_translate(&mut app, &args(&["addin", "#dupa"]));
        assert!(
            last_event(&app).contains("translate.enabled is off"),
            "the user must not think it is live: {}",
            last_event(&app)
        );
    }

    const BUF: &str = "test/#dupa";

    /// A queue holding one incoming line still being translated and, behind
    /// it, an outgoing echo's reservation — the mixed state both `del`
    /// directions have to reason about.
    fn app_with_mixed_queue() -> App {
        let mut app = app_with_channel();
        app.config.translate.buffers.insert(
            BUF.to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: true,
                lang: Some("de".to_string()),
                my_lang: None,
            },
        );
        let mut queue = crate::translate::queue::TranslateQueue::new();
        let msg = crate::state::buffer::Message {
            id: 1,
            timestamp: chrono::Utc::now(),
            message_type: crate::state::buffer::MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: "guten tag".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        };
        queue.push_pending(
            1,
            "guten tag".to_string(),
            crate::translate::queue::PendingPayload {
                message: msg,
                activity: crate::state::buffer::ActivityLevel::Activity,
                show_original: false,
            },
        );
        queue.reserve(2); // an outgoing send still being translated
        app.state.translate_queues.insert(BUF.to_string(), queue);
        app
    }

    fn shown(app: &App) -> Vec<String> {
        app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    #[test]
    fn delout_leaves_the_incoming_lines_alone() {
        // Outgoing is a different direction. Flushing the shared queue forced
        // lines that were translating perfectly well to render as timeouts,
        // on a buffer where incoming translation is still switched on.
        let mut app = app_with_mixed_queue();
        cmd_translate(&mut app, &args(&["delout", "#dupa"]));
        assert!(
            !shown(&app).iter().any(|t| t.contains("untranslated")),
            "no incoming line may be forced out by disabling the other \
             direction: {:?}",
            shown(&app)
        );
        assert_eq!(
            app.state.translate_queues[BUF].len(),
            2,
            "both the in-flight line and the reservation are untouched"
        );
    }

    #[test]
    fn delin_releases_incoming_lines_but_keeps_outgoing_reservations() {
        // The pending line must not be lost — the user saw it arrive — but
        // the reservation belongs to a send already dispatched, whose
        // outcome is still coming. Dropping it lets newer rows overtake the
        // user's own message.
        let mut app = app_with_mixed_queue();
        cmd_translate(&mut app, &args(&["delin", "#dupa"]));
        assert!(
            shown(&app).iter().any(|t| t.contains("guten tag")),
            "the in-flight line is released, not dropped: {:?}",
            shown(&app)
        );
        let queue = app
            .state
            .translate_queues
            .get(BUF)
            .expect("the reservation keeps the queue alive");
        assert_eq!(
            queue.len(),
            1,
            "and the reservation is what remains — the echo it holds a place              for is still coming"
        );
    }
}
