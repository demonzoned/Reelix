# syntax=docker/dockerfile:1
# Multi-stage build: cargo-chef caches dependency layers for both the server and the wasm UI.

FROM rust:1-bookworm@sha256:9181b56c2a21ab1d5e73cffe04fc762426d30aa7fac3c91141dd033f49a7fc8b AS chef
RUN cargo install --locked cargo-chef
WORKDIR /src

# ---- Dependency recipe -------------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---- Server binary -----------------------------------------------------------
FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json --package reelix-server
COPY . .
RUN cargo build --release --locked --package reelix-server --bin reelix \
    && cp target/release/reelix /reelix

# ---- Web UI (Leptos CSR via Trunk) -------------------------------------------
FROM chef AS web
RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked trunk
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json \
    --target wasm32-unknown-unknown --package reelix-web
COPY . .
# Onboarding: the set-up wizard by default; --build-arg REELIX_WEB_FEATURES=classic-setup builds
# Reelix's classic onboarding instead.
ARG REELIX_WEB_FEATURES=""
RUN cd crates/reelix-web && trunk build --release ${REELIX_WEB_FEATURES:+--features "$REELIX_WEB_FEATURES"}

# ---- Runtime -----------------------------------------------------------------
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS runtime
ARG TARGETARCH
# util-linux (setpriv) is part of the base image; tzdata makes TZ work.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# yt-dlp handles YouTube downloads and uses Deno for YouTube's JavaScript. Both must
# track YouTube's changes: bump these pins (versions + SHA256 from the releases'
# SHA2-256SUMS / .sha256sum assets) regularly, or override them with --build-arg.
ARG YTDLP_VERSION=2026.10.26
ARG YTDLP_SHA256_AMD64=767ba9dce7052aa196f8b12652f433d76de128ba168966fa84b6394d5e6c6795
ARG YTDLP_SHA256_ARM64=e87e9533e89fe5889abaa21f5114dfe7756d6aa6219b83551add04d48b18e483
ARG DENO_VERSION=2.9.8
ARG DENO_SHA256_AMD64=e890975bc2549807bbcc0b584ffafda72cc7c93349dd99c80483dd784f6c4609
ARG DENO_SHA256_ARM64=d49596c20b9788cf8f5d8ddf644103d4bde56ed239753f0bdb92ba9361d51905
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) ytdlp=yt-dlp_linux; deno=x86_64-unknown-linux-gnu; \
             ytdlp_sha="${YTDLP_SHA256_AMD64}"; deno_sha="${DENO_SHA256_AMD64}" ;; \
      arm64) ytdlp=yt-dlp_linux_aarch64; deno=aarch64-unknown-linux-gnu; \
             ytdlp_sha="${YTDLP_SHA256_ARM64}"; deno_sha="${DENO_SHA256_ARM64}" ;; \
      *) echo "unsupported architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    apt-get update; \
    apt-get install -y --no-install-recommends curl unzip; \
    curl -fsSL --retry 3 -o /usr/local/bin/yt-dlp "https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/${ytdlp}"; \
    curl -fsSL --retry 3 -o /tmp/deno.zip "https://github.com/denoland/deno/releases/download/v${DENO_VERSION}/deno-${deno}.zip"; \
    printf '%s  %s\n' "${ytdlp_sha}" /usr/local/bin/yt-dlp "${deno_sha}" /tmp/deno.zip | sha256sum -c -; \
    unzip -q /tmp/deno.zip -d /usr/local/bin; \
    rm /tmp/deno.zip; \
    chmod 0755 /usr/local/bin/yt-dlp /usr/local/bin/deno; \
    apt-get purge -y curl unzip; \
    apt-get autoremove -y; \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /reelix /app/reelix
COPY --from=web /src/crates/reelix-web/dist /app/web
COPY docker/entrypoint.sh /app/entrypoint.sh
RUN chmod 0755 /app/entrypoint.sh /app/reelix

# Two glibc malloc arenas instead of one per thread: about a third less memory, no measurable cost.
ENV REELIX_DATA_DIR=/config \
    MALLOC_ARENA_MAX=2 \
    REELIX_WEB_DIR=/app/web \
    REELIX_DOCKER=1 \
    DENO_DIR=/tmp/deno \
    PUID=1000 \
    PGID=1000 \
    UMASK=022

EXPOSE 7700
VOLUME /config
WORKDIR /app
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=60s --timeout=10s --start-period=20s --retries=3 CMD ["/app/reelix", "healthcheck"]
ENTRYPOINT ["/app/entrypoint.sh"]
