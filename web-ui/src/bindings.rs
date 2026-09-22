use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{HtmlTextAreaElement, KeyboardEvent};

use crate::keybindings::{Binding, Dispatch, KeyboardConfig, Keymap, Sequence};
use crate::protocol::WebCommand;
use crate::state::AppState;

struct PendingKey {
    event: KeyboardEvent,
    tokens: Vec<String>,
}

struct Runtime {
    config: KeyboardConfig,
    map: Keymap,
    sequence: Sequence,
    events: VecDeque<PendingKey>,
    buffer: Option<String>,
    shell: bool,
    timeout: Option<leptos::leptos_dom::helpers::TimeoutHandle>,
}

fn tokens(event: &KeyboardEvent) -> Vec<String> {
    let key = event.key();
    let token = match key.as_str() {
        "Escape" => "^[".into(),
        "Enter" => "^M".into(),
        "Tab" if event.shift_key() => "stab".into(),
        "Tab" => "^I".into(),
        "ArrowLeft" => "left".into(),
        "ArrowRight" => "right".into(),
        "ArrowUp" => "up".into(),
        "ArrowDown" => "down".into(),
        "Home" => "home".into(),
        "End" => "end".into(),
        "PageUp" => "prior".into(),
        "PageDown" => "next".into(),
        "Backspace" => "backspace".into(),
        "Delete" => "delete".into(),
        _ if key.chars().count() == 1 && event.ctrl_key() => {
            format!("^{}", key.to_ascii_uppercase())
        }
        _ if key.chars().count() == 1 => key,
        _ if key.starts_with('F') && key[1..].parse::<u8>().is_ok() => key.to_ascii_lowercase(),
        _ => return Vec::new(),
    };
    let mut result = crate::keybindings::terminal_tokens(&token);
    if event.alt_key() {
        result.insert(0, "^[".into());
    }
    result
}

fn composer() -> Option<HtmlTextAreaElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id("chat-input")?
        .dyn_into()
        .ok()
}

fn modal(state: AppState) -> bool {
    state.settings_open.get_untracked()
        || state.wizard_open.get_untracked()
        || state.appearance_open.get_untracked()
        || state.emote_picker_open.get_untracked()
        || state.emoji_picker_open.get_untracked()
        || state.manual_preview.get_untracked().is_some()
}

fn edit(action: &str, data: &str) {
    let Some(input) = composer() else { return };
    let mut text: Vec<char> = input.value().chars().collect();
    let units = input.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end_units = input.selection_end().ok().flatten().unwrap_or(0) as usize;
    let position = |units| {
        let mut count = 0;
        text.iter()
            .take_while(|ch| {
                count += ch.len_utf16();
                count <= units
            })
            .count()
    };
    let mut start = position(units);
    let mut end = position(end_units);
    match action {
        "backward_character" => {
            start = if start == end {
                start.saturating_sub(1)
            } else {
                start
            };
            end = start;
        }
        "forward_character" => {
            end = if start == end {
                (end + 1).min(text.len())
            } else {
                end
            };
            start = end;
        }
        "beginning_of_line" => {
            start = 0;
            end = 0;
        }
        "end_of_line" => {
            start = text.len();
            end = start;
        }
        "backward_word" => {
            while start > 0 && text[start - 1].is_whitespace() {
                start -= 1;
            }
            while start > 0 && !text[start - 1].is_whitespace() {
                start -= 1;
            }
            end = start;
        }
        "forward_word" => {
            while end < text.len() && !text[end].is_whitespace() {
                end += 1;
            }
            while end < text.len() && text[end].is_whitespace() {
                end += 1;
            }
            start = end;
        }
        _ => {
            match action {
                "backspace" if start == end => start = start.saturating_sub(1),
                "delete_character" if start == end => end = (end + 1).min(text.len()),
                "erase_line" => {
                    start = 0;
                    end = text.len();
                }
                "erase_to_beg_of_line" => start = 0,
                "erase_to_end_of_line" => end = text.len(),
                "delete_previous_word" => {
                    while start > 0 && text[start - 1].is_whitespace() {
                        start -= 1;
                    }
                    while start > 0 && !text[start - 1].is_whitespace() {
                        start -= 1;
                    }
                }
                _ => {}
            }
            let insert = if action == "insert_text" { data } else { "" };
            text.splice(start..end, insert.chars());
            start += insert.chars().count();
            end = start;
            input.set_value(&text.iter().collect::<String>());
            if let Ok(event) = web_sys::Event::new("input") {
                let _ = input.dispatch_event(&event);
            }
        }
    }
    let utf16 = |index| {
        u32::try_from(text[..index].iter().map(|ch| ch.len_utf16()).sum::<usize>())
            .unwrap_or(u32::MAX)
    };
    let _ = input.set_selection_range(utf16(start), utf16(end));
}

