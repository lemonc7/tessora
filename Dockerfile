# syntax=docker/dockerfile:1.7

FROM rust:1.98-bookworm AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        build-essential \
        musl-tools \
    && rustup target add x86_64-unknown-linux-musl \
    && rm -rf /var/lib/apt/lists/*

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install topcoat-cli --version 0.10.0 --locked

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    export CARGO_BUILD_TARGET=x86_64-unknown-linux-musl \
           CC_x86_64_unknown_linux_musl=musl-gcc \
           CARGO_TARGET_X86_64_UNKNOWN_LINUX_LINKER=musl-gcc \
    && cargo build --release --locked \
    && topcoat asset bundle --release --bin tessora --out /out/assets \
    && install -Dm755 target/x86_64-unknown-linux-musl/release/tessora /out/tessora

FROM alpine:3.23 AS runtime

RUN apk add --no-cache ca-certificates curl ffmpeg \
    && addgroup -g 1000 tessora \
    && adduser -D -H -u 1000 -G tessora tessora \
    && mkdir -p /app/assets /data/downloads \
    && chown -R tessora:tessora /app /data

WORKDIR /app

COPY --from=builder --chown=tessora:tessora /out/tessora /app/tessora
COPY --from=builder --chown=tessora:tessora /out/assets /app/assets

ENV HOST=0.0.0.0 \
    PORT=3000 \
    TZ=Asia/Shanghai \
    RUST_LOG=info \
    DOWNLOAD_ROOT=/data/downloads \
    TEMP_ROOT=/data/downloads \
    FFMPEG_BIN=/usr/bin/ffmpeg \
    MAX_CONCURRENT_DOWNLOADS=2 \
    CONCURRENT_FRAGMENTS=8 \
    MEDIA_SETTLE_SECONDS=60

USER tessora

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:3000/ >/dev/null || exit 1

STOPSIGNAL SIGTERM

ENTRYPOINT ["/app/tessora"]
