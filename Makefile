.PHONY: all clean wasm build release install test clippy check test-web clippy-web docs docs-check

# Full clean rebuild: clean → WASM → native release
all: clean wasm release

# Clean all build artifacts
clean:
	cargo clean
	rm -rf web-ui/dist static/web

# Build WASM frontend
wasm:
	cd web-ui && trunk build --release
	rm -rf static/web && mkdir -p static/web
	cp -r web-ui/dist/* static/web/

# Native release build (embeds WASM from static/web/)
release:
	cargo build --release

# Native dev build (no WASM rebuild)
build:
	cargo build -p repartee

# Install to /usr/local/bin
install: release
	cp target/release/repartee /usr/local/bin/repartee
	ln -sf /usr/local/bin/repartee /usr/local/bin/reptee

# Run tests (both crates — the release pre-flight runs this target, so the
# web-ui unit tests must be part of it, not a separate opt-in)
test: test-web
	cargo test -p repartee

# Run clippy (both crates — same 0-warnings policy)
clippy: clippy-web
	cargo clippy -p repartee --all-targets

# Run web-ui unit tests (host-side; DOM-free helpers only)
test-web:
	cargo test -p repartee-web

# Run clippy on the web-ui crate
clippy-web:
	cargo clippy -p repartee-web --all-targets

# Regenerate the static docs site from docs/src/content/*.md and
# docs/commands/*.md. The HTML under docs/ is what repart.ee serves and is
# checked in, but nothing builds it automatically — so editing a .md
# without running this ships a page nobody sees. Run it whenever you touch
# either source directory.
docs:
	cd docs && bun install --frozen-lockfile 2>/dev/null || (cd docs && bun install)
	cd docs && bun run build.ts

# Fail if the checked-in HTML is out of date with the markdown. Intended
# for CI; locally, `make docs` then commit the result.
docs-check: docs
	@git diff --quiet -- docs/ || { \
		echo "error: docs/ HTML is stale — run 'make docs' and commit the result:"; \
		git diff --stat -- docs/; \
		exit 1; \
	}
	@echo "docs/ HTML is up to date"
