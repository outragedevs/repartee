.PHONY: all clean wasm build release install test clippy check

# Full clean rebuild: clean → WASM → native release
all: clean wasm release

# Clean all build artifacts
clean:
	cargo clean
	rm -rf web-ui/dist

# Build WASM frontend
wasm:
	cd web-ui && trunk build --release

# Native release build (embeds WASM from web-ui/dist/)
release:
	cargo build --release

# Native dev build (no WASM rebuild)
build:
	cargo build -p repartee

# Install to /usr/local/bin
install: release
	cp target/release/repartee /usr/local/bin/repartee
	ln -sf /usr/local/bin/repartee /usr/local/bin/reptee

# Run tests
test:
	cargo test -p repartee

# Run clippy
clippy:
	cargo clippy -p repartee --all-targets
