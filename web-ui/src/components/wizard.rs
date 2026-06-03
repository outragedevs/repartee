//! Add-server wizard modal (web). Mirrors the TUI `/wizard server` form: a
//! centered modal with Basics/Advanced tabs, mouse-first, emitting a structured
//! [`WebCommand::SaveServer`]. Add-only — the web client has no full server
//! config to pre-fill an edit (edit lives in the TUI).

use leptos::prelude::*;

use crate::protocol::{SaveServerCmd, WebCommand};
use crate::state::AppState;

#[component]
pub fn ServerWizard() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let open = state.wizard_open;

    // Local field signals.
    let network = RwSignal::new(String::new());
    let address = RwSignal::new(String::new());
    let port = RwSignal::new(String::new());
    let tls = RwSignal::new(false);
    let tls_verify = RwSignal::new(true);
    let bind_ip = RwSignal::new(String::new());
    let nick = RwSignal::new(String::new());
    let username = RwSignal::new(String::new());
    let realname = RwSignal::new(String::new());
    let channels = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let sasl_user = RwSignal::new(String::new());
    let sasl_pass = RwSignal::new(String::new());
    let sasl_mechanism = RwSignal::new("Auto".to_string());
    let encoding = RwSignal::new(String::new());
    let autoconnect = RwSignal::new(true);
    let auto_reconnect = RwSignal::new(true);
    let reconnect_delay = RwSignal::new(String::new());
    let reconnect_max_retries = RwSignal::new(String::new());
    let autosendcmd = RwSignal::new(String::new());
    let client_cert_path = RwSignal::new(String::new());
    let page = RwSignal::new(0u8);
    let error = RwSignal::new(Option::<String>::None);

    // Reset every field whenever the modal (re)opens.
    Effect::new(move |_| {
        if open.get() {
            network.set(String::new());
            address.set(String::new());
            port.set(String::new());
            tls.set(false);
            tls_verify.set(true);
            bind_ip.set(String::new());
            nick.set(String::new());
            username.set(String::new());
            realname.set(String::new());
            channels.set(String::new());
            password.set(String::new());
            sasl_user.set(String::new());
            sasl_pass.set(String::new());
            sasl_mechanism.set("Auto".to_string());
            encoding.set(String::new());
            autoconnect.set(true);
            auto_reconnect.set(true);
            reconnect_delay.set(String::new());
            reconnect_max_retries.set(String::new());
            autosendcmd.set(String::new());
            client_cert_path.set(String::new());
            page.set(0);
            error.set(None);
        }
    });

    let on_save = move |_| {
        if network.get().trim().is_empty() {
            error.set(Some("Network Name is required".into()));
            page.set(0);
            return;
        }
        if address.get().trim().is_empty() {
            error.set(Some("Server address is required".into()));
            page.set(0);
            return;
        }
        let cmd = SaveServerCmd {
            id: None, // add-only
            network: network.get(),
            address: address.get(),
            port: port.get().trim().parse::<u16>().ok(),
            tls: tls.get(),
            tls_verify: tls_verify.get(),
            autoconnect: autoconnect.get(),
            channels: channels.get(),
            nick: nick.get(),
            username: username.get(),
            realname: realname.get(),
            bind_ip: bind_ip.get(),
            encoding: encoding.get(),
            sasl_user: sasl_user.get(),
            sasl_mechanism: sasl_mechanism.get(),
            autosendcmd: autosendcmd.get(),
            client_cert_path: client_cert_path.get(),
            auto_reconnect: auto_reconnect.get(),
            reconnect_delay: reconnect_delay.get(),
            reconnect_max_retries: reconnect_max_retries.get(),
            // Empty masked field = leave unset (no explicit-clear control on web).
            password: opt(password.get()),
            sasl_pass: opt(sasl_pass.get()),
        };
        crate::ws::send_command(&WebCommand::SaveServer(Box::new(cmd)));
        open.set(false);
    };

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="wizard-backdrop" on:click=move |_| open.set(false)></div>
            <div class="wizard-modal">
                <div class="wizard-head">
                    <h3>"Add Server"</h3>
                    <span class="wizard-x" on:click=move |_| open.set(false)>"\u{2715}"</span>
                </div>
                <div class="wizard-tabs">
                    <button
                        class=move || if page.get() == 0 { "wizard-tab active" } else { "wizard-tab" }
                        on:click=move |_| page.set(0)
                    >"Basics"</button>
                    <button
                        class=move || if page.get() == 1 { "wizard-tab active" } else { "wizard-tab" }
                        on:click=move |_| page.set(1)
                    >"Advanced"</button>
                </div>
                <div class="wizard-body">
                    <Show when=move || page.get() == 0 fallback=|| ()>
                        {text_row("Network Name", network)}
                        {text_row("Server address / IP", address)}
                        {text_row("Port", port)}
                        {check_row("Use TLS/SSL", tls)}
                        {check_row("Verify TLS certificate", tls_verify)}
                        {text_row("Bind IP", bind_ip)}
                    </Show>
                    <Show when=move || page.get() == 1 fallback=|| ()>
                        {text_row("Nick", nick)}
                        {text_row("Username", username)}
                        {text_row("Realname", realname)}
                        {text_row("Channels (comma-separated)", channels)}
                        {pass_row("Server password", password)}
                        {text_row("SASL user", sasl_user)}
                        {pass_row("SASL pass", sasl_pass)}
                        {select_row("SASL mechanism", sasl_mechanism)}
                        {text_row("Encoding", encoding)}
                        {check_row("Autoconnect", autoconnect)}
                        {check_row("Auto-reconnect", auto_reconnect)}
                        {text_row("Reconnect delay (s)", reconnect_delay)}
                        {text_row("Reconnect max retries", reconnect_max_retries)}
                        {text_row("Autosendcmd", autosendcmd)}
                        {text_row("Client cert path", client_cert_path)}
                    </Show>
                </div>
                {move || error.get().map(|e| view! { <p class="wizard-error">{e}</p> })}
                <div class="wizard-foot">
                    <button class="wizard-btn s" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button class="wizard-btn p" on:click=on_save>"Save"</button>
                </div>
            </div>
        </Show>
    }
}

