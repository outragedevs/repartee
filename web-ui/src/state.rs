use std::collections::{HashMap, HashSet};

use leptos::prelude::*;

use crate::protocol::*;

/// Per-buffer cap on in-memory rendered messages. Older lines are
/// dropped from the head (oldest-first) when the cap is exceeded; the
/// DB still holds full history for scroll-back via `FetchMessages`.
///
/// 1000 strikes the balance: deep enough to cover typical IRC sessions
/// without scroll-back, shallow enough that the DOM stays light. We
/// don't index-virtualize the list — IRC chat lines have wildly
/// variable heights (single line vs. wrapped paragraph vs. 200 px
/// image preview), which makes naive index-based virtualization
/// wrong (scrollbar size, jump-to-position) and proper
/// measurement-based virtualization a substantial undertaking.
const MAX_BUFFER_MESSAGES: usize = 1000;

/// Higher per-buffer cap while the user is scrolled up reading backlog (the
/// buffer is "pinned"). Loaded older pages are kept up to this bound instead of
/// being trimmed back to `MAX_BUFFER_MESSAGES`; returning to the bottom collapses
/// the buffer back down. Mirrors the TUI's `PINNED_BACKLOG_CAP`.
pub(crate) const PINNED_WEB_CAP: usize = 5000;

/// Client-side application state, stored as Leptos signals.
#[derive(Clone, Copy)]
pub struct AppState {
    pub authenticated: RwSignal<bool>,
    pub connected: RwSignal<bool>,
    pub buffers: RwSignal<Vec<BufferMeta>>,
    pub connections: RwSignal<Vec<ConnectionMeta>>,
    pub active_buffer: RwSignal<Option<String>>,
    pub messages: RwSignal<HashMap<String, Vec<WireMessage>>>,
    pub nick_lists: RwSignal<HashMap<String, Vec<WireNick>>>,
    pub nick_lists_loaded: RwSignal<HashSet<String>>,
    pub mention_count: RwSignal<u32>,
    pub session_hint: RwSignal<bool>,
    pub theme: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
    /// Text handed back by the server after a refused send, for the composer
    /// to restore. Cleared by the input component once consumed.
    pub restore_input: RwSignal<Option<String>>,
    pub timestamp_format: RwSignal<String>,
    pub line_height: RwSignal<f32>,
    /// Client-side appearance overrides (persisted in localStorage, applied
    /// as CSS vars by an Effect in `app.rs`). `None` = follow the server
    /// setting / stylesheet default. These exist so a phone user can bump
    /// text size without recompiling or touching the TUI config.
    pub font_size_override: RwSignal<Option<u32>>,
    pub line_height_override: RwSignal<Option<f32>>,
    /// Appearance menu modal open flag.
    pub appearance_open: RwSignal<bool>,
    pub nick_column_width: RwSignal<u32>,
    pub nick_max_length: RwSignal<u32>,
    pub nick_colors_enabled: RwSignal<bool>,
    pub nick_colors_in_nicklist: RwSignal<bool>,
    pub nick_color_saturation: RwSignal<f32>,
    pub nick_color_lightness: RwSignal<f32>,
    /// Shell screen content for the active shell buffer.
    pub shell_screen: RwSignal<Option<crate::protocol::ShellScreenData>>,
    /// Bumped on every SyncInit (initial connect or lag recovery).
    /// Read with `get_untracked()` only — the Layout Effect uses it as
    /// part of its in-flight FetchMessages dedup key, NOT as a reactive
    /// trigger. The reactive trigger is `active_buffer` (Leptos 0.7
    /// `.set()` fires subscribers regardless of value equality).
    pub sync_version: RwSignal<u32>,
    /// Tracks which buffers have had their DB backlog fetched via FetchMessages.
    /// Prevents the Layout Effect from skipping the fetch when only live
    /// NewMessage events are cached (the root cause of empty-buffer-on-switch).
    pub backlog_loaded: RwSignal<HashSet<String>>,
    /// Whether the chat view is scrolled to (or near) the bottom.
    /// When true, new messages auto-scroll. When false, the user is reading
    /// backlog and the view stays put.
    pub is_at_bottom: RwSignal<bool>,
    /// Per-message preview dismissals: `(message_id, link)` pairs. Persisted
    /// to localStorage so a "hide this thumbnail" decision survives reload.
    pub dismissed_previews: RwSignal<HashSet<(u64, String)>>,
    /// Whether `:name:` tokens render as inline emote images (mirrors the
    /// server's `[emotes]` config; pushed via `SettingsChanged`).
    pub emotes_enabled: RwSignal<bool>,
    /// Server wizard modal open flag. The web wizard is add-only (the client has
    /// no full server config to pre-fill an edit), so there is no edit-id here.
    pub wizard_open: RwSignal<bool>,
    /// GG emote (`:name:` GIF) picker modal open flag.
    pub emote_picker_open: RwSignal<bool>,
    /// UTF-8 Unicode emoji picker modal open flag (desktop only).
    pub emoji_picker_open: RwSignal<bool>,
    /// A token to splice into the input at the caret (`:name:` or a Unicode
    /// emoji). The input component consumes it and clears it back to `None`.
    pub pending_insert: RwSignal<Option<String>>,
    /// A nick to mention, set by tapping a nick in the chat log. The input
    /// component consumes it, choosing the delimiter by caret context
    /// (`nick: ` at a line start, `nick ` mid-sentence).
    pub pending_mention: RwSignal<Option<String>>,
    /// Per-buffer: whether the server reported more history is available older
    /// than what's loaded (from the `has_more` field of `Messages`). Drives the
    /// scroll-up loader — `false` (or absent) means stop fetching.
    pub backlog_has_more: RwSignal<HashMap<String, bool>>,
    /// Buffers with an in-flight `FetchMessages`. Guards initial loads and
    /// scroll-back requests against duplicate viewport/resize fetches.
    pub backlog_fetching: RwSignal<HashSet<String>>,
    /// buffer_id -> nicks currently typing (IRCv3 `+typing`).
    pub typing: RwSignal<HashMap<String, Vec<String>>>,
    /// The status line's items, in the server's `statusbar.items` order, under
    /// the names `/items` uses. Seeded by `SyncInit`, updated live by
    /// `StatusbarConfig`. The status line renders from this — it is the whole
    /// reason `/items …` and `statusbar.enabled` take effect in the browser.
    pub statusbar_items: RwSignal<Vec<String>>,
    /// `statusbar.enabled` — `false` renders no status line at all.
    pub statusbar_enabled: RwSignal<bool>,
}

impl AppState {
    pub fn new() -> Self {
        // Load theme from localStorage if available.
        let theme_key = crate::constants::storage_key("theme");
        let saved_theme: String = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s: web_sys::Storage| s.get_item(&theme_key).ok().flatten())
            .unwrap_or_else(|| "nightfall".to_string());

