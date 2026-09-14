FROM rust:1.95-slim-trixie AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        curl \
        musl-tools \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown x86_64-unknown-linux-musl

ENV CC_x86_64_unknown_linux_musl=musl-gcc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_LINKER=musl-gcc

RUN curl -L --proto '=https' --tlsv1.2 -sSf \
    https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh \
    | bash \
    && cargo binstall dioxus-cli@0.7.10 -y --force

COPY . .

RUN dx bundle --package tessora --release \
    @client --platform web \
    @server --platform server --target x86_64-unknown-linux-musl

FROM alpine:3.23 AS runtime

WORKDIR /app

ENV IP=0.0.0.0 \
    DOWNLOAD_ROOT=/app/downloads \
    DATABASE_PATH=/app/data/tessora.db \
    RUST_LOG=info,dioxus_server=warn,hyper_util=warn

RUN apk add --no-cache ca-certificates ffmpeg \
    && addgroup -g 1000 tessora \
    && adduser -D -H -u 1000 -G tessora tessora \
    && mkdir -p /app/downloads /app/data \
    && chown -R tessora:tessora /app/downloads /app/data

COPY --from=builder --chown=tessora:tessora /app/dist/ /app/

USER tessora
EXPOSE 8080
ENTRYPOINT ["/app/server"]
