# Installation

## Requirements

- **Rust 1.85+** — repartee uses the Rust 2024 edition. Install the toolchain with `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`.
- **A terminal with 256-color or truecolor support** — any modern terminal works: iTerm2, Alacritty, kitty, WezTerm, Windows Terminal, GNOME Terminal, etc.

## Install from crates.io

The quickest way to get started:

```bash
cargo install repartee
repartee
```

## Install from source

If you want to hack on repartee or run the latest unreleased code:

```bash
git clone https://github.com/outragedevs/repartee.git
cd repartee
cargo build --release
./target/release/repartee
```

## Command-line usage

```
repartee                  # normal start (fork + terminal)
repartee -d / --detach    # start headless (no terminal)
repartee a [pid]          # attach to a running session
repartee attach [pid]     # same as above
repartee -v / --version   # print version
```

See [Sessions & Detach](sessions.html) for details on background sessions.

## Binary size

The release binary is approximately 5MB (includes bundled SQLite and Lua). The `--release` profile enables LTO, single codegen unit, and symbol stripping for minimal size.

## Build options

The `Cargo.toml` release profile is pre-configured for small binaries:

```toml
[profile.release]
lto = true
codegen-units = 1
panic = "abort"
strip = true
```
