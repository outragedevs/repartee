// Test builds only, and only because the diagnostic has no span to attach an
// `#[allow]` to: clippy reports it against `main.rs:1` with no source text, so
// it cannot be silenced where it is raised or even attributed to a function.
// It appears once for the whole test binary and moves between unrelated tests
// as the crate grows — it followed the translate concurrency stress test, the
// oversized-answer test, and a test that allocates nothing at all in turn.
// Nothing in this crate declares a large stack array; suppressing it here
// beats deleting tests that cover real races to appease it.
#![cfg_attr(test, allow(clippy::large_stack_arrays))]

mod app;
mod commands;
mod config;
mod constants;
mod dcc;
mod e2e;
mod emotes;
mod fs_secure;
mod image_preview;
mod irc;
mod nick_color;
mod scripting;
mod session;
mod shell;
mod shrink;
mod spellcheck;
mod state;
mod storage;
mod theme;
mod translate;
mod ui;
mod web;

// Swap glibc ptmalloc2 for jemalloc on Linux. glibc fragments its arena under
// bursty allocation patterns in long-running processes — we observed 3 GB RSS
// growth on Debian before the v0.8.4 chat_view render-budget fix, and even
// post-fix the baseline working set drifts upward over weeks of uptime.
// jemalloc returns memory to the OS more aggressively and is already the
// default system allocator on FreeBSD, so this brings Linux in line with BSD.
// macOS keeps libsystem_malloc — no #[cfg] coverage here means the dep is not
// even pulled into the build graph on non-Linux targets. See
// docs/superpowers/specs/2026-04-10-v084-oom-fix-design.md for rationale.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use color_eyre::eyre::{Result, eyre};
use tracing_subscriber::EnvFilter;

const BACKEND_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const BACKEND_CONNECT_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

fn log_path() -> std::path::PathBuf {
    constants::home_dir().join(format!("{}.log", constants::APP_NAME))
}

fn setup_logging() {
    let log_dir = constants::home_dir();
    if std::fs::create_dir_all(&log_dir).is_err() {
        // Without a writable home dir there's nowhere to log; subscribers
        // never get installed, but startup must still continue.
        return;
    }
    let Ok(log_file) = std::fs::File::options()
        .create(true)
        .append(true)
        .open(log_path())
    else {
        return;
    };
    // Default to WARN so the log file always carries enough breadcrumbs to
    // diagnose silent post-fork crashes ("No session found for PID X")
    // without forcing the user to remember `RUST_LOG=info` first. Users can
    // still raise/lower the level via `RUST_LOG`.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(log_file)
        .with_ansi(false)
        .init();
}

/// One row of `--help`: how the user spells it, what it takes, what it does.
///
/// [`SUBCOMMANDS`] and [`OPTIONS`] are the only place the launch surface is
/// written down. A test in this file reads this file's own source and proves
/// every argv literal `main` dispatches on has a row here, so a new flag
/// cannot ship with an unchanged help screen.
struct CliOption {
    /// Every spelling, comma-separated, exactly as the user types it.
    forms: &'static str,
    /// Value placeholder such as `<IP>`, or empty for a switch.
    value: &'static str,
    /// Description, pre-split into lines. The column width is fixed at compile
    /// time rather than wrapped at runtime, so the output never depends on
    /// `$COLUMNS`.
    help: &'static [&'static str],
}

impl CliOption {
    /// `forms` plus the value placeholder, as one printed label.
    fn label(&self) -> String {
        if self.value.is_empty() {
            self.forms.to_string()
        } else {
            format!("{} {}", self.forms, self.value)
        }
    }

    fn label_width(&self) -> usize {
        self.label().chars().count()
    }
}

const SUBCOMMANDS: &[CliOption] = &[
    CliOption {
        forms: "a, attach",
        value: "[PID]",
        help: &[
            "Attach to a session running in the background. With",
            "no PID, attaches to the only live session, or lists",
            "them if more than one is up.",
        ],
    },
    CliOption {
        forms: "l, logs",
        value: "",
        help: &["Browse stored message history. Opens no connection."],
    },
    CliOption {
        forms: "help",
        value: "",
        help: &["Print this help. Same as --help."],
    },
];