        Self::with_persisted(
            saved_theme,
            // Clamp on load: the Aa menu clamps on write, but a stale or
            // hand-edited stored value (e.g. "999") would otherwise apply
            // raw and could make the UI — including the reset button —
            // unusable.
            load_stored_parsed::<i64>(&crate::constants::storage_key("font-size"))
                .map(crate::components::appearance::clamp_font),
            load_stored_parsed::<f32>(&crate::constants::storage_key("line-height"))
                .map(crate::components::appearance::clamp_line_h),
            load_dismissed_previews(),
        )
    }

    /// The half of [`Self::new`] that touches no browser storage.
    ///
    /// Split out so host-side tests can build a state at all: every
    /// `localStorage` read in `new()` goes through `web_sys::window()`, which
    /// panics outside wasm.
    fn with_persisted(
        saved_theme: String,
        font_size_override: Option<u32>,
        line_height_override: Option<f32>,
        dismissed_previews: HashSet<(u64, String)>,
    ) -> Self {
        Self {
            authenticated: RwSignal::new(false),
            connected: RwSignal::new(false),
            buffers: RwSignal::new(Vec::new()),
            connections: RwSignal::new(Vec::new()),
            active_buffer: RwSignal::new(None),
            messages: RwSignal::new(HashMap::new()),
            nick_lists: RwSignal::new(HashMap::new()),
            nick_lists_loaded: RwSignal::new(HashSet::new()),
            mention_count: RwSignal::new(0),
            session_hint: RwSignal::new(false),
            theme: RwSignal::new(saved_theme),
            error: RwSignal::new(None),
            restore_input: RwSignal::new(None),
            timestamp_format: RwSignal::new("%H:%M".to_string()),
            line_height: RwSignal::new(1.35),
            font_size_override: RwSignal::new(font_size_override),
            line_height_override: RwSignal::new(line_height_override),
            appearance_open: RwSignal::new(false),
            nick_column_width: RwSignal::new(12),
            nick_max_length: RwSignal::new(9),
            nick_colors_enabled: RwSignal::new(true),
            nick_colors_in_nicklist: RwSignal::new(true),
            nick_color_saturation: RwSignal::new(0.65),
            nick_color_lightness: RwSignal::new(0.65),
            shell_screen: RwSignal::new(None),
            sync_version: RwSignal::new(0),
            backlog_loaded: RwSignal::new(HashSet::new()),
            is_at_bottom: RwSignal::new(true),
            dismissed_previews: RwSignal::new(dismissed_previews),
            emotes_enabled: RwSignal::new(true),
            wizard_open: RwSignal::new(false),
            emote_picker_open: RwSignal::new(false),
            emoji_picker_open: RwSignal::new(false),
            pending_insert: RwSignal::new(None),
            pending_mention: RwSignal::new(None),
            backlog_has_more: RwSignal::new(HashMap::new()),
            backlog_fetching: RwSignal::new(HashSet::new()),
            typing: RwSignal::new(HashMap::new()),
            // Empty until SyncInit lands: the server owns this list, and
            // guessing a default here would resurrect the very hardcoding this
            // replaces (a tab would briefly show items the config removed).
            statusbar_items: RwSignal::new(Vec::new()),
            statusbar_enabled: RwSignal::new(true),
        }
    }

    /// Switch the active buffer locally and inform the server — the shared
    /// "user clicked a window" path (buffer list, Act numbers): set the
    /// signal, echo `SwitchBuffer`, and mark the buffer read up to now.
    pub fn switch_to_buffer(&self, buffer_id: &str) {
        self.shell_screen.set(None);
        self.active_buffer.set(Some(buffer_id.to_string()));
        crate::ws::send_command(&WebCommand::SwitchBuffer {
            buffer_id: buffer_id.to_string(),
        });
        crate::ws::send_command(&WebCommand::MarkRead {
            buffer_id: buffer_id.to_string(),
            up_to: chrono::Utc::now().timestamp(),
        });
    }

    /// Collapse a buffer's loaded backlog window after the user returns to the
    /// bottom: trim back to `MAX_BUFFER_MESSAGES` (freeing the older lines we're
    /// no longer displaying) and re-arm `has_more` so a later scroll-up fetches
    /// again. Mirrors the TUI's collapse-on-return-to-bottom.
    pub fn collapse_backlog(&self, buffer_id: &str) {
        let mut trimmed = false;
        self.messages.update(|msgs| {
            if let Some(entry) = msgs.get_mut(buffer_id)
                && entry.len() > MAX_BUFFER_MESSAGES
            {
                let drop = entry.len() - MAX_BUFFER_MESSAGES;
                entry.drain(..drop);
                entry.shrink_to(MAX_BUFFER_MESSAGES);
                trimmed = true;
            }
        });
        if trimmed {
            self.backlog_has_more.update(|m| {
                m.insert(buffer_id.to_string(), true);
            });
        }
    }

    /// Handle a WebEvent from the server, updating signals accordingly.
    pub fn handle_event(&self, event: WebEvent) {
        match event {
            WebEvent::SyncInit {
                buffers,
                connections,
                mention_count,
                active_buffer_id,
                timestamp_format,
                emotes_enabled,
                typing,
                statusbar_items,
                statusbar_enabled,
            } => {
                // Clear cached messages, nick lists, and backlog-loaded flags —
                // forces re-fetch. Handles both initial connect and lag-recovery resync.
                self.messages.set(HashMap::new());
                self.nick_lists.set(HashMap::new());
                self.nick_lists_loaded.set(HashSet::new());
                self.backlog_loaded.set(HashSet::new());
                self.backlog_has_more.set(HashMap::new());
                self.backlog_fetching.set(HashSet::new());
                // REPLACE the typing map with the snapshot's — never blank it.
                // Whatever we were showing is stale across a resync, but the
                // server's set is not: the live `Typing` push only fires when the
                // visible set *changes*, so a refresh of an already-typing nick
                // sends nothing and `paused` is sent once and then sits for 30s.
                // Blanking here left a reconnecting tab with no indicator for up
                // to that long while the TUI showed it the whole time.
                self.typing.set(typing);
                self.statusbar_items.set(statusbar_items);
                self.statusbar_enabled.set(statusbar_enabled);

                self.buffers.set(buffers);
                self.connections.set(connections);
                self.mention_count.set(mention_count);
                self.emotes_enabled.set(emotes_enabled);
                self.authenticated.set(true);
                self.connected.set(true);
                if let Some(fmt) = timestamp_format
                    && !fmt.is_empty()
                {
                    self.timestamp_format.set(fmt);
                }

                self.sort_buffers();

                // Bump sync_version FIRST so the Layout Effect's
                // pending-fetch dedup (keyed by (buffer_id, sync_version))
                // admits a new fetch for the active buffer under the new
                // epoch — even if the buffer ID is the same as before.
                self.sync_version.update(|v| *v += 1);

                let fallback = self.buffers.with_untracked(|buffers| {
                    buffers
                        .iter()
                        .find(|buffer| buffer.buffer_type == "channel")
                        .or_else(|| buffers.first())
                        .map(|buffer| buffer.id.clone())
                });
                let active_buffer = active_buffer_id.or(fallback);
                if active_buffer_changed(
                    self.active_buffer.get_untracked().as_deref(),
                    active_buffer.as_deref(),
                ) {
                    self.shell_screen.set(None);
                }
                self.active_buffer.set(active_buffer);
            }
            WebEvent::NewMessage { buffer_id, message } => {
                // Keep the larger window only for the buffer the user is actively
                // reading backlog in (active + scrolled up); every other buffer
                // trims to the normal cap.
                let active = self.active_buffer.get_untracked();
                let pinned =
                    active.as_deref() == Some(&buffer_id) && !self.is_at_bottom.get_untracked();
                let cap = if pinned {
                    PINNED_WEB_CAP
                } else {
                    MAX_BUFFER_MESSAGES
                };
                let mut trimmed = false;
                self.messages.update(|msgs| {
                    let entry = msgs.entry(buffer_id.clone()).or_default();
                    // Dedup — guards against the SyncInit → FetchMessages
                    // round-trip race where the same message arrives as both a
                    // live NewMessage and inside the fetched backlog snapshot.
                    // Identity is `same_wire_message` (rowid alone for stored
                    // rows, counter id for live ones — see there for why the
                    // transport id is not a cross-source key). id=0
                    // (date separators) is always admitted.
                    if !message_already_present(entry, &message) {
                        entry.push(message);
                        trimmed = cap_messages(entry, cap);
                    }
                });
                // A trim dropped the oldest rows, so older history exists below
                // the in-memory head again: re-arm scroll-up even if a previous
                // fetch had reached the start and set has_more=false.
                if trimmed {
                    self.backlog_has_more.update(|m| {
                        m.insert(buffer_id.clone(), true);
                    });
                }
                // Update unread count if not the active buffer.
                let is_active = self.active_buffer.get_untracked().as_deref() == Some(&buffer_id);
                if !is_active {
                    self.buffers.update(|bufs| {
                        if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                            b.unread_count += 1;
                        }
                    });
                }
            }
            WebEvent::InsertMessage { buffer_id, message } => {
                // A reconnect gap-fill row. It belongs between the pre-disconnect
                // tail and post-reconnect live messages, so insert it by
                // (timestamp, id) instead of appending. No unread bump — it's
                // backlog, not new activity.
                let active = self.active_buffer.get_untracked();
                let pinned =
                    active.as_deref() == Some(&buffer_id) && !self.is_at_bottom.get_untracked();
                let cap = if pinned {
                    PINNED_WEB_CAP
                } else {
                    MAX_BUFFER_MESSAGES
                };
                let mut trimmed = false;
                self.messages.update(|msgs| {
                    let entry = msgs.entry(buffer_id.clone()).or_default();
                    if !message_already_present(entry, &message) {
                        // Order by full-millisecond ts_ms, not whole-second
                        // timestamp: a gap-fill row sharing a second with live
                        // messages gets a fresh, larger id, so a (seconds, id) key
                        // would place older history after newer live lines.
                        let key = insert_order_key(&message);
                        let pos = entry
                            .iter()
                            .position(|m| insert_order_key(m) > key)
                            .unwrap_or(entry.len());
                        entry.insert(pos, message);
                        trimmed = cap_messages(entry, cap);
                    }
                });
                // A trim dropped the oldest loaded rows, so older history exists
                // below the in-memory head again — re-arm scroll-up even if a
                // previous fetch had reached the start and set has_more=false
                // (mirrors the NewMessage path). Without this, a gap-fill insert at
                // a full buffer could permanently suppress further scroll-back.
                if trimmed {
                    self.backlog_has_more.update(|m| {
                        m.insert(buffer_id.clone(), true);
                    });
                }
            }
            WebEvent::DeleteMessages {
                buffer_id,
                message_ids,
            } => {
                // A transient E2E placeholder was swept server-side after its
                // decrypted line surfaced. Drop it here too — otherwise the
                // placeholder (delivered earlier via NewMessage) and the
                // decrypted InsertMessage both stay visible until a resync.
                self.messages.update(|msgs| {
                    if let Some(entry) = msgs.get_mut(&buffer_id) {
                        entry.retain(|m| !message_ids.contains(&m.id));
                    }
                });
            }
            WebEvent::TopicChanged {
                buffer_id, topic, ..
            } => {
                self.buffers.update(|bufs| {
                    if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                        b.topic = topic;
                    }
                });
            }
            WebEvent::ActivityChanged {
                buffer_id,
                activity,
                unread_count,
            } => {
                self.buffers.update(|bufs| {
                    if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                        b.activity = activity;
                        b.unread_count = unread_count;
                    }
                });
            }
            WebEvent::BufferCreated { buffer } => {
                let new_id = buffer.id.clone();
                self.buffers.update(|bufs| bufs.push(buffer));
                self.sort_buffers();
                // Auto-switch to newly created buffer (matches terminal behavior).
                self.shell_screen.set(None);
                self.active_buffer.set(Some(new_id));
            }
            WebEvent::BufferClosed { buffer_id } => {
                // If the closed buffer was active, switch to first available.
                if self.active_buffer.get_untracked().as_deref() == Some(&buffer_id) {
                    let bufs = self.buffers.get_untracked();
                    let fallback = bufs
                        .iter()
                        .find(|b| b.id != buffer_id)
                        .map(|b| b.id.clone());
                    self.shell_screen.set(None);
                    self.active_buffer.set(fallback);
                }
                self.buffers
                    .update(|bufs| bufs.retain(|b| b.id != buffer_id));
                self.messages.update(|msgs| {
                    msgs.remove(&buffer_id);
                });
                self.nick_lists.update(|lists| {
                    lists.remove(&buffer_id);
                });
                self.nick_lists_loaded.update(|set| {
                    set.remove(&buffer_id);
                });
                self.backlog_loaded.update(|set| {
                    set.remove(&buffer_id);
                });
                // Also drop backlog scroll state, so a buffer later re-created
                // with the same id (e.g. rejoining a channel) doesn't inherit a
                // stale `has_more`/`fetching` entry that would block scroll-up.
                self.backlog_has_more.update(|m| {
                    m.remove(&buffer_id);
                });
                self.backlog_fetching.update(|s| {
                    s.remove(&buffer_id);
                });
                self.typing.update(|t| {
                    t.remove(&buffer_id);
                });
            }
            WebEvent::ConnectionStatus {
                conn_id,
                connected,
                nick,
                label,
            } => {
                self.connections.update(|conns| {
                    if let Some(c) = conns.iter_mut().find(|c| c.id == conn_id) {
                        c.connected = connected;
                        c.nick = nick;
                    } else {
                        conns.push(ConnectionMeta {
                            id: conn_id,
                            label,
                            nick,
                            connected,
                            user_modes: String::new(),
                            lag: None,
                        });
                    }
                });
            }
            WebEvent::Messages {
                buffer_id,
                messages,
                has_more,
                ..
            } => {
                // A scroll-back response arrives while the user is reading
                // backlog (not at bottom): keep a larger window so the just-
                // prepended older page isn't immediately trimmed away. An
                // initial load arrives at the bottom and uses the normal cap.
                let active = self.active_buffer.get_untracked();
                let cap = if active.as_deref() == Some(&buffer_id)
                    && !self.is_at_bottom.get_untracked()
                {
                    PINNED_WEB_CAP
                } else {
                    MAX_BUFFER_MESSAGES
                };
                self.messages.update(|msgs| {
                    let entry = msgs.entry(buffer_id.clone()).or_default();
                    // Drop incoming messages already present in the cache —
                    // happens when NewMessage events arrive between sending
                    // FetchMessages and receiving the response (the server
                    // serves the in-memory buffer for `before=None`, which
                    // includes messages the client already received live).
                    let filtered = dedupe_incoming(entry, messages);
                    // Prepend the older page, inserting date separators and
                    // dropping the redundant head separator at a same-day seam
                    // (mirrors the TUI paginator — see `prepend_backlog_page`).
                    let mut combined = prepend_backlog_page(filtered, std::mem::take(entry));
                    cap_messages(&mut combined, cap);
                    *entry = combined;
                });
                // Mark this buffer's DB backlog as loaded so the Layout Effect
                // won't re-fetch on subsequent switches to this buffer.
                self.backlog_loaded.update(|set| {
                    set.insert(buffer_id.clone());
                });
                // Record whether older history remains, and clear the in-flight
                // guard so the next scroll-up may fetch again.
                self.backlog_has_more.update(|m| {
                    m.insert(buffer_id.clone(), has_more);
                });
                self.backlog_fetching.update(|s| {
                    s.remove(&buffer_id);
                });
            }
            WebEvent::NickList {
                buffer_id, nicks, ..
            } => {
                let nick_count = u32::try_from(nicks.len()).unwrap_or(u32::MAX);
                self.nick_lists.update(|lists| {
                    let mut sorted = nicks;
                    sort_nicks(&mut sorted);
                    lists.insert(buffer_id.clone(), sorted);
                });
                self.nick_lists_loaded.update(|set| {
                    set.insert(buffer_id.clone());
                });
                self.buffers.update(|buffers| {
                    if let Some(buffer) = buffers.iter_mut().find(|buffer| buffer.id == buffer_id) {
                        buffer.nick_count = nick_count;
                    }
                });
            }
            WebEvent::MentionAlert { .. } => {
                self.mention_count.update(|c| *c += 1);
            }
            WebEvent::MentionsList { .. } => {
                self.mention_count.set(0);
            }
            WebEvent::NickEvent {
                buffer_id,
                kind,
                nick,
                new_nick,
                prefix,
                modes,
                away,
                ..
            } => match kind {
                NickEventKind::Join => {
                    let cache_complete = self
                        .nick_lists_loaded
                        .with_untracked(|set| set.contains(&buffer_id));
                    let mut inserted = false;
                    self.nick_lists.update(|lists| {
                        let list = lists.entry(buffer_id.clone()).or_default();
                        if !list.iter().any(|n| n.nick == nick) {
                            inserted = true;
                            list.push(WireNick {
                                nick: nick.clone(),
                                prefix: prefix.unwrap_or_default(),
                                modes: modes.unwrap_or_default(),
                                away: away.unwrap_or(false),
                            });
                            sort_nicks(list);
                        }
                    });
                    if nick_count_should_change(cache_complete, inserted) {
                        self.buffers.update(|bufs| {
                            if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                                b.nick_count = b.nick_count.saturating_add(1);
                            }
                        });
                    }
                }
                NickEventKind::Part | NickEventKind::Quit => {
                    let cache_complete = self
                        .nick_lists_loaded
                        .with_untracked(|set| set.contains(&buffer_id));
                    let mut removed = false;
                    self.nick_lists.update(|lists| {
                        if let Some(list) = lists.get_mut(&buffer_id) {
                            let previous_len = list.len();
                            list.retain(|n| n.nick != nick);
                            removed = list.len() != previous_len;
                        }
                    });
                    if nick_count_should_change(cache_complete, removed) {
                        self.buffers.update(|bufs| {
                            if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                                b.nick_count = b.nick_count.saturating_sub(1);
                            }
                        });
                    }
                }
                NickEventKind::NickChange => {
                    if let Some(ref new) = new_nick {
                        self.nick_lists.update(|lists| {
                            if let Some(list) = lists.get_mut(&buffer_id)
                                && let Some(entry) = list.iter_mut().find(|n| n.nick == nick)
                            {
                                entry.nick = new.clone();
                                sort_nicks(list);
                            }
                        });
                    }
                }
                NickEventKind::ModeChange => {
                    self.nick_lists.update(|lists| {
                        if let Some(list) = lists.get_mut(&buffer_id) {
                            if let Some(entry) = list.iter_mut().find(|n| n.nick == nick) {
                                if let Some(ref p) = prefix {
                                    entry.prefix = p.clone();
                                }
                                if let Some(ref m) = modes {
                                    entry.modes = m.clone();
                                }
                            }
                            sort_nicks(list);
                        }
                    });
                }
                NickEventKind::AwayChange => {
                    self.nick_lists.update(|lists| {
                        if let Some(list) = lists.get_mut(&buffer_id)
                            && let Some(entry) = list.iter_mut().find(|n| n.nick == nick)
                            && let Some(a) = away
                        {
                            entry.away = a;
                        }
                    });
                }
            },
            WebEvent::ActiveBufferChanged { buffer_id } => {
                // Skip if the client already switched to this buffer locally
                // (the click handler sets active_buffer before the server echoes).
                // Without this guard:
                // - The echo re-fires the Layout Effect, causing duplicate FetchMessages
                // - Rapid clicks can be overridden by a stale echo for a previous buffer
                //
                // Multi-tab opt-out: this event is broadcast to ALL web
                // sessions whenever the TUI flips its active buffer.
                // A user with two browser tabs would see them both jump
                // when the TUI moves. Set localStorage
                // `web_follow_tui_buffer=false` to make this tab
                // ignore TUI-driven buffer changes. Default is to
                // follow (the single-tab + TUI workflow expects sync).
                if !follow_tui_active_buffer() {
                    return;
                }
                if self.active_buffer.get_untracked().as_deref() != Some(&buffer_id) {
                    self.shell_screen.set(None);
                    self.active_buffer.set(Some(buffer_id));
                }
            }
            WebEvent::SettingsChanged {
                timestamp_format,
                line_height,
                theme,
                nick_column_width,
                nick_max_length,
                nick_colors,
                nick_colors_in_nicklist,
                nick_color_saturation,
                nick_color_lightness,
                emotes_enabled,
            } => {
                self.timestamp_format.set(timestamp_format);
                self.line_height.set(line_height);
                self.theme.set(theme);
                if nick_column_width > 0 {
                    self.nick_column_width.set(nick_column_width);
                }
                if nick_max_length > 0 {
                    self.nick_max_length.set(nick_max_length);
                }
                self.nick_colors_enabled.set(nick_colors);
                self.nick_colors_in_nicklist.set(nick_colors_in_nicklist);
                self.nick_color_saturation.set(nick_color_saturation);
                self.nick_color_lightness.set(nick_color_lightness);
                self.emotes_enabled.set(emotes_enabled);
            }
            WebEvent::StatusbarConfig { items, enabled } => {
                self.statusbar_items.set(items);
                self.statusbar_enabled.set(enabled);
            }
            WebEvent::Error { message, .. } => {
                self.error.set(Some(message));
            }
            WebEvent::RestoreInput { text, .. } => {
                // The composer owns its own value signal, so hand the text
                // over here and let the input component pick it up.
                self.restore_input.set(Some(text));
            }
            WebEvent::ShellScreen {
                buffer_id,
                cols,
                rows,
                cursor_row,
                cursor_col,
                cursor_visible,
                ..
            } => {
                // Only update if this shell buffer is currently active.
                if self.active_buffer.get_untracked().as_deref() == Some(buffer_id.as_str()) {
                    self.shell_screen
                        .set(Some(crate::protocol::ShellScreenData {
                            cols,
                            rows,
                            cursor_row,
                            cursor_col,
                            cursor_visible,
                        }));
                }
            }
            WebEvent::Typing { buffer_id, nicks } => {
                self.typing.update(|t| {
                    if nicks.is_empty() {
                        t.remove(&buffer_id);
                    } else {
                        t.insert(buffer_id, nicks);
                    }
                });
            }
        }
    }

    /// Sort buffers to match TUI order: mentions first → connection label → buffer type → name.
    ///
    /// Buffer type sort order: mentions(0) < server(1) < channel(2) < query(3) < dcc_chat(4) < special(5).
    fn sort_buffers(&self) {
        let connections = self.connections.get_untracked();
        self.buffers.update(|bufs| {
            bufs.sort_by(|a, b| {
                // Mentions always sorts first, regardless of connection label.
                let a_mentions = a.buffer_type == "mentions";
                let b_mentions = b.buffer_type == "mentions";
                b_mentions
                    .cmp(&a_mentions)
                    .then_with(|| {
                        let label_a = connections
                            .iter()
                            .find(|c| c.id == a.connection_id)
                            .map_or_else(
                                || a.connection_id.to_lowercase(),
                                |c| c.label.to_lowercase(),
                            );
                        let label_b = connections
                            .iter()
                            .find(|c| c.id == b.connection_id)
                            .map_or_else(
                                || b.connection_id.to_lowercase(),
                                |c| c.label.to_lowercase(),
                            );
                        label_a.cmp(&label_b)
                    })
                    .then_with(|| {
                        buf_type_order(&a.buffer_type).cmp(&buf_type_order(&b.buffer_type))
                    })
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        });
    }
}

/// The buffer list with its 1-based display numbers — THE numbering rule.
/// Both the buffer list and the status-line Act indicator must consume this
/// (never re-derive `idx + 1` locally), so "Act: 4" always names the window
/// the list labels "4.".
pub fn numbered_buffers(buffers: &[BufferMeta]) -> impl Iterator<Item = (u32, &BufferMeta)> {
    buffers
        .iter()
        .enumerate()
        .map(|(idx, b)| (u32::try_from(idx + 1).unwrap_or(u32::MAX), b))
}

/// Trim the buffer to `cap`, dropping oldest from the head. Returns `true` if it
/// actually dropped anything — the caller uses that to re-arm `has_more`, since a
/// trim means older history now lives only below the in-memory head.
fn cap_messages(messages: &mut Vec<WireMessage>, cap: usize) -> bool {
    if messages.len() <= cap {
        return false;
    }
    let drop_count = messages.len() - cap;
    messages.drain(..drop_count);
    messages.shrink_to(cap);
    true
}

fn active_buffer_changed(current: Option<&str>, next: Option<&str>) -> bool {
    current != next
}

fn nick_count_should_change(cache_complete: bool, membership_changed: bool) -> bool {
    !cache_complete || membership_changed
}

fn buf_type_order(t: &str) -> u8 {
    match t {
        "mentions" => 0,
        "server" => 1,
        "channel" => 2,
        "query" => 3,
        "dcc_chat" => 4,
        "special" => 5,
        "shell" => 6,
        _ => 7,
    }
}

/// Sort nicks by prefix rank (~&@%+ order), then alphabetically.
fn sort_nicks(nicks: &mut [WireNick]) {
    nicks.sort_by(|a, b| {
        prefix_rank(&a.prefix)
            .cmp(&prefix_rank(&b.prefix))
            .then_with(|| a.nick.to_lowercase().cmp(&b.nick.to_lowercase()))
    });
}

fn prefix_rank(prefix: &str) -> u8 {
    const ORDER: &str = "~&@%+";
    prefix
        .chars()
        .next()
        .and_then(|c| ORDER.find(c))
        .map_or(ORDER.len() as u8, |i| i as u8)
}

/// Local calendar date for a Unix timestamp (the grouping key for date
/// separators). `None` only if the timestamp is out of `chrono`'s range.
fn local_date_of(ts: i64) -> Option<chrono::NaiveDate> {
    use chrono::TimeZone;
    chrono::DateTime::from_timestamp(ts, 0).map(|dt| {
        chrono::Local
            .from_utc_datetime(&dt.naive_utc())
            .date_naive()
    })
}

/// Whether two wire messages are the same logical message.
///
/// `log_id` (the SQLite rowid) is the only cross-source identity: the same
/// row reaches the client as id=rowid from a DB page (`stored_to_wire`) but
/// as a transient AppState-counter id when served from the in-memory buffer
/// (`message_to_wire`) — and that counter restarts with the process, so it
/// can't be compared across sources at all. Two `Some(log_id)` values match
/// on the rowid alone; two live messages (`log_id` None) match on the
/// counter id (same process, stable); a live message never matches a stored
/// row (see `new_live_message_not_dropped_by_db_row_with_same_id`).
///
/// INVARIANT: the `<For>` render key in `chat_view.rs` still keys on the
/// `(id, log_id)` composite — it relies on this dedup layer to guarantee at
/// most one entry per rowid ever reaches the rendered list. Any new insert
/// path must route through `message_already_present`/`dedupe_incoming`.
fn same_wire_message(a: &WireMessage, b: &WireMessage) -> bool {
    match (a.log_id, b.log_id) {
        (Some(x), Some(y)) => x == y,
        (None, None) => a.id == b.id,
        _ => false,
    }
}

/// Whether `msg` is already loaded in `existing`, by the [`same_wire_message`]
/// identity. Date separators (`id == 0`) are never considered duplicates.
fn message_already_present(existing: &[WireMessage], msg: &WireMessage) -> bool {
    msg.id != 0
        && existing
            .iter()
            .any(|m| m.id != 0 && same_wire_message(m, msg))
}

/// Timeline ordering key for a sorted (gap-fill) insert: full-millisecond time,
/// tie-broken by transport id. Falls back to `timestamp * 1000` when `ts_ms` is
/// absent (`0`) so a server that omits the field still orders by whole seconds.
fn insert_order_key(msg: &WireMessage) -> (i64, u64) {
    let ms = if msg.ts_ms != 0 {
        msg.ts_ms
    } else {
        msg.timestamp * 1000
    };
    (ms, msg.id)
}

/// Drop entries from an incoming `messages` batch that are already loaded in
/// `existing`, by the [`same_wire_message`] identity.
///
/// The transport `id` is NOT a stable cross-source key: live messages carry a
/// transient in-memory `AppState` counter (`message_to_wire`) while stored rows
/// carry a `SQLite` rowid (`stored_to_wire`), and the two ranges overlap. Keying
/// on `id` alone would treat a valid older DB row as a duplicate of an unrelated
/// live message with the same number, punching gaps in scroll-back. A stored
/// row's identity is its rowid (`log_id`) alone — the same row may arrive under
/// two different transport ids (in-memory copy vs DB page). Live messages
/// (`log_id` None) key on the counter id. Date separators (`id == 0`) are
/// always admitted.
fn dedupe_incoming(existing: &[WireMessage], messages: Vec<WireMessage>) -> Vec<WireMessage> {
    let mut existing_log_ids: HashSet<i64> = HashSet::new();
    let mut existing_live_ids: HashSet<u64> = HashSet::new();
    for m in existing.iter().filter(|m| m.id != 0) {
        match m.log_id {
            Some(log_id) => {
                existing_log_ids.insert(log_id);
            }
            None => {
                existing_live_ids.insert(m.id);
            }
        }
    }
    messages
        .into_iter()
        .filter(|m| {
            m.id == 0
                || match m.log_id {
                    Some(log_id) => !existing_log_ids.contains(&log_id),
                    None => !existing_live_ids.contains(&m.id),
                }
        })
        .collect()
}

/// Combine a freshly-fetched older `page` (chronological, oldest→newest) with
/// the existing newer `tail`, inserting date separators into the page and
/// dropping the now-redundant head separator at the seam.
///
/// Mirrors the TUI paginator (`src/app/backlog.rs`): `insert_date_separators`
/// always emits a leading separator for the page's first date, and the existing
/// `tail` already carries its own leading separator from when it was first
/// loaded. When the page's newest real message shares a local date with that
/// head separator, the seam has no date change — so the head separator is a
/// duplicate of the one the page just emitted and must be removed, else
/// scrolling back through several pages of the same day shows the date header
/// repeated mid-day.
fn prepend_backlog_page(page: Vec<WireMessage>, mut tail: Vec<WireMessage>) -> Vec<WireMessage> {
    let mut combined = insert_date_separators(page);

    let seam_date = combined
        .iter()
        .rev()
        .find(|m| m.event_key.as_deref() != Some("date_separator"))
        .and_then(|m| local_date_of(m.timestamp));
    if let Some(seam_date) = seam_date
        && tail.first().is_some_and(|m| {
            m.event_key.as_deref() == Some("date_separator")
                && local_date_of(m.timestamp) == Some(seam_date)
        })
    {
        tail.remove(0);
    }

    combined.append(&mut tail);
    combined
}

/// Insert date separator lines between messages from different days.
///
/// Mirrors the TUI's `load_backlog` behavior — adds `─── Day, DD Mon YYYY ───`
/// event lines at each date boundary in the message list.
fn insert_date_separators(messages: Vec<WireMessage>) -> Vec<WireMessage> {
    if messages.is_empty() {
        return messages;
    }

    let mut result = Vec::with_capacity(messages.len() + 5);
    let mut last_date: Option<chrono::NaiveDate> = None;

    for msg in messages {
        let local_date = local_date_of(msg.timestamp);

        if msg.event_key.as_deref() == Some("date_separator") {
            if local_date.is_some() && local_date == last_date {
                continue;
            }
            if local_date.is_some() {
                last_date = local_date;
            }
            result.push(msg);
            continue;
        }

        if msg.event_key.as_deref() == Some("backlog_end") {
            result.push(msg);
            continue;
        }

        if let Some(date) = local_date {
            if last_date.is_some_and(|d| d != date) || last_date.is_none() {
                let formatted = date.format("%a, %d %b %Y");
                result.push(WireMessage {
                    id: 0,
                    timestamp: msg.timestamp,
                    ts_ms: msg.ts_ms,
                    msg_type: "event".to_string(),
                    nick: None,
                    nick_mode: None,
                    text: format!("\u{2500}\u{2500}\u{2500} {formatted} \u{2500}\u{2500}\u{2500}"),
                    highlight: false,
                    log_id: None,
                    event_key: Some("date_separator".to_string()),
                    previews: Vec::new(),
                    orig_offset: None,
                });
            }
            last_date = Some(date);
        }

        result.push(msg);
    }

    result
}

/// LocalStorage key controlling whether this browser tab follows
/// `ActiveBufferChanged` events emitted by the TUI. Set to `"false"`
/// to make this tab independent (typical multi-tab workflow). Any
/// other value (missing, `"true"`) keeps the legacy follow behavior.
const FOLLOW_TUI_BUFFER_KEY: &str = "web_follow_tui_buffer";

fn follow_tui_active_buffer() -> bool {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return true;
    };
    !matches!(
        storage.get_item(FOLLOW_TUI_BUFFER_KEY),
        Ok(Some(ref v)) if v == "false"
    )
}

