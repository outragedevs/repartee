FROM rust:latest@sha256:a8a5f0a1e5fe7dfe1d352591e4a1c7dd2c08fd70475cae872cf3458ba0df0546
RUN apt-get update && apt-get install -y --no-install-recommends libchafa-dev libglib2.0-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY . .
RUN make build && app_binary="$(sed -n 's/^pub const APP_NAME: \&str = "\([^"]*\)";$/\1/p' src/constants.rs)" && install -s "target/debug/$app_binary" "/usr/local/bin/$app_binary" && rm -rf target