const OPTIONS: &[CliOption] = &[
    CliOption {
        forms: "-d, --detach",
        value: "",
        help: &[
            "Start headless — no terminal, no splash. Servers",
            "connect and scripts load immediately; attach later.",
        ],
    },
    CliOption {
        forms: "-h, --bind",
        value: "<IP>",
        help: &[
            "Send outgoing IRC connections from this local",
            "address, as in irssi. Also accepts --bind=<IP>.",
            "Applies to this run only, never written to",
            "config.toml. A server's own bind_ip wins; the",
            "persistent equivalent is general.default_bind_ip.",
        ],
    },
    CliOption {
        forms: "-v, --version",
        value: "",
        help: &["Print the version and exit."],
    },
    CliOption {
        forms: "--help",
        value: "",
        help: &["Print this help and exit. (-h is the bind flag above.)"],
    },
];

/// Render the `--help` screen. No trailing newline — [`print_help`] adds it.
fn help_text() -> String {
    let app = constants::APP_NAME;
    // Labels are padded to the widest across *both* tables so the two blocks
    // share one description column. `4` is the row indent and `2` the gap
    // between label and description; `desc_col` is where continuation lines and
    // the ENVIRONMENT block have to start to line up with them.
    let width = SUBCOMMANDS
        .iter()
        .chain(OPTIONS)
        .map(CliOption::label_width)
        .max()
        .unwrap_or(0);
    let desc_col = 4 + width + 2;

    let mut lines = vec![
        format!(
            "{app} {} — {}",
            constants::APP_VERSION,
            constants::APP_DESCRIPTION
        ),
        String::new(),
        "USAGE:".to_string(),
        format!("    {app} [OPTIONS]"),
        format!("    {app} <SUBCOMMAND> [ARGS]"),
        String::new(),
        format!("With no subcommand {app} forks: the backend runs headless and this"),
        "terminal is only a view onto it, so Ctrl+Z or /detach leaves the session —".to_string(),
        format!("connections, scrollback and scripts — running. Reattach with `{app} a`."),
    ];

    for (title, rows) in [("SUBCOMMANDS", SUBCOMMANDS), ("OPTIONS", OPTIONS)] {
        lines.push(String::new());
        lines.push(format!("{title}:"));
        for row in rows {
            let label = row.label();
            for (i, text) in row.help.iter().enumerate() {
                if i == 0 {
                    lines.push(format!("    {label:<width$}  {text}"));
                } else {
                    lines.push(format!("{:desc_col$}{text}", ""));
                }
            }
        }
    }

    lines.push(String::new());
    lines.push("ENVIRONMENT:".to_string());
    lines.push(format!(
        "    {:<width$}  Log verbosity, `warn` unless set. Accepts a level or a",
        "RUST_LOG"
    ));
    lines.push(format!(
        "{:desc_col$}per-module filter, e.g. RUST_LOG={app}::irc=debug.",
        ""
    ));

    // Paths come from the constants accessors so the help can never disagree
    // with where the files actually are.
    let files = [
        (constants::config_path(), "Configuration."),
        (
            constants::env_path(),
            "Credentials — never stored in config.toml.",
        ),
        (constants::theme_dir(), "Themes."),
        (
            constants::log_dir().join("messages.db"),
            "Message history, read by `l`.",
        ),
        (log_path(), "Diagnostics — see RUST_LOG above."),
    ];
    let shown: Vec<(String, &str)> = files
        .iter()
        .map(|(path, note)| (path.display().to_string(), *note))
        .collect();
    let file_width = shown
        .iter()
        .map(|(p, _)| p.chars().count())
        .max()
        .unwrap_or(0);
    lines.push(String::new());
    lines.push("FILES:".to_string());
    for (path, note) in &shown {
        lines.push(format!("    {path:<file_width$}  {note}"));
    }

    lines.push(String::new());
    lines.push(format!("Full documentation: {}", constants::APP_URL));
    lines.join("\n")
}