/// Read + parse an optional localStorage value; absent or malformed → `None`.
fn load_stored_parsed<T: std::str::FromStr>(key: &str) -> Option<T> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
        .and_then(|v| v.parse().ok())
}

/// Persist an optional override: `Some` writes the value, `None` removes the
/// key (back to defaults).
pub fn store_or_remove(key: &str, value: Option<&str>) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    match value {
        Some(v) => {
            let _ = storage.set_item(key, v);
        }
        None => {
            let _ = storage.remove_item(key);
        }
    }
}

const MAX_DISMISSED_ENTRIES: usize = 1000;

/// Read the dismiss list from localStorage. Each entry is `<msg_id>\t<link>`,
/// one per line. Malformed entries are silently dropped.
fn load_dismissed_previews() -> HashSet<(u64, String)> {
    let mut out = HashSet::new();
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return out;
    };
    let Ok(Some(raw)) = storage.get_item(&crate::constants::storage_key("dismissed-previews"))
    else {
        return out;
    };
    for line in raw.lines() {
        if let Some((id_s, link)) = line.split_once('\t')
            && let Ok(id) = id_s.parse::<u64>()
        {
            out.insert((id, link.to_string()));
        }
    }
    out
}

/// Persist the dismiss list to localStorage. Capped at
/// [`MAX_DISMISSED_ENTRIES`] (oldest by hash order) so a long-running
/// session doesn't grow the key indefinitely.
pub fn save_dismissed_previews(dismissed: &HashSet<(u64, String)>) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    let mut entries: Vec<&(u64, String)> = dismissed.iter().collect();
    if entries.len() > MAX_DISMISSED_ENTRIES {
        // Keep the highest message ids — those are the most recent dismissals.
        entries.sort_by_key(|e| std::cmp::Reverse(e.0));
        entries.truncate(MAX_DISMISSED_ENTRIES);
    }
    let mut serialised = String::new();
    for (id, link) in entries {
        serialised.push_str(&id.to_string());
        serialised.push('\t');
        serialised.push_str(link);
        serialised.push('\n');
    }
    let _ = storage.set_item(
        &crate::constants::storage_key("dismissed-previews"),
        &serialised,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AppState::new()` reads `localStorage` through `web_sys`, which panics
    /// off-wasm — build the storage-free half instead.
    fn headless_state() -> AppState {
        AppState::with_persisted("nightfall".to_string(), None, None, HashSet::new())
    }

    #[test]
    fn sync_init_seeds_typing_from_the_snapshot() {
        // The regression: a tab that connects (or lag-resyncs) while alice is
        // mid-sentence used to blank its typing map and wait for a push that
        // never comes — a 3s `active` refresh is not a change server-side, and
        // `paused` is sent once and lives 30s.
        let state = headless_state();
        // Exactly the JSON the server emits, so a field/serde-name drift between
        // the two hand-mirrored protocol.rs files fails here.
        let json = r#"{"type":"SyncInit","buffers":[],"connections":[],"mention_count":0,
            "active_buffer_id":null,"timestamp_format":"%H:%M","emotes_enabled":true,
            "typing":{"libera/#rust":["alice","Bob"]}}"#;
        let event: WebEvent = serde_json::from_str(json).expect("mirrors the server's SyncInit");

        state.handle_event(event);

        assert_eq!(
            state.typing.get_untracked()["libera/#rust"],
            vec!["alice", "Bob"],
            "SyncInit must seed the typing map, not blank it"
        );
    }

    #[test]
    fn sync_init_without_typing_clears_a_stale_map() {
        // The snapshot REPLACES the map: an empty payload still has to wipe
        // whatever the previous session left on screen.
        let state = headless_state();
        state.typing.set(HashMap::from([(
            "libera/#rust".to_string(),
            vec!["ghost".to_string()],
        )]));

        state.handle_event(WebEvent::SyncInit {
            buffers: Vec::new(),
            connections: Vec::new(),
            mention_count: 0,
            active_buffer_id: None,
            timestamp_format: None,
            emotes_enabled: true,
            typing: HashMap::new(),
            statusbar_items: Vec::new(),
            statusbar_enabled: true,
        });

        assert!(state.typing.get_untracked().is_empty());
    }

    #[test]
    fn sync_init_seeds_the_statusbar_config() {
        // The browser must render the status line from the server's config —
        // this is the wire half of that (the renderer half is in
        // `components::status_line`).
        let state = headless_state();
        let json = r#"{"type":"SyncInit","buffers":[],"connections":[],"mention_count":0,
            "timestamp_format":"%H:%M",
            "statusbar_items":["time","channel_info","typing","lag"],
            "statusbar_enabled":false}"#;
        let event: WebEvent = serde_json::from_str(json).expect("parses");
        state.handle_event(event);
        assert_eq!(
            state.statusbar_items.get_untracked(),
            vec!["time", "channel_info", "typing", "lag"]
        );
        assert!(!state.statusbar_enabled.get_untracked());
    }

    #[test]
    fn a_statusbar_config_push_updates_an_open_tab() {
        // `/items remove typing` (or `/set statusbar.enabled false`) in the
        // terminal must reach a tab that is already open, with no refresh.
        let state = headless_state();
        state
            .statusbar_items
            .set(vec!["time".to_string(), "typing".to_string()]);
        state.handle_event(WebEvent::StatusbarConfig {
            items: vec!["time".to_string()],
            enabled: true,
        });
        assert_eq!(state.statusbar_items.get_untracked(), vec!["time"]);
    }

    #[test]
    fn sync_init_from_a_server_that_omits_typing_still_applies() {
        // `#[serde(default)]` on both sides: an older server must not fail the
        // deserialization of the WHOLE SyncInit.
        let state = headless_state();
        let json = r#"{"type":"SyncInit","buffers":[],"connections":[],"mention_count":0,
            "timestamp_format":"%H:%M"}"#;
        let event: WebEvent = serde_json::from_str(json).expect("parses without typing");
        state.handle_event(event);
        assert!(state.typing.get_untracked().is_empty());
    }

    #[test]
    fn active_buffer_changed_preserves_same_buffer() {
        assert!(!active_buffer_changed(Some("shell:1"), Some("shell:1")));
    }

    #[test]
    fn active_buffer_changed_detects_different_buffer() {
        assert!(active_buffer_changed(Some("shell:1"), Some("channel:1")));
    }

    #[test]
    fn partial_nick_cache_does_not_suppress_count_change() {
        assert!(nick_count_should_change(false, false));
    }

    #[test]
    fn complete_nick_cache_suppresses_missing_member_count_change() {
        assert!(!nick_count_should_change(true, false));
    }

    #[test]
    fn complete_nick_cache_applies_member_count_change() {
        assert!(nick_count_should_change(true, true));
    }

    /// A stored (DB-sourced) message: `log_id == id` (the rowid), as
    /// `stored_to_wire` produces.
    fn msg(id: u64, ts: i64) -> WireMessage {
        WireMessage {
            id,
            timestamp: ts,
            ts_ms: ts * 1000,
            msg_type: "message".to_string(),
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: "hi".to_string(),
            highlight: false,
            log_id: Some(i64::try_from(id).unwrap_or_default()),
            event_key: None,
            previews: Vec::new(),
            orig_offset: None,
        }
    }

    /// A live (in-memory) message: transient counter `id`, no `log_id`, as
    /// `message_to_wire` produces for a freshly received message.
    fn live_msg(id: u64, ts: i64) -> WireMessage {
        WireMessage {
            log_id: None,
            ..msg(id, ts)
        }
    }

    fn separator_count(list: &[WireMessage]) -> usize {
        list.iter()
            .filter(|m| m.event_key.as_deref() == Some("date_separator"))
            .count()
    }

    #[test]
    fn gap_fill_orders_by_subsecond_not_id() {
        // A live message at .500 with a small id, and a reconnect gap-fill row at
        // .200 (older) that was assigned a fresh, larger id. The gap-fill row must
        // sort BEFORE the live message — a whole-second (timestamp, id) key would
        // wrongly place it after, since they share the second and its id is bigger.
        let mut live = live_msg(5, 100);
        live.ts_ms = 100_500;
        let mut gap = msg(99, 100);
        gap.ts_ms = 100_200;

        assert!(
            insert_order_key(&gap) < insert_order_key(&live),
            "older gap-fill row sorts before the newer same-second live line"
        );
        // The pre-fix (timestamp, id) key would order them the wrong way round.
        assert!((gap.timestamp, gap.id) > (live.timestamp, live.id));
    }

    #[test]
    fn insert_order_key_falls_back_to_seconds_when_ts_ms_absent() {
        let mut m = live_msg(3, 150);
        m.ts_ms = 0; // a sender that omitted the field
        assert_eq!(insert_order_key(&m), (150_000, 3));
    }

    #[test]
    fn cap_messages_reports_whether_it_trimmed() {
        // The InsertMessage (gap-fill) and NewMessage handlers both rely on this
        // return value to re-arm backlog_has_more when a trim drops loaded rows.
        let mut under: Vec<WireMessage> = (0..3).map(|i| msg(i, 100)).collect();
        assert!(!cap_messages(&mut under, 5), "no trim under the cap");
        assert_eq!(under.len(), 3);

        let mut over: Vec<WireMessage> = (0..8).map(|i| msg(i, 100)).collect();
        assert!(
            cap_messages(&mut over, 5),
            "trims and reports true at the cap"
        );
        assert_eq!(over.len(), 5);
        // Oldest rows are the ones dropped (drain from the front).
        assert_eq!(over.first().map(|m| m.id), Some(3));
    }

    // 2024-06-09 12:00 / 13:00 UTC — same calendar day in every realistic tz.
    const JUN9_12: i64 = 1_717_934_400; // 2024-06-09T12:00:00Z
    const JUN9_13: i64 = 1_717_938_000; // 2024-06-09T13:00:00Z
    const JUN9_14: i64 = 1_717_941_600; // 2024-06-09T14:00:00Z
    // 2024-06-08 12:00 UTC — a clearly different day, far from any midnight.
    const JUN8_12: i64 = 1_717_848_000; // 2024-06-08T12:00:00Z

    #[test]
    fn seam_same_day_drops_duplicate_separator() {
        // Existing tail: one Jun-9 page already loaded, led by its separator.
        let tail = insert_date_separators(vec![msg(10, JUN9_14)]);
        assert_eq!(separator_count(&tail), 1);

        // Scroll back: an older Jun-9 page. The seam has no date change, so the
        // tail's head separator must be dropped — exactly one Jun-9 header total.
        let page = vec![msg(1, JUN9_12), msg(2, JUN9_13)];
        let combined = prepend_backlog_page(page, tail);

        assert_eq!(
            separator_count(&combined),
            1,
            "same-day seam must collapse to a single date separator"
        );
        // The lone separator sits at the very top, above the oldest message.
        assert_eq!(combined[0].event_key.as_deref(), Some("date_separator"));
        assert_eq!(combined[1].id, 1);
    }

    #[test]
    fn seam_date_change_keeps_both_separators() {
        // Tail starts on Jun 9; older page is entirely Jun 8 → the boundary is a
        // real date change, so both separators are correct and must survive.
        let tail = insert_date_separators(vec![msg(10, JUN9_12)]);
        let page = vec![msg(1, JUN8_12)];
        let combined = prepend_backlog_page(page, tail);

        assert_eq!(
            separator_count(&combined),
            2,
            "a genuine date change at the seam keeps both date separators"
        );
    }

    #[test]
    fn prepend_with_empty_page_is_a_noop_on_separators() {
        let tail = insert_date_separators(vec![msg(10, JUN9_12)]);
        let before = separator_count(&tail);
        let combined = prepend_backlog_page(Vec::new(), tail);
        assert_eq!(separator_count(&combined), before);
    }

    #[test]
    fn server_date_separator_is_not_duplicated() {
        let mut server_separator = msg(1, JUN9_12);
        server_separator.msg_type = "event".to_string();
        server_separator.nick = None;
        server_separator.event_key = Some("date_separator".to_string());
        server_separator.text = "─── Sun, 09 Jun 2024 ───".to_string();

        let with_separators = insert_date_separators(vec![server_separator, msg(2, JUN9_13)]);

        assert_eq!(separator_count(&with_separators), 1);
    }

    #[test]
    fn backlog_end_does_not_create_a_date_boundary() {
        let mut marker = msg(1, JUN8_12);
        marker.msg_type = "event".to_string();
        marker.nick = None;
        marker.event_key = Some("backlog_end".to_string());

        let with_separators = insert_date_separators(vec![marker, msg(2, JUN9_12)]);

        assert_eq!(separator_count(&with_separators), 1);
    }

    #[test]
    fn dedupe_keeps_db_rows_overlapping_live_ids() {
        // The bug: live messages hold transient counter ids (100,150,200) while
        // an older DB page holds rowids (50,100,150). Keying on id alone would
        // drop rowids 100 and 150 as "duplicates" of the live ids, gapping the
        // scroll-back. They are distinct messages and must all survive.
        let existing = vec![
            live_msg(100, JUN9_12),
            live_msg(150, JUN9_13),
            live_msg(200, JUN9_14),
        ];
        let incoming = vec![msg(50, JUN8_12), msg(100, JUN8_12), msg(150, JUN8_12)];
        let kept = dedupe_incoming(&existing, incoming);
        assert_eq!(kept.len(), 3, "overlapping DB rowids must not be dropped");
    }

    #[test]
    fn dedupe_drops_live_message_already_present() {
        // Initial-load overlap: the server re-sends a live message the client
        // already received (same in-memory id, both log_id = None) — drop it.
        let existing = vec![live_msg(5, JUN9_12)];
        let incoming = vec![live_msg(5, JUN9_12), live_msg(6, JUN9_13)];
        let kept = dedupe_incoming(&existing, incoming);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 6);
    }

    #[test]
    fn dedupe_drops_db_row_already_present() {
        // A re-served stored row (same rowid) is a genuine duplicate.
        let existing = vec![msg(100, JUN9_12)];
        let incoming = vec![msg(100, JUN9_12), msg(99, JUN8_12)];
        let kept = dedupe_incoming(&existing, incoming);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 99);
    }

    #[test]
    fn dedupe_always_admits_separators() {
        let existing = vec![live_msg(0, JUN9_12)]; // id 0 sentinel present
        let incoming = vec![live_msg(0, JUN9_13)];
        let kept = dedupe_incoming(&existing, incoming);
        assert_eq!(kept.len(), 1, "id-0 separators are never deduped");
    }

    #[test]
    fn new_live_message_not_dropped_by_db_row_with_same_id() {
        // A backlog DB row with rowid 42 is loaded; a fresh live message also
        // carries in-memory id 42. They are distinct (log_id Some vs None) and
        // the live one must NOT be silently dropped.
        let existing = vec![msg(42, JUN9_12)]; // DB row, log_id = Some(42)
        let incoming = live_msg(42, JUN9_14); // live, log_id = None
        assert!(
            !message_already_present(&existing, &incoming),
            "a live message must not collide with a DB rowid"
        );
    }

    #[test]
    fn same_db_row_under_different_transport_id_is_duplicate() {
        // The same SQLite row can reach the client under two transport ids:
        // in-memory copies go out with the AppState counter (`message_to_wire`,
        // id=500 here) while DB pages carry the rowid (`stored_to_wire`,
        // id=100). `log_id` (the rowid) is the stable identity — same
        // `Some(log_id)` means the same row, whatever the transport id says.
        let seeded = WireMessage {
            id: 500,
            log_id: Some(100),
            ..msg(100, JUN9_12)
        };
        let existing = vec![seeded];
        let incoming = msg(100, JUN9_12); // id=100, log_id=Some(100)
        assert!(
            message_already_present(&existing, &incoming),
            "same rowid under a different transport id is the same message"
        );
        let kept = dedupe_incoming(&existing, vec![msg(100, JUN9_12), msg(99, JUN8_12)]);
        assert_eq!(kept.len(), 1, "the rowid-100 copy must be dropped");
        assert_eq!(kept[0].id, 99);
    }

    #[test]
    fn duplicate_live_message_is_detected() {
        // The race the dedup guards: the same live message already present.
        let existing = vec![live_msg(7, JUN9_12)];
        let incoming = live_msg(7, JUN9_12);
        assert!(message_already_present(&existing, &incoming));
    }

    #[test]
    fn separator_is_never_a_duplicate() {
        let existing = vec![live_msg(0, JUN9_12)];
        let incoming = live_msg(0, JUN9_13);
        assert!(!message_already_present(&existing, &incoming));
    }
}

