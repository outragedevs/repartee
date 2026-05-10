# Web frontend improvements — sessions, login, clickable links, image previews

**Branch:** `feat/web-improvements`
**Status:** spec approved, autonomous implementation
**Author:** Dawid + Claude (auto-mode)
**Date:** 2026-05-10

## Goals

Bring the Repartee web UI closer to The Lounge in four practical areas, none of which require touching IRC protocol or storage code:

1. **Clickable links** in chat messages (left-click → new tab, right-click → browser context menu).
2. **Optional image previews** under each message that contains image URLs (server-proxied thumbnails, with an Imgur exception that loads from Imgur directly because Imgur blocks most server IPs).
3. **Persistent login sessions** that survive process restart and IP changes (so a phone bouncing between Wi-Fi and cellular doesn't get logged out, and adding the site to the home screen as a PWA stays signed in for ~90 days).
4. **Username field** in the login form so password managers (1Password / Bitwarden / iCloud Keychain) recognise it as a standard login form and offer to fill credentials.

## Non-goals

- Multi-user accounts. Repartee is single-user; the username field is purely cosmetic / for password manager UX.
- Per-link "OG metadata" preview cards (title + description + image like The Lounge does). Just the thumbnail image.
- Video / audio embeds.
- Push notifications.
- A "list active sessions" UI (the data structure leaves room for it, but no UI in this scope).

## Design summary

### 1. Persistent sessions

**Storage.** Replace the in-memory `SessionStore` with a file-backed one at `~/.repartee/web_sessions.bin`. Persist using `postcard` (already used for session detach in `src/session/`) with file permission `0600`. Tokens are stored **hashed** — never in clear — using `HMAC-SHA256(token, web_session_secret)`. The 32-byte session secret lives in `~/.repartee/.env` as `WEB_SESSION_SECRET`. If the env var is missing on startup, generate a random one and persist it (so it stays stable across restarts, but rotating it invalidates every existing session — a deliberate "log everyone out" knob).

Persisted record:

```rust
struct PersistedSession {
    token_hash: [u8; 32],   // HMAC(raw_token, WEB_SESSION_SECRET)
    created_at: i64,         // unix seconds
    last_used: i64,          // unix seconds
    user_agent: String,      // display only — not used for validation
    label: Option<String>,   // future: "iPhone Safari", manually settable
}
```

**Lifecycle.**
- `SessionStore::load(path, secret)` at server startup; missing file → empty store.
- `create(user_agent)` → 32 random bytes → hex-encode = raw token (returned to client) → HMAC = stored key → push to in-memory map → `save_atomic()` (write tmp + rename, perm 0600).
- `validate(raw_token)` → HMAC → lookup → check `last_used + max_age > now`. Returns `&Session` on hit, `None` otherwise. **No IP-binding. No UA-binding.** The 32-byte HttpOnly+Secure+SameSite=Strict cookie is the security boundary.
- `record_use(raw_token)` → bump `last_used`. Save is debounced — only flush to disk every 60s of activity, plus on every `create`/`revoke`. (Avoids a disk write on every WS message.)
- `purge_expired()` runs hourly from a background tokio task.
- `revoke(raw_token)` and `revoke_all()` for logout (`revoke` wired to `POST /api/logout`; `revoke_all` not wired this scope but available).

**Cookie.** The `Set-Cookie` header gains `Max-Age=<session_days * 86400>` (default 90 days, configurable via `web.session_days`). Other attributes unchanged: `HttpOnly`, `Secure`, `SameSite=Strict`, `Path=/`. The legacy `web.session_hours` config field is removed (no published v1.x users yet for this feature).

**WebSocket handshake.** `ws::ws_handler` already pulls the cookie and calls `SessionStore::validate(token, ip)`. Drop the IP arg; otherwise unchanged.

### 2. Username field

**Config.** Add `web.username: String` (default `"repartee"`).

**Endpoint.** `GET /api/login_info` (unauthenticated, no rate limit) returns `{ "username": "repartee" }`. The login form fetches this once on mount and pre-fills the username input. (Mirrors how The Lounge embeds initial public config into the page; an endpoint is simpler in our axum + WASM split because the WASM bundle is static.)

**Form.** `web-ui/src/components/login.rs` is wrapped in a real `<form>` (browsers refuse to offer "save password" on bare inputs). Markup:

```html
<form on:submit=...>
  <input type="text"     name="username" autocomplete="username"         value=...>
  <input type="password" name="password" autocomplete="current-password" value=...>
  <button type="submit">Login</button>
</form>
```

**Login endpoint.** `POST /api/login` body becomes `{ username, password }`. The server **ignores** the username field and only validates the password — semantically there is one user. The username is sent so the browser sees a complete credential pair (username for the autofill record's "key", password for verification). The username's only purpose is password-manager UX; it is not authoritative.

`/set web.username <value>` updates the config; the form picks it up via `/api/login_info` next page load.

### 3. Clickable links

**Pure client-side.** No protocol or backend change.

In `web-ui/src/format.rs`, after `parse_format` produces `StyledSpan`s, a second pass `linkify_spans(spans)` walks every span and:
1. Skips spans that already have non-text content (none currently exist, but the function is future-proof).
2. For plain-text spans, runs the URL regex (same regex as `src/image_preview/detect::URL_RE`) over the text.
3. Splits the span at every match into a sequence: `[text-before, link, text-between, link, ..., text-after]`. Each fragment inherits the original span's styling. Link fragments gain `link: Some(url)`.

Updated `StyledSpan`:

```rust
pub struct StyledSpan {
    pub text: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub link: Option<String>,    // NEW
}
```

In `chat_view.rs`, `render_styled_text` checks `span.link`:
- `None` → `<span style=...>` (current behaviour)
- `Some(url)` → `<a href={url} target="_blank" rel="noopener noreferrer" class="msg-link" style=...>{text}</a>`

`target="_blank"` triggers "open in new tab" on left-click. Right-click yields the browser's native link context menu ("Open Link in New Window/Tab", "Copy Link Address", etc.). `rel="noopener noreferrer"` is mandatory: prevents the opened tab from accessing `window.opener` and from leaking referrer headers to the destination.

CSS additions in `web-ui/styles/`:
```css
.msg-link {
    color: var(--link, #87cefa);
    text-decoration: underline;
    text-decoration-thickness: 1px;
    cursor: pointer;
}
.msg-link:hover { text-decoration-thickness: 2px; }
```

### 4. Image previews

**Toggle.** `web.image_previews: bool` (default `false`) and `web.image_previews_max_per_msg: u32` (default `4`, matching The Lounge's cap). When the server has previews disabled, no extraction work is performed and clients render no thumbnails regardless of their local preference.

When the server has previews enabled, individual clients can still hide them locally via a localStorage toggle (`web_image_previews_enabled`, default `true` once the server enables). UI for the local toggle is out of scope for v1 — the localStorage key is read but flipping it requires devtools. Adding a Settings panel toggle is a follow-up.

**Server-side extraction.**

A new module `src/web/preview.rs` exposes `WebPreviewExtractor`:

```rust
pub struct WebPreviewExtractor {
    secret: [u8; 32],
    max_per_msg: usize,
    registry: Mutex<HashMap<String, String>>,  // hash → url
}

impl WebPreviewExtractor {
    pub fn extract(&self, text: &str) -> Vec<LinkPreview> {
        // 1. extract_urls(text) using image_preview::detect
        // 2. For each URL, classify with the Imgur exception:
        //    - host matches *.imgur.com → if DirectImage, ClientDirect
        //                                  else, Skip
        //    - other DirectImage         → ServerProxy
        //    - ImgbbPage / GenericPage   → ServerProxy
        //    - ImgurPage (caught above)  → Skip
        // 3. For ServerProxy, compute hash = hex(HMAC(secret, url))
        //    and insert into registry.
        // 4. Cap at max_per_msg, dedupe identical URLs within one message.
    }
}
```

Wire types (in `web/protocol.rs`):

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct LinkPreview {
    pub link: String,                  // original URL from message text
    pub kind: LinkPreviewKind,
    pub thumb_url: Option<String>,     // /api/preview?h=<hash> OR https://i.imgur.com/...
}

#[derive(Clone, Serialize, Deserialize)]
pub enum LinkPreviewKind {
    ClientDirect,   // <img src=link>; browser fetches from third party
    ServerProxy,    // <img src="/api/preview?h=…">; server fetches + thumbnails
}
```

`WireMessage` gains `pub previews: Vec<LinkPreview>` (with `serde(default, skip_serializing_if = "Vec::is_empty")` to keep backlog and old clients happy).

**Plumbing.** `AppState` and `App` both gain `web_preview_extractor: Option<Arc<WebPreviewExtractor>>` (None = feature disabled). `message_to_wire` and `stored_to_wire` take `Option<&WebPreviewExtractor>`; all six call sites in `state/events.rs` and `app/web.rs` thread it through. When `None`, the extractor field on the wire message is empty.

**Endpoint.** `GET /api/preview?h=<hex_hash>` (auth required; standard cookie/session check):

1. Lookup `hash → url` in the extractor's registry. Miss → `404 Not Found`.
2. Cache check: `~/.repartee/web_thumbnails/<hash>.jpg`. Hit → stream from disk.
3. Cache miss → fetch source:
   - URL classified `DirectImage` → re-use `image_preview::fetch::fetch_image` (network fetch with size + timeout limits).
   - URL classified `ImgbbPage` / `GenericPage` → fetch HTML, parse with `scraper`, find first `<meta property="og:image">` content. Re-fetch that URL as image.
4. Decode with `image::ImageReader`. Generate thumbnail via `image::imageops::thumbnail(&img, 400, 300)` (preserves aspect, downscales only).
5. Encode JPEG quality 80 → `image::codecs::jpeg::JpegEncoder`.
6. Write to cache atomically (tmp + rename, perm 0600 inherited from parent).
7. Respond `Content-Type: image/jpeg`, `Cache-Control: public, max-age=86400, immutable`, body = bytes.

**Cache size.** `web.thumbnail_cache_mb: u32` (default 200). On startup and hourly thereafter, prune the cache directory by oldest mtime until total size ≤ limit.

**Imgur details.** "Is this Imgur?" = `host.ends_with("imgur.com")`. So `i.imgur.com/abc.png` and `imgur.com/gallery/xyz` both count. The Direct vs Page distinction (already classified by `image_preview::detect`) decides:
- `i.imgur.com/abc.png` → `DirectImage` → `ClientDirect` with `thumb_url = Some(link)`.
- `imgur.com/gallery/xyz` → `ImgurPage` → `Skip` (no preview, just a link). Imgur reliably blocks server-side scraping of gallery HTML, so attempting it adds latency for nothing.

**Frontend rendering.** In `chat_view.rs`, after the message text spans, render:

```html
<div class="msg-previews" style:display=if previews.is_empty() {"none"}>
  {for p in previews where !is_dismissed(msg.id, p.link)}
    <a href={p.link} target="_blank" rel="noopener noreferrer" class="msg-preview-link">
      <img src={p.thumb_url.unwrap()} class="msg-preview-thumb"
           loading="lazy"
           on:error=hide_self />
    </a>
    <button class="msg-preview-dismiss" on:click=dismiss>×</button>
  {/for}
</div>
```

Per-message dismiss state: `state.dismissed_previews: RwSignal<HashSet<(msg_id, link)>>` backed by localStorage. Pruned to last 1000 entries on persist (avoid unbounded growth). Matches The Lounge's behaviour.

`loading="lazy"` lets the browser defer fetching off-screen thumbnails. `on:error` hides the broken `<img>` parent so a failed preview doesn't leave a broken icon.

**Security note.** Using `?h=<hash>` instead of `?url=<encoded>` is deliberate — the server only knows about URLs it has previously extracted from real IRC messages, so the endpoint cannot be abused as an open HTTP proxy.

## File-by-file change map

### Backend (Rust)
| File | Change |
|------|--------|
| `src/config/mod.rs` | Add `WebConfig` fields: `username`, `session_days`, `image_previews`, `image_previews_max_per_msg`, `thumbnail_cache_mb`. Remove `session_hours`. |
| `src/config/env.rs` | Read `WEB_SESSION_SECRET`; auto-generate + persist if missing. |
| `src/web/auth.rs` | Replace in-memory `SessionStore` with file-backed, hashed-token store. New `Session::user_agent`, `Session::label`. Drop IP arg from `validate`. |
| `src/web/preview.rs` (new) | `WebPreviewExtractor`, hash registry, fetch + thumbnail logic. |
| `src/web/server.rs` | `LoginRequest { username, password }` (username unused); cookie gets `Max-Age`; new routes `GET /api/login_info` and `GET /api/preview`; `AppHandle.preview_extractor`. |
| `src/web/ws.rs` | Drop IP arg from `validate`. |
| `src/web/protocol.rs` | `LinkPreview`, `LinkPreviewKind`; add `previews` to `WireMessage`. |
| `src/web/snapshot.rs` | `message_to_wire(msg, extractor)` and `stored_to_wire(msg, extractor)` take optional extractor. |
| `src/state/events.rs` | All four `message_to_wire` call sites pass `self.web_preview_extractor.as_deref()`. New field on `AppState`. |
| `src/state/mod.rs` (or wherever AppState is defined) | New field `web_preview_extractor: Option<Arc<WebPreviewExtractor>>`. |
| `src/app/web.rs` | Three `message_to_wire`/`stored_to_wire` call sites updated. Pass extractor from `App`. |
| `src/app/mod.rs` (or where App is built) | Construct `WebPreviewExtractor` from config + secret on startup; share with both `AppState` and `AppHandle`. |
| `src/commands/settings.rs` | Add new keys to get/set match arms and `WEB_SETTINGS` list. |

### Frontend (Leptos WASM)
| File | Change |
|------|--------|
| `web-ui/src/protocol.rs` | Mirror `LinkPreview`, `LinkPreviewKind`, add `previews` to `Message`. |
| `web-ui/src/format.rs` | `StyledSpan.link: Option<String>`. New `linkify_spans()` second pass. |
| `web-ui/src/components/login.rs` | Wrap in `<form>`, add username `<input>`, fetch `/api/login_info`, submit `{ username, password }`. |
| `web-ui/src/components/chat_view.rs` | Render `<a>` for spans with `link`. Render `.msg-previews` block under each message that has `previews`. Dismiss button. |
| `web-ui/src/state.rs` | `dismissed_previews: RwSignal<HashSet<(u64, String)>>` with localStorage persistence. |
| `web-ui/styles/` | CSS for `.msg-link`, `.msg-previews`, `.msg-preview-thumb`, `.msg-preview-dismiss`. |

## Testing strategy

**Backend.**
- `web/auth.rs`: round-trip save/load; HMAC stored, raw token never on disk; rotating secret invalidates existing tokens; expiry; debounced save.
- `web/preview.rs`: classification routes (Imgur DirectImage → ClientDirect, ImgurPage → Skip, other DirectImage → ServerProxy, ImgbbPage/GenericPage → ServerProxy); hash determinism; cap honoured; deduplication.
- `web/server.rs`: `/api/login_info` returns configured username; `/api/login` accepts `{username, password}` and ignores username; cookie has `Max-Age`; `/api/preview` 404s for unknown hash, 401s without session.
- `web/snapshot.rs`: `message_to_wire(msg, None)` produces empty `previews`; with `Some(extractor)` populates it.

**Frontend.**
- `web-ui/src/format.rs`: `linkify_spans` correctly splits a styled span containing one or more URLs; preserves styling on each fragment; handles URL at start, middle, end; handles adjacent URLs.

**Manual / integration.**
- Cargo build + clippy + test must pass with 0 warnings (project policy).
- `make wasm` must build successfully (no Leptos type regressions).
- Smoke test: log in via browser with default `repartee` username, verify cookie persists across browser restart (Application tab > Storage > Cookies > Max-Age column), verify session survives `repartee` restart, verify links open on click, enable previews and verify thumbnails render.

## Risks and follow-ups

- **Cache directory growth.** Background prune is `O(n)` over `web_thumbnails/`; with the 200 MB default cap and ~30 KB JPEGs that's ~7000 files which is fine. If a user sets a very large cap we may want a B-tree of mtimes; out of scope.
- **HMAC hash collisions.** Truncating the HMAC to 64 bits would be unsafe as a registry key; we keep all 32 bytes (64 hex chars) — collision probability is cryptographically negligible.
- **Imgur direct images and CORS.** `<img src="https://i.imgur.com/...">` in a page served from `https://repart.ee` should work — `<img>` requests are not subject to CORS, only XHR/fetch are. If Imgur ever serves a `Cross-Origin-Resource-Policy: same-origin` header for direct images, we will need to fall back to server proxy with a known-good user agent. Not worth pre-handling.
- **WebKit (Safari) and SameSite=Strict cookies on PWA install.** Empirically Safari preserves cookies across "Add to Home Screen" if `Max-Age` is set and `SameSite` is `Strict` or `Lax`. Validation is part of the manual smoke test.
- **No "remember me" toggle.** Every successful login gets the full 90-day cookie. Acceptable for single-user; revisit if multi-device session management becomes a need.
- **Username changes don't auto-update existing browser sessions.** A user who changes `web.username` after some browsers have already saved credentials will see two saved logins in their password manager. Documented behaviour, not a bug.
