pub mod api;
pub mod engine;
pub mod event_bus;
pub mod lua;

/// Actions that scripts request via API callbacks.
///
/// Script callbacks cannot directly mutate `App` because they run inside
/// `Arc<dyn Fn>` closures. Instead they send `ScriptAction` values through
/// an mpsc channel, and the App event loop drains and executes them.
#[derive(Debug)]
pub enum ScriptAction {
    Say {
        target: String,
        text: String,
        conn_id: Option<String>,
    },
    Action {
        target: String,
        text: String,
        conn_id: Option<String>,
    },
    Notice {
        target: String,
        text: String,
        conn_id: Option<String>,
    },
    Raw {
        line: String,
        conn_id: Option<String>,
    },
    Join {
        channel: String,
        key: Option<String>,
        conn_id: Option<String>,
    },
    Part {
        channel: String,
        msg: Option<String>,
        conn_id: Option<String>,
    },
    ChangeNick {
        nick: String,
        conn_id: Option<String>,
    },
    Whois {
        nick: String,
        conn_id: Option<String>,
    },
    Mode {
        channel: String,
        mode_string: String,
        conn_id: Option<String>,
    },
    Kick {
        channel: String,
        nick: String,
        reason: Option<String>,
        conn_id: Option<String>,
    },
    Ctcp {
        target: String,
        ctcp_type: String,
        message: Option<String>,
        conn_id: Option<String>,
    },
    LocalEvent {
        text: String,
    },
    BufferEvent {
        buffer_id: String,
        text: String,
    },
    SwitchBuffer {
        buffer_id: String,
    },
    ExecuteCommand {
        line: String,
    },
    RegisterCommand {
        name: String,
        description: String,
        usage: String,
    },
    UnregisterCommand {
        name: String,
    },
    Log {
        script: String,
        message: String,
    },
    StartTimer {
        id: u64,
        interval_ms: u64,
    },
    StartTimeout {
        id: u64,
        delay_ms: u64,
    },
    CancelTimer {
        id: u64,
    },
    TimerFired {
        id: u64,
    },
}