/// Reap a child process if it has already exited (non-blocking).
///
/// Returns `Some(human_status)` when the child is gone — covers both
/// normal exit and signal termination — so the parent's pre-attach wait
/// loop can fail fast with the actual reason instead of timing out 5s
/// later with "No session found for PID X". `is_pid_alive` (kill(0))
/// cannot do this on its own: a child that exited but hasn't been
/// `wait`ed for is a zombie and `kill(0)` reports it as alive.
fn try_reap(child_pid: u32) -> Option<String> {
    let Ok(pid) = libc::pid_t::try_from(child_pid) else {
        return Some("invalid PID".into());
    };
    let mut status: libc::c_int = 0;
    // SAFETY: WNOHANG makes waitpid non-blocking; passing a valid pointer
    // to an i32 is sound. Returns 0 if child still running, pid if reaped,
    // -1 on ECHILD/EINTR (treat as "still around" — be conservative).
    let result = unsafe {
        libc::waitpid(
            pid,
            std::ptr::from_mut::<libc::c_int>(&mut status),
            libc::WNOHANG,
        )
    };
    if result != pid {
        return None;
    }
    if libc::WIFEXITED(status) {
        Some(format!("exit code {}", libc::WEXITSTATUS(status)))
    } else if libc::WIFSIGNALED(status) {
        Some(format!("killed by signal {}", libc::WTERMSIG(status)))
    } else {
        Some("unknown termination".into())
    }
}

async fn wait_for_backend_socket(
    child_pid: u32,
    sock_path: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<tokio::net::UnixStream> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let connect_error = match tokio::net::UnixStream::connect(sock_path).await {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                error
            }
            Err(error) => {
                return Err(eyre!(
                    "Backend (PID {child_pid}) session socket {} rejected the connection: \
                     {error}. See {} for details.",
                    sock_path.display(),
                    log_path().display()
                ));
            }
        };

        if let Some(reason) = try_reap(child_pid) {
            return Err(eyre!(
                "Backend exited during startup ({reason}). See {} for details.",
                log_path().display()
            ));
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(eyre!(
                "Backend (PID {child_pid}) is alive but did not accept a session connection at \
                 {} within {timeout:?}. Last socket error: {connect_error}. See {} for details.",
                sock_path.display(),
                log_path().display()
            ));
        }

        tokio::time::sleep(BACKEND_CONNECT_RETRY.min(remaining)).await;
    }
}

/// Print the help screen.
///
/// Write errors are dropped on purpose. `--help` is routinely piped into `head`
/// or `less`, and when the reader closes early `println!` panics with
/// "failed printing to stdout: Broken pipe" — a far worse answer to a request
/// for usage than a truncated help screen.
fn print_help() {
    use std::io::Write as _;
    let _ = writeln!(std::io::stdout(), "{}", help_text());
}

/// Did the user ask for usage?
///
/// `--help` counts anywhere in argv; `help` only as the first argument, because
/// further right it is a value somebody typed (`--bind help` is a malformed
/// bind, not a request for help).
fn wants_help(args: &[String]) -> bool {
    args.iter().skip(1).any(|a| a == "--help") || args.get(1).map(String::as_str) == Some("help")
}

/// Trailing nudge for a malformed bind flag, for `-h` only: `--bind` says what
/// the user meant, whereas `-h` is the spelling they may have reached for
/// wanting usage.
fn bind_help_hint(arg: &str) -> String {
    if arg == "-h" {
        format!(" — for usage, run `{} --help`", constants::APP_NAME)
    } else {
        String::new()
    }
}

