.PHONY: all clean wasm build release install

all: release

# Build WASM frontend with trunk
wasm:
	cd web-ui && trunk build --release

# Clean all build artifacts
clean:
	cargo clean

# Development build (native only, no WASM rebuild)
build:
	cargo build -p repartee

# Full release build: WASM → clean → native
release: wasm
	cargo build --release -p repartee

# Install to /usr/local/bin
install: release
	cp target/release/repartee /usr/local/bin/repartee
	ln -sf /usr/local/bin/repartee /usr/local/bin/reptee