fn synthetic(key: &str, bypass: &Cell<bool>) {
    let Some(input) = composer() else { return };
    let init = web_sys::KeyboardEventInit::new();
    init.set_key(key);
    init.set_bubbles(true);
    init.set_cancelable(true);
    if let Ok(event) = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init) {
        bypass.set(true);
        let _ = input.dispatch_event(&event);
        bypass.set(false);
    }
}

fn execute(state: AppState, binding: &Binding, bypass: &Cell<bool>) {
    let buffers = state.buffers.get_untracked();
    let current = state.active_buffer.get_untracked();
    let selected = match binding.action.as_str() {
        "change_window" => binding.data.parse::<u32>().ok().and_then(|number| {
            crate::state::numbered_buffers(&buffers)
                .find(|(n, _)| *n == number)
                .map(|(_, b)| b.id.clone())
        }),
        "previous_window" | "next_window" => current
            .as_ref()
            .and_then(|id| buffers.iter().position(|b| &b.id == id))
            .map(|index| {
                let step = if binding.action == "next_window" {
                    1
                } else {
                    buffers.len() - 1
                };
                buffers[(index + step) % buffers.len()].id.clone()
            }),
        "active_window" => buffers
            .iter()
            .enumerate()
            .filter(|(_, b)| b.activity > 0 && Some(&b.id) != current.as_ref())
            .min_by_key(|(index, b)| {
                (
                    std::cmp::Reverse(b.activity),
                    b.activity_order.unwrap_or(u64::MAX),
                    *index,
                )
            })
            .map(|(_, b)| b.id.clone()),
        _ => None,
    };
    if let Some(id) = selected {
        state.shell_screen.set(None);
        state.active_buffer.set(Some(id.clone()));
        crate::ws::send_command(&WebCommand::SwitchBufferLocal { buffer_id: id });
        return;
    }
    match binding.action.as_str() {
        "command" => {
            if let Some(buffer_id) = current {
                let text = format!("/{}", binding.data.trim_start_matches('/'));
                if !crate::components::settings::handle_wizard_command(state, &text)
                    && !crate::components::emote_picker::handle_emote_command(state, &text)
                {
                    crate::ws::send_command(&WebCommand::RunCommand { buffer_id, text });
                }
            }
        }
        "multi" => {
            for part in binding.data.split(';') {
                let part = part.trim_start();
                let (action, data) = part.split_once(' ').unwrap_or((part, ""));
                execute(state, &Binding::new(action, data), bypass);
            }
        }
        "send_line" => synthetic("Enter", bypass),
        "word_completion" => synthetic("Tab", bypass),
        "backward_history" => synthetic("ArrowUp", bypass),
        "forward_history" => synthetic("ArrowDown", bypass),
        "nothing" | "key" | "change_window" | "active_window" | "previous_window"
        | "next_window" | "refresh_screen" => {}
        "scroll_backward" | "scroll_forward" | "scroll_start" | "scroll_end" => {
            if let Some(element) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.query_selector(".chat-messages").ok().flatten())
            {
                state
                    .scroll_mode
                    .set(crate::state::ScrollMode::ReadingHistory);
                let top = match binding.action.as_str() {
                    "scroll_start" => 0,
                    "scroll_end" => element.scroll_height(),
                    "scroll_backward" => {
                        element.scroll_top().saturating_sub(element.client_height())
                    }
                    _ => element.scroll_top().saturating_add(element.client_height()),
                };
                element.set_scroll_top(top);
            }
        }
        action => edit(action, &binding.data),
    }
}

fn replay(
    state: AppState,
    pending: &PendingKey,
    shell: bool,
    buffer: &Option<String>,
    bypass: &Cell<bool>,
) {
    if shell {
        if let Some(buffer_id) = buffer {
            let bytes = crate::components::shell_view::key_event_to_bytes(&pending.event);
            crate::ws::send_command(&WebCommand::ShellInput {
                buffer_id: buffer_id.clone(),
                data: crate::components::shell_view::base64_encode(&bytes),
            });
        }
        return;
    }
    let event = &pending.event;
    match event.key().as_str() {
        "ArrowLeft" => edit("backward_character", ""),
        "ArrowRight" => edit("forward_character", ""),
        "Home" => edit("beginning_of_line", ""),
        "End" => edit("end_of_line", ""),
        "Backspace" => edit("backspace", ""),
        "Delete" => edit("delete_character", ""),
        "Enter" if event.alt_key() || event.shift_key() => edit("insert_text", "\n"),
        "Escape" | "Enter" | "Tab" | "ArrowUp" | "ArrowDown" => synthetic(&event.key(), bypass),
        key if key.chars().count() == 1
            && !event.ctrl_key()
            && !event.alt_key()
            && !event.meta_key() =>
        {
            edit("insert_text", key)
        }
        _ => {}
    }
    let _ = state;
}

