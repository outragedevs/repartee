use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn NickList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let nicks = move || {
        let active_id = state.active_buffer.get()?;
        let lists = state.nick_lists.get();
        lists.get(&active_id).cloned()
    };

    let grouped = move || {
        let nicks = nicks().unwrap_or_default();
        let mut ops = Vec::new();
        let mut voiced = Vec::new();
        let mut regular = Vec::new();

        for n in &nicks {
            if n.prefix.contains('~') || n.prefix.contains('&') || n.prefix.contains('@') {
                ops.push(n.clone());
            } else if n.prefix.contains('%') || n.prefix.contains('+') {
                voiced.push(n.clone());
            } else {
                regular.push(n.clone());
            }
        }

        (ops, voiced, regular)
    };

    view! {
        <div class="nick-list">
            {move || {
                let (ops, voiced, regular) = grouped();
                let active_buffer_id = state.active_buffer.get_untracked();

                let render_nicks = |nicks: &[crate::protocol::WireNick], prefix_class: &str| {
                    nicks.iter().map(|n| {
                        let away_class = if n.away { " away" } else { "" };
                        let class = format!("nick-entry{away_class}");
                        let pclass = prefix_class.to_string();
                        let prefix = n.prefix.clone();
                        let nick = n.nick.clone();
                        let nick_for_click = n.nick.clone();
                        let buf_id = active_buffer_id.clone();
                        let on_click = move |_| {
                            if let Some(ref buffer_id) = buf_id {
                                crate::ws::send_command(&WebCommand::RunCommand {
                                    buffer_id: buffer_id.clone(),
                                    text: format!("/query {}", nick_for_click),
                                });
                            }
                        };
                        view! {
                            <div class=class on:click=on_click>
                                <span class=pclass>{prefix}</span>
                                <span>{nick}</span>
                            </div>
                        }
                    }).collect::<Vec<_>>()
                };

                let ops_len = ops.len();
                let voiced_len = voiced.len();
                let regular_len = regular.len();

                view! {
                    {if !ops.is_empty() {
                        Some(view! {
                            <div class="mode-group">{format!("Ops ({ops_len})")}</div>
                            {render_nicks(&ops, "prefix-op")}
                        })
                    } else { None }}
                    {if !voiced.is_empty() {
                        Some(view! {
                            <div class="mode-group">{format!("Voiced ({voiced_len})")}</div>
                            {render_nicks(&voiced, "prefix-voice")}
                        })
                    } else { None }}
                    {if !regular.is_empty() {
                        Some(view! {
                            <div class="mode-group">{format!("Users ({regular_len})")}</div>
                            {render_nicks(&regular, "prefix-normal")}
                        })
                    } else { None }}
                }
            }}
        </div>
    }
}