/// Trim and convert empty → `None` (so an untouched credential is left unset).
fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

fn text_row(label: &'static str, sig: RwSignal<String>) -> impl IntoView {
    view! {
        <div class="wizard-row">
            <label>{label}</label>
            <input
                prop:value=move || sig.get()
                on:input=move |ev| sig.set(event_target_value(&ev))
            />
        </div>
    }
}

fn pass_row(label: &'static str, sig: RwSignal<String>) -> impl IntoView {
    view! {
        <div class="wizard-row">
            <label>{label}</label>
            <input
                type="password"
                prop:value=move || sig.get()
                on:input=move |ev| sig.set(event_target_value(&ev))
            />
        </div>
    }
}

fn check_row(label: &'static str, sig: RwSignal<bool>) -> impl IntoView {
    view! {
        <label class="wizard-check">
            <input
                type="checkbox"
                prop:checked=move || sig.get()
                on:change=move |ev| sig.set(event_target_checked(&ev))
            />
            {label}
        </label>
    }
}

fn select_row(label: &'static str, sig: RwSignal<String>) -> impl IntoView {
    let opts = ["Auto", "PLAIN", "EXTERNAL"];
    view! {
        <div class="wizard-row">
            <label>{label}</label>
            <select on:change=move |ev| sig.set(event_target_value(&ev))>
                {opts.iter().map(|o| {
                    let o = (*o).to_string();
                    let o_sel = o.clone();
                    let o_text = o.clone();
                    view! {
                        <option value=o selected=move || sig.get() == o_sel>{o_text}</option>
                    }
                }).collect::<Vec<_>>()}
            </select>
        </div>
    }
}
