# repartee

A modern terminal IRC client built with Ratatui, Tokio, and Rust. Inspired by irssi, designed for the future.

## Demo

<div style="text-align: center; margin: 16px 0;">
  <a href="https://www.youtube.com/watch?v=okU4WKF5GDI" target="_blank">
    <img src="https://img.youtube.com/vi/okU4WKF5GDI/maxresdefault.jpg" alt="Repartee Demo" style="max-width: 100%; border-radius: 8px; border: 1px solid var(--border);">
  </a>
  <p style="color: var(--text-muted); font-size: 13px; margin-top: 6px;">TUI (left) | Mobile web (center) | Desktop web (right) — 1:1 state sync across all interfaces.</p>
</div>

## Features

<div class="card-grid">
  <div class="card">
    <div class="card-title">Full IRC Protocol</div>
    <div class="card-body">Channels, queries, CTCP, SASL, TLS, channel modes, ban lists — the complete IRC experience.</div>
  </div>
  <div class="card">
    <div class="card-title">irssi-style Navigation</div>
    <div class="card-body">Esc+1–9 window switching, /commands, aliases. If you know irssi, you already know repartee.</div>
  </div>
  <div class="card">
    <div class="card-title">Mouse Support</div>
    <div class="card-body">Click buffers and nicks, drag to resize side panels. Terminal client, modern interaction.</div>
  </div>
  <div class="card">
    <div class="card-title">Netsplit Detection</div>
    <div class="card-body">Batches join/part floods into single events so your scrollback stays readable.</div>
  </div>
  <div class="card">
    <div class="card-title">Flood Protection</div>
    <div class="card-body">Blocks CTCP spam and nick-change floods from botnets automatically.</div>
  </div>
  <div class="card">
    <div class="card-title">Persistent Logging</div>
    <div class="card-body">SQLite with optional AES-256-GCM encryption and FTS5 full-text search across all logs.</div>
  </div>
  <div class="card">
    <div class="card-title">Nick Coloring</div>
    <div class="card-body">Deterministic per-nick colors (WeeChat-style). HSL hue wheel for truecolor, 256-color and 16-color fallbacks. Auto-detected terminal capability, configurable saturation/lightness.</div>
  </div>
  <div class="card">
    <div class="card-title">Theming</div>
    <div class="card-body">irssi-compatible format strings with 24-bit color support and custom abstracts.</div>
  </div>
  <div class="card">
    <div class="card-title">Lua Scripting</div>
    <div class="card-body">Lua 5.4 scripts with an event bus, custom commands, and full IRC/state access.</div>
  </div>
  <div class="card">
    <div class="card-title">Spell Checking</div>
    <div class="card-body">Multilingual inline spell checking with Hunspell dictionaries and a 7,400-word computing/IT dictionary. Replace and highlight modes, Tab cycles suggestions.</div>
  </div>
  <div class="card">
    <div class="card-title">IRCv3 Capabilities</div>
    <div class="card-body">Full IRCv3 suite: server-time, echo-message, away-notify, account tags, BATCH netsplit grouping, SASL SCRAM-SHA-256, and more.</div>
  </div>
  <div class="card">
    <div class="card-title">Extended Bans</div>
    <div class="card-body">WHOX account tracking and extban support — ban by account name with <code>/ban -a accountname</code>.</div>
  </div>
  <div class="card">
    <div class="card-title">Web Frontend</div>
    <div class="card-body">Built-in HTTPS web UI with mobile support. Real-time bidirectional sync with the terminal — switch buffers, send messages, see nick changes live.</div>
  </div>
  <div class="card">
    <div class="card-title">DCC CHAT</div>
    <div class="card-body">Direct client-to-client messaging with active and passive (reverse) connections, auto-accept masks, and nick tracking.</div>
  </div>
  <div class="card">
    <div class="card-title">Embedded Shell</div>
    <div class="card-body">Full PTY terminal inside Repartee — run vim, btop, irssi, or any command without leaving the client. Available in TUI and web (beamterm WebGL2 renderer with Nerd Font, mouse selection, font resize).</div>
  </div>
  <div class="card">
    <div class="card-title">Detach & Reattach</div>
    <div class="card-body">Detach from your terminal and reattach later — IRC connections stay alive. Like tmux, built in.</div>
  </div>
  <div class="card">
    <div class="card-title">Single Binary</div>
    <div class="card-body">Compiles to a ~15MB standalone executable with WASM web frontend bundled. Zero runtime dependencies.</div>
  </div>
  <div class="card">
    <div class="card-title">Written in Rust</div>
    <div class="card-body">Memory-safe, zero-cost abstractions, fearless concurrency. Built on tokio async runtime.</div>
  </div>
</div>

## Quick Install

```bash
cargo install repartee
repartee
```

That's it. No build steps, no configuration required. Connect to a server with `/server add` and you're chatting.

New to repartee? Start with the [Installation guide](installation.html).