fn dispatch(
    state: AppState,
    runtime: &Rc<RefCell<Runtime>>,
    bypass: &Cell<bool>,
    output: Vec<Dispatch>,
) {
    let mut output = VecDeque::from(output);
    while let Some(item) = output.pop_front() {
        match item {
            Dispatch::Action(binding) => {
                runtime.borrow_mut().events.clear();
                execute(state, &binding, bypass);
            }
            Dispatch::Replay(_) => {
                let (pending, shell, buffer) = {
                    let mut runtime = runtime.borrow_mut();
                    (
                        runtime.events.pop_front(),
                        runtime.shell,
                        runtime.buffer.clone(),
                    )
                };
                if let Some(pending) = pending {
                    for _ in 1..pending.tokens.len() {
                        output.pop_front();
                    }
                    replay(state, &pending, shell, &buffer, bypass);
                }
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn now() -> u64 {
    js_sys::Date::now().max(0.0) as u64
}

fn cancel(state: AppState, runtime: &Rc<RefCell<Runtime>>, bypass: &Cell<bool>) {
    let (events, shell, buffer) = {
        let mut runtime = runtime.borrow_mut();
        if let Some(timeout) = runtime.timeout.take() {
            timeout.clear();
        }
        runtime.sequence.clear();
        (
            std::mem::take(&mut runtime.events),
            runtime.shell,
            runtime.buffer.clone(),
        )
    };
    for event in events {
        if shell
            || (event.event.key().chars().count() == 1
                && !event.event.ctrl_key()
                && !event.event.alt_key()
                && !event.event.meta_key())
        {
            replay(state, &event, shell, &buffer, bypass);
        }
    }
}

fn arm_timeout(state: AppState, runtime: Rc<RefCell<Runtime>>, bypass: Rc<Cell<bool>>) {
    let deadline = {
        let mut runtime = runtime.borrow_mut();
        if let Some(timeout) = runtime.timeout.take() {
            timeout.clear();
        }
        runtime.sequence.deadline(&runtime.map)
    };
    if let Some(deadline) = deadline {
        let timer_runtime = runtime.clone();
        let handle = leptos::leptos_dom::helpers::set_timeout_with_handle(
            move || {
                let output = {
                    let mut runtime = timer_runtime.borrow_mut();
                    runtime.timeout = None;
                    let Runtime { sequence, map, .. } = &mut *runtime;
                    sequence.expire(map, now())
                };
                dispatch(state, &timer_runtime, &bypass, output);
                arm_timeout(state, timer_runtime, bypass);
            },
            std::time::Duration::from_millis(deadline.saturating_sub(now())),
        )
        .ok();
        runtime.borrow_mut().timeout = handle;
    }
}

fn synchronize(state: AppState, runtime: &Rc<RefCell<Runtime>>, bypass: &Cell<bool>) {
    let config = state.keyboard.get_untracked();
    let buffer = state.active_buffer.get_untracked();
    let shell = state
        .buffers
        .get_untracked()
        .iter()
        .any(|b| Some(&b.id) == buffer.as_ref() && b.buffer_type == "shell");
    let changed = {
        let runtime = runtime.borrow();
        runtime.config != config
            || runtime.buffer != buffer
            || runtime.shell != shell
            || !state.connected.get_untracked()
            || modal(state)
    };
    if changed {
        cancel(state, runtime, bypass);
        if let Ok(map) = config.compile() {
            let mut runtime = runtime.borrow_mut();
            runtime.map = if shell { map.navigation_only() } else { map };
            runtime.config = config;
            runtime.buffer = buffer;
            runtime.shell = shell;
        }
    }
}

pub fn install(state: AppState) {
    let config = state.keyboard.get_untracked();
    let runtime = Rc::new(RefCell::new(Runtime {
        map: config.compile().unwrap_or_else(|_| {
            KeyboardConfig::default()
                .compile()
                .expect("default bindings")
        }),
        config,
        sequence: Sequence::default(),
        events: VecDeque::new(),
        buffer: state.active_buffer.get_untracked(),
        shell: false,
        timeout: None,
    }));
    let bypass = Rc::new(Cell::new(false));
    let effect_runtime = runtime.clone();
    let effect_bypass = bypass.clone();
    Effect::new(move || {
        state.keyboard.track();
        state.active_buffer.track();
        state.connected.track();
        state.buffers.track();
        state.settings_open.track();
        state.wizard_open.track();
        state.appearance_open.track();
        state.emote_picker_open.track();
        state.emoji_picker_open.track();
        state.manual_preview.track();
        synchronize(state, &effect_runtime, &effect_bypass);
    });
    let key_runtime = runtime.clone();
    let key_bypass = bypass.clone();
    let keydown = Closure::<dyn Fn(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if key_bypass.get() {
            return;
        }
        synchronize(state, &key_runtime, &key_bypass);
        let other_input = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|target| {
                target.id() != "chat-input"
                    && (matches!(target.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                        || target.get_attribute("contenteditable").is_some())
            });
        if event.is_composing()
            || event.meta_key()
            || other_input
            || modal(state)
            || !state.connected.get_untracked()
        {
            cancel(state, &key_runtime, &key_bypass);
            return;
        }
        let incoming = tokens(&event);
        if incoming.is_empty() {
            return;
        }
        let previous_context = {
            let runtime = key_runtime.borrow();
            (runtime.buffer.clone(), runtime.shell)
        };
        let expired = {
            let mut runtime = key_runtime.borrow_mut();
            let Runtime { sequence, map, .. } = &mut *runtime;
            sequence.expire(map, now())
        };
        dispatch(state, &key_runtime, &key_bypass, expired);
        synchronize(state, &key_runtime, &key_bypass);
        let context_changed = {
            let runtime = key_runtime.borrow();
            previous_context != (runtime.buffer.clone(), runtime.shell)
        };
        if key_runtime.borrow().shell && event.key() == "Escape" {
            cancel(state, &key_runtime, &key_bypass);
            if context_changed {
                event.prevent_default();
                event.stop_immediate_propagation();
                let buffer = key_runtime.borrow().buffer.clone();
                replay(
                    state,
                    &PendingKey {
                        event,
                        tokens: incoming,
                    },
                    true,
                    &buffer,
                    &key_bypass,
                );
            }
            return;
        }
        if modal(state) {
            event.prevent_default();
            event.stop_immediate_propagation();
            return;
        }
        let output = {
            let mut runtime = key_runtime.borrow_mut();
            runtime.events.push_back(PendingKey {
                event: event.clone(),
                tokens: incoming.clone(),
            });
            let Runtime { sequence, map, .. } = &mut *runtime;
            sequence.feed_event(map, incoming.clone(), now())
        };
        let replay: Vec<_> = incoming.into_iter().map(Dispatch::Replay).collect();
        let mut output = output;
        let passthrough = !context_changed && output.ends_with(&replay);
        if passthrough {
            output.truncate(output.len() - replay.len());
            key_runtime.borrow_mut().events.pop_back();
        } else {
            event.prevent_default();
            event.stop_immediate_propagation();
        }
        dispatch(state, &key_runtime, &key_bypass, output);
        arm_timeout(state, key_runtime.clone(), key_bypass.clone());
    });
    let cancel_runtime = runtime.clone();
    let cancel_bypass = bypass.clone();
    let cancel_input = Closure::<dyn Fn(web_sys::Event)>::new(move |_| {
        cancel(state, &cancel_runtime, &cancel_bypass)
    });
    if let Some(window) = web_sys::window() {
        let _ = window.add_event_listener_with_callback_and_bool(
            "keydown",
            keydown.as_ref().unchecked_ref(),
            true,
        );
        for name in ["paste", "compositionstart", "blur"] {
            let _ = window.add_event_listener_with_callback_and_bool(
                name,
                cancel_input.as_ref().unchecked_ref(),
                true,
            );
        }
    }
    let handles = StoredValue::new_local((runtime, keydown, cancel_input));
    on_cleanup(move || {
        handles.with_value(|(runtime, keydown, cancel_input)| {
            if let Some(timeout) = runtime.borrow_mut().timeout.take() {
                timeout.clear();
            }
            if let Some(window) = web_sys::window() {
                let _ = window.remove_event_listener_with_callback_and_bool(
                    "keydown",
                    keydown.as_ref().unchecked_ref(),
                    true,
                );
                for name in ["paste", "compositionstart", "blur"] {
                    let _ = window.remove_event_listener_with_callback_and_bool(
                        name,
                        cancel_input.as_ref().unchecked_ref(),
                        true,
                    );
                }
            }
        })
    });
}
