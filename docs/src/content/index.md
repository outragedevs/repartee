# repartee

A modern terminal IRC client built with Ratatui, Tokio, and Rust. Inspired by irssi, designed for the future.

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
    <div class="card-title">Theming</div>
    <div class="card-body">irssi-compatible format strings with 24-bit color support and custom abstracts.</div>
  </div>
  <div class="card">
    <div class="card-title">Lua Scripting</div>
    <div class="card-body">Lua 5.4 scripts with an event bus, custom commands, and full IRC/state access.</div>
  </div>
  <div class="card">
    <div class="card-title">Single Binary</div>
    <div class="card-body">Compiles to a ~5MB standalone executable. Zero runtime dependencies.</div>
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
