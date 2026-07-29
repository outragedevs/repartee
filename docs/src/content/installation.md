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
make release
./target/release/repartee
```

## Troubleshooting

### `<jemalloc>: Unsupported system page size`

```text
<jemalloc>: Unsupported system page size
memory allocation of 4 bytes failed
Aborted
```

The prebuilt Linux binary uses jemalloc, which fixes its page size at compile time and refuses to start when the running kernel's pages are larger. Affected releases are **v1.7.0 and earlier on ARM64**, where the binary was built for 4 KiB pages. It hits any aarch64 kernel with larger pages — most visibly Raspberry Pi OS on a Pi 5, which defaults to 16 KiB.

Check what your kernel uses:

```bash
getconf PAGESIZE
```

`4096` means something else is wrong. `16384` or `65536` means you have hit this.

Two ways out, in order of preference:

1. **Use v1.7.1 or newer.** The ARM64 binary is now built for 64 KiB pages, which covers 4, 16, and 64 KiB kernels alike.
2. **Build it yourself** with `cargo install repartee`. Compiling on the machine you will run on always matches its page size.

Forcing the kernel back to 4 KiB pages (`kernel=kernel8.img` in `/boot/firmware/config.txt` on a Pi 5) also works, but it changes your system to suit one program — prefer either option above.

## Command-line usage

```
repartee                     # normal start (fork + terminal)
repartee -d / --detach       # start headless (no terminal)
repartee a [pid]             # attach to a running session
repartee attach [pid]        # same as above
repartee l / logs            # open read-only log browser
repartee -h / --bind <ip>    # bind outgoing connections to a local IP
repartee -v / --version      # print version
repartee --help / help       # print usage for all of the above
```

`-h` is the bind-address flag, as in irssi — usage is `--help`. Run
`repartee --help` for the same list plus the environment variables and file
paths repartee uses.

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