/// Parse `-h <ip>`, `--bind <ip>`, or `--bind=<ip>` out of the CLI
/// argv. Returns `Ok(Some(ip))` if any form is present, `Ok(None)` if
/// none is, and `Err(...)` if `-h` / `--bind` appears without a value.
///
/// Modelled on irssi's `-h <hostname>` flag — the value is a host-wide
/// runtime override for outgoing IRC bind address. Per-server
/// `bind_ip` (config or `/connect -bind=`) still wins; this only fills
/// in the gap when no per-server value is set. The CLI flag
/// deliberately never mutates `config.toml`, so a one-off invocation
/// (`repartee -h 192.0.2.10`) doesn't pollute later sessions.
///
/// We scan argv unconditionally — passing `-h` to a subcommand that
/// doesn't IRC-connect (`attach`, `logs`) is harmless: the override is
/// stored on `App` but never read.
fn parse_bind_override(args: &[String]) -> Result<Option<String>> {
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if let Some(value) = arg.strip_prefix("--bind=") {
            if value.is_empty() {
                return Err(eyre!("--bind= requires a value, e.g. --bind=192.0.2.10"));
            }
            return Ok(Some(value.to_string()));
        }
        if arg == "-h" || arg == "--bind" {
            let hint = bind_help_hint(arg);
            let value = args.get(i + 1).ok_or_else(|| {
                eyre!("{arg} requires an IP address, e.g. {arg} 192.0.2.10{hint}")
            })?;
            if value.starts_with('-') {
                return Err(eyre!(
                    "{arg} requires an IP address, got flag '{value}'{hint}"
                ));
            }
            return Ok(Some(value.clone()));
        }
        i += 1;
    }
    Ok(None)
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Before everything else, including the bind parse below: that parse exits
    // 2 on a malformed `-h`, so `repartee -h --help` would otherwise answer a
    // request for help with a bind-address error.
    if wants_help(&args) {
        print_help();
        return Ok(());
    }

    // Parse the bind override early so we surface a usage error to the
    // user's TTY before forking (the daemon child has stderr redirected
    // to /dev/null and would otherwise eat the message).
    let cli_bind_override = match parse_bind_override(&args) {
        Ok(value) => value,
        Err(e) => {
            eprintln!("{}: {e:#}", constants::APP_NAME);
            std::process::exit(2);
        }
    };

    // Handle --version / -v before any setup (no tokio needed).
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("{} {}", constants::APP_NAME, constants::APP_VERSION);
        return Ok(());
    }

    // Handle attach subcommand: `repartee a [pid]` or `repartee attach [pid]`
    // Runs purely as a shim — no fork needed.
    if args.get(1).map(String::as_str) == Some("a")
        || args.get(1).map(String::as_str) == Some("attach")
    {
        color_eyre::install()?;
        setup_logging();
        let target_pid = args.get(2).and_then(|s| s.parse::<u32>().ok());
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(session::shim::run_shim(target_pid, false));
    }

    // Handle log browser subcommand: `repartee l` or `repartee logs`.
    // Direct mode like `attach` — no fork, no IRC, no socket listener.
    // Pre-fork validation isn't needed here (we never fork) but config
    // parse errors still surface inside `App::new` and reach the user's
    // TTY directly.
    if args.get(1).map(String::as_str) == Some("l")
        || args.get(1).map(String::as_str) == Some("logs")
    {
        color_eyre::install()?;
        setup_logging();
        ui::install_panic_hook();
        constants::ensure_config_dir();
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut app = app::App::new_log_browser()?;
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    app.cached_term_cols = cols;
                    app.cached_term_rows = rows;
                }
                app.terminal = Some(ui::setup_terminal()?);
                let result = app.run().await;
                if let Some(ref mut terminal) = app.terminal {
                    let _ = ui::restore_terminal(terminal);
                }
                result
            });
    }

    // Handle -d / --detach: start headless (no fork, no terminal).
    if args.iter().any(|a| a == "--detach" || a == "-d") {
        color_eyre::install()?;
        setup_logging();
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async move {
                let mut app = app::App::new()?;
                app.cli_bind_override = cli_bind_override;
                app.detached = true;
                let pid = std::process::id();
                let sock_path = session::socket_path(pid);
                eprintln!("Starting detached. PID={pid}");
                eprintln!("Socket: {}", sock_path.display());
                eprintln!("Attach with: {} a", constants::APP_NAME);
                let result = app.run().await;
                app::App::remove_own_socket();
                result
            });
    }

    // --- Normal start: fork before tokio. ---
    // Child becomes the headless backend (IRC, state, socket listener).
    // Parent becomes the shim (bridges terminal ↔ socket).
    // On detach, the parent/shim exits → shell gets prompt back.
    //
    // Validate config + theme on the parent's TTY *before* forking. The
    // child runs with stderr redirected to /dev/null, so any `App::new`
    // failure (e.g. a TOML typo like `autoconnect = fals`) would otherwise
    // disappear into the void and surface as a generic "No session found
    // for PID X" 5 seconds later. Failing fast here puts the actual
    // toml-error line/column on the user's screen.
    constants::ensure_config_dir();
    if let Err(e) =
        config::validate_startup_files(&constants::config_path(), &constants::theme_dir())
    {
        // `{e:#}` formats the eyre chain without color_eyre's source-location
        // footer — the toml parser already prints line/column inside the
        // message, anything more would just clutter the user's terminal.
        eprintln!("{}: {e:#}", constants::APP_NAME);
        std::process::exit(1);
    }

    // Fork BEFORE any tokio runtime or threads exist.
    let fork_result = unsafe { libc::fork() };

    match fork_result {
        -1 => {
            // Fork failed — fall back to direct mode (no detach support).
            color_eyre::install()?;
            setup_logging();
            ui::install_panic_hook();
            let mut app = app::App::new()?;
            app.cli_bind_override = cli_bind_override;
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                app.cached_term_cols = cols;
                app.cached_term_rows = rows;
            }
            app.terminal = Some(ui::setup_terminal()?);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let result = rt.block_on(app.run());
            if let Some(ref mut terminal) = app.terminal {
                let _ = ui::restore_terminal(terminal);
            }
            result
        }
        0 => {
            // Child: headless backend process.
            unsafe {
                libc::setsid();
                let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if devnull >= 0 {
                    libc::dup2(devnull, libc::STDIN_FILENO);
                    libc::dup2(devnull, libc::STDOUT_FILENO);
                    libc::dup2(devnull, libc::STDERR_FILENO);
                    libc::close(devnull);
                }
            }
            color_eyre::install()?;
            setup_logging();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let mut app = app::App::new()?;
                    app.cli_bind_override = cli_bind_override;
                    app.detached = true;
                    let result = app.run().await;
                    app::App::remove_own_socket();
                    result
                })
        }
        child_pid => {
            // Parent: terminal shim connecting to the child's socket.
            // The splash screen runs while the daemon starts up in the background.
            let child_pid = u32::try_from(child_pid)
                .map_err(|_| color_eyre::eyre::eyre!("fork returned invalid PID: {child_pid}"))?;
            color_eyre::install()?;
            setup_logging();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let sock_path = session::socket_path(child_pid);

                    // Show splash animation — the daemon socket typically
                    // appears during this time (splash takes ~1.5-2.5s).
                    session::shim::run_splash(Some(&sock_path)).await?;

                    let stream = wait_for_backend_socket(
                        child_pid,
                        &sock_path,
                        BACKEND_STARTUP_TIMEOUT,
                    )
                    .await?;
                    session::shim::run_connected_shim(child_pid, stream).await
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CliOption, OPTIONS, SUBCOMMANDS, help_text, parse_bind_override,
        wait_for_backend_socket, wants_help,
    };

    async fn delayed_listener(path: std::path::PathBuf, delay: std::time::Duration) {
        tokio::time::sleep(delay).await;
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        let _ = listener.accept().await.unwrap();
    }

    #[tokio::test]
    async fn backend_wait_connects_after_delayed_socket_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delayed.sock");
        let listener = tokio::spawn(delayed_listener(
            path.clone(),
            std::time::Duration::from_millis(75),
        ));

        let stream = wait_for_backend_socket(
            std::process::id(),
            &path,
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();

        drop(stream);
        listener.await.unwrap();
    }

    #[tokio::test]
    async fn backend_wait_retries_a_stale_socket_until_rebound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let listener = tokio::spawn(delayed_listener(
            path.clone(),
            std::time::Duration::from_millis(75),
        ));

        let stream = wait_for_backend_socket(
            std::process::id(),
            &path,
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();

        drop(stream);
        listener.await.unwrap();
    }

    /// Every form as the user would type it, across both tables.
    fn declared_forms() -> Vec<String> {
        SUBCOMMANDS
            .iter()
            .chain(OPTIONS)
            .flat_map(|o| o.forms.split(", "))
            .map(|f| f.trim().to_string())
            .collect()
    }

    #[test]
    fn help_lists_every_declared_form() {
        let help = help_text();
        for form in declared_forms() {
            assert!(
                help.contains(&form),
                "help output is missing the form {form:?}:\n{help}"
            );
        }
    }

    #[test]
    fn help_names_the_binary_and_the_sections() {
        let help = help_text();
        assert!(help.starts_with(crate::constants::APP_NAME));
        for section in [
            "USAGE:",
            "SUBCOMMANDS:",
            "OPTIONS:",
            "ENVIRONMENT:",
            "FILES:",
        ] {
            assert!(help.contains(section), "help output is missing {section}");
        }
        // The version belongs on the first line so `--help` also answers
        // "which build is this?".
        assert!(
            help.lines()
                .next()
                .unwrap()
                .contains(crate::constants::APP_VERSION),
            "version missing from the first help line"
        );
    }

    #[test]
    fn help_descriptions_share_one_column() {
        // Every description line — first or continuation — starts at the same
        // column, so the two tables read as one aligned block.
        let help = help_text();
        let desc_col = 4
            + SUBCOMMANDS
                .iter()
                .chain(OPTIONS)
                .map(CliOption::label_width)
                .max()
                .unwrap()
            + 2;
        let mut checked = 0;
        for row in SUBCOMMANDS.iter().chain(OPTIONS) {
            for line in row.help {
                let rendered = help
                    .lines()
                    .find(|l| l.contains(*line))
                    .unwrap_or_else(|| panic!("description line missing: {line}"));
                // Byte offset doubles as the char column: everything to the
                // left of a description is a label or padding, all ASCII.
                assert_eq!(
                    rendered.find(*line),
                    Some(desc_col),
                    "misaligned description: {rendered:?}"
                );
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    #[test]
    fn help_fits_an_80_column_terminal() {
        // A terminal client's own help must not wrap in the narrowest terminal
        // anyone still uses. FILES lines are exempt: they print real absolute
        // paths, whose length is the user's business rather than ours.
        let home = crate::constants::home_dir().display().to_string();
        for line in help_text().lines() {
            if line.contains(&home) {
                continue;
            }
            assert!(
                line.chars().count() <= 80,
                "help line is {} columns: {line:?}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn help_paths_come_from_the_constants() {
        // A hardcoded `~/.repartee/config.toml` would drift the moment the
        // layout changes; assert the real accessor output is what gets printed.
        let help = help_text();
        assert!(help.contains(&crate::constants::config_path().display().to_string()));
        assert!(help.contains(&crate::constants::env_path().display().to_string()));
    }

    #[test]
    fn help_is_requested_by_long_flag_or_bare_subcommand() {
        assert!(wants_help(&args(&["--help"])));
        assert!(wants_help(&args(&["help"])));
        // Help wins even when appended to a half-typed command line — that is
        // exactly when it gets typed.
        assert!(wants_help(&args(&["-h", "--help"])));
        assert!(wants_help(&args(&["a", "--help"])));
    }

    #[test]
    fn help_is_not_requested_by_anything_else() {
        assert!(!wants_help(&args(&[])));
        assert!(!wants_help(&args(&["-h", "192.0.2.10"])));
        assert!(!wants_help(&args(&["-d"])));
        // `help` is a subcommand, so only in first position. Further right it
        // is somebody's value, and swallowing it would silently ignore a bad
        // bind.
        assert!(!wants_help(&args(&["--bind", "help"])));
        // argv[0] is a path, not a request.
        assert!(!wants_help(&["--help".to_string()]));
    }

    /// Pull every argv literal `main` compares against out of this file's own
    /// source: `== "-x"` and `== Some("sub")`. Scans only the code above
    /// `#[cfg(test)]`, which both excludes the tests and keeps this scanner's
    /// own needles from matching themselves.
    fn dispatched_literals(src: &str) -> Vec<&str> {
        // Split on the test module header specifically, not on any `cfg(test)`
        // attribute: a test-only helper added above `main` would otherwise
        // truncate the scan and quietly stop covering the dispatch below it.
        // `expect` rather than a fallback — if the marker moves, this test has
        // to fail loudly instead of degrading into a weaker check.
        let (code, _) = src
            .split_once("#[cfg(test)]\nmod tests")
            .expect("test module header moved — update the scanner's split marker");
        let mut found = Vec::new();
        for (idx, _) in code.match_indices("== ") {
            let rest = &code[idx + 3..];
            // The two dispatch shapes are distinguished so an unrelated string
            // comparison elsewhere in this file cannot demand a help row: a
            // bare `== "…"` counts only when it looks like a flag, while
            // `== Some("…")` is only ever how a subcommand is matched here.
            let (body, flag_only) = if let Some(body) = rest.strip_prefix('"') {
                (body, true)
            } else if let Some(body) = rest.strip_prefix("Some(\"") {
                (body, false)
            } else {
                continue;
            };
            let Some(end) = body.find('"') else { continue };
            let literal = &body[..end];
            if literal.is_empty() || literal.contains(char::is_whitespace) {
                continue;
            }
            if flag_only && !literal.starts_with('-') {
                continue;
            }
            found.push(literal);
        }
        found
    }

    #[test]
    fn every_dispatched_argv_literal_has_a_help_row() {
        let literals = dispatched_literals(include_str!("main.rs"));
        // Guard the scanner itself: if it silently stops matching, the
        // assertion below passes vacuously.
        assert!(
            literals.len() >= 8,
            "scanner found only {} literals — it has stopped working: {literals:?}",
            literals.len()
        );
        let declared = declared_forms();
        for literal in literals {
            assert!(
                declared.iter().any(|f| f == literal),
                "main() dispatches on {literal:?} but no CliOption declares it — \
                 add a row so --help stays complete"
            );
        }
    }

    fn args(xs: &[&str]) -> Vec<String> {
        std::iter::once("repartee")
            .chain(xs.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn no_flag_returns_none() {
        assert_eq!(parse_bind_override(&args(&[])).unwrap(), None);
        assert_eq!(parse_bind_override(&args(&["-d"])).unwrap(), None);
        assert_eq!(parse_bind_override(&args(&["a", "1234"])).unwrap(), None);
        // Guards the case where `--help` reaches this parser anyway: it must
        // not be mistaken for a bind flag or a bind value.
        assert_eq!(parse_bind_override(&args(&["--help"])).unwrap(), None);
    }

    #[test]
    fn short_flag_with_value() {
        assert_eq!(
            parse_bind_override(&args(&["-h", "192.0.2.10"])).unwrap(),
            Some("192.0.2.10".into())
        );
    }

    #[test]
    fn long_flag_separate() {
        assert_eq!(
            parse_bind_override(&args(&["--bind", "2001:db8::1"])).unwrap(),
            Some("2001:db8::1".into())
        );
    }

    #[test]
    fn long_flag_equals() {
        assert_eq!(
            parse_bind_override(&args(&["--bind=10.0.0.5"])).unwrap(),
            Some("10.0.0.5".into())
        );
    }

    #[test]
    fn combined_with_other_flags() {
        assert_eq!(
            parse_bind_override(&args(&["-d", "-h", "192.0.2.10"])).unwrap(),
            Some("192.0.2.10".into())
        );
        assert_eq!(
            parse_bind_override(&args(&["--detach", "--bind=192.0.2.10"])).unwrap(),
            Some("192.0.2.10".into())
        );
    }

    #[test]
    fn missing_value_errors() {
        assert!(parse_bind_override(&args(&["-h"])).is_err());
        assert!(parse_bind_override(&args(&["--bind"])).is_err());
        assert!(parse_bind_override(&args(&["--bind="])).is_err());
    }

    #[test]
    fn flag_value_rejected() {
        // -h followed by another flag is a missing-value error, not
        // a "bind to literal -d" mistake.
        assert!(parse_bind_override(&args(&["-h", "-d"])).is_err());
        assert!(parse_bind_override(&args(&["--bind", "--detach"])).is_err());
    }

    #[test]
    fn bare_dash_h_points_at_help() {
        // `-h` is the bind flag, but it is also what most people type when they
        // want usage. The error has to name the flag that gives it.
        let err = format!("{:#}", parse_bind_override(&args(&["-h"])).unwrap_err());
        assert!(err.contains("--help"), "unhelpful -h error: {err}");
        let err = format!(
            "{:#}",
            parse_bind_override(&args(&["-h", "-d"])).unwrap_err()
        );
        assert!(err.contains("--help"), "unhelpful -h error: {err}");
        // --bind is unambiguous — the user meant bind, so no help nudge.
        let err = format!("{:#}", parse_bind_override(&args(&["--bind"])).unwrap_err());
        assert!(!err.contains("--help"), "noisy --bind error: {err}");
    }

    #[test]
    fn first_occurrence_wins() {
        // Doesn't really matter, but documents the behavior: if a user
        // passes two binds, the first one is used.
        assert_eq!(
            parse_bind_override(&args(&["-h", "1.1.1.1", "--bind=2.2.2.2"])).unwrap(),
            Some("1.1.1.1".into())
        );
    }
}
