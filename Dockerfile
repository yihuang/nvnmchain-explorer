# Multi-stage build: compile once, ship a small glibc runtime.

FROM rust:1.97-slim AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
RUN cargo fetch --locked

COPY src ./src
COPY templates ./templates
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    cargo build --release --locked

# stable (trixie) glibc >= whatever rust:1.97-slim ships, so the binary runs
# regardless of which Debian release the builder image is based on.
FROM debian:stable-slim
RUN useradd --system --no-create-home --uid 10001 app
COPY --from=builder /build/target/release/nvnmchain-explorer /usr/local/bin/nvnmchain-explorer
COPY deploy/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENV DB_PATH=/data/explorer.db \
    RUST_LOG=nvnmchain_explorer=info \
    NATIVE_SYMBOL=OM
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
