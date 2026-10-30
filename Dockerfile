# James – voice bot for TeamSpeak 3/6.
# Build: docker build -t james .   Run: see docker-compose.yml and docs/installation.md

# ---- Rust binary --------------------------------------------------------------------------
FROM rust:1-bookworm AS rust
RUN apt-get update && apt-get install -y --no-install-recommends cmake libopus-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p james && cp target/release/james /james

# ---- Web interface ------------------------------------------------------------------------
FROM node:24-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

# ---- Runtime ------------------------------------------------------------------------------
FROM node:24-bookworm-slim
ARG DENO_VERSION=v2.9.8
ARG DENO_SHA256=e890975bc2549807bbcc0b584ffafda72cc7c93349dd99c80483dd784f6c4609
ARG PIPER_VERSION=1.8.0
# Must match the PO token server (docker-compose.yml).
ARG BGUTIL_VERSION=2.0.2
ARG VOICES="de_DE-thorsten-medium en_US-lessac-medium"
ARG FASTER_WHISPER_VERSION=1.2.1
# Speech recognition model (Whisper base, multilingual), pinned to a revision and checked.
ARG WHISPER_MODEL=Systran/faster-whisper-base
ARG WHISPER_REVISION=ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66
RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg libopus0 python3 python3-venv ca-certificates curl unzip tini \
    && rm -rf /var/lib/apt/lists/*
# yt-dlp (with the bgutil PO token plugin) and Piper (with faster-whisper for speech
# recognition, sharing its libraries) in their own virtual environments.
# yt-dlp is the newest release at build time; the published image is rebuilt weekly.
RUN python3 -m venv /opt/yt-dlp \
    && /opt/yt-dlp/bin/pip install --no-cache-dir -U "yt-dlp[default]" "bgutil-ytdlp-pot-provider==${BGUTIL_VERSION}" \
    && ln -s /opt/yt-dlp/bin/yt-dlp /usr/local/bin/yt-dlp \
    && python3 -m venv /opt/piper \
    && /opt/piper/bin/pip install --no-cache-dir "piper-tts==${PIPER_VERSION}" "faster-whisper==${FASTER_WHISPER_VERSION}" \
    && mkdir -p /opt/james/voices \
    && /opt/piper/bin/python -m piper.download_voices --download-dir /opt/james/voices $VOICES
RUN mkdir -p /opt/james/stt/whisper-base && cd /opt/james/stt/whisper-base \
    && for f in config.json model.bin tokenizer.json vocabulary.txt; do \
        curl -fsSL -o "$f" "https://huggingface.co/${WHISPER_MODEL}/resolve/${WHISPER_REVISION}/$f" || exit 1; \
    done \
    && printf '%s\n' \
        "56a6d8110d311f19c8f0471e562832c7527f146b567275bfca59fcf7c184da9a  config.json" \
        "d01c3014881c9c6f3133c182f3d2887eb6ca1c789a7538c5c007196857a0a6a9  model.bin" \
        "fb7b63191e9bb045082c79fd742a3106a12c99513ab30df4a0d47fa6cb6fd0ab  tokenizer.json" \
        "34ce3fe1c5041027b3f8d42912270993f986dbc4bb34cf27f951e34a1e453913  vocabulary.txt" \
        | sha256sum -c -
# Deno lets yt-dlp solve YouTube's JavaScript challenges.
RUN curl -fsSL -o /tmp/deno.zip \
        "https://github.com/denoland/deno/releases/download/${DENO_VERSION}/deno-x86_64-unknown-linux-gnu.zip" \
    && echo "${DENO_SHA256}  /tmp/deno.zip" | sha256sum -c - \
    && unzip -q /tmp/deno.zip -d /usr/local/bin && rm /tmp/deno.zip && deno --version

COPY --from=rust /james /opt/james/bin/james
COPY --from=web /web/build /opt/james/web
COPY plugin-host/package.json /opt/james/plugin-host/package.json
COPY plugin-host/src /opt/james/plugin-host/src
COPY docker/entrypoint.sh /opt/james/bin/entrypoint.sh

# Everything outside /data belongs to root: the bot cannot change its own tools.
RUN useradd --create-home --uid 1001 james && mkdir -p /data && chown james:james /data
# The entrypoint starts as root only to hand /data to the james user (a bind mount created by
# Docker belongs to root), then runs James as james.
WORKDIR /data
VOLUME /data
EXPOSE 8087

ENV JAMES_CONFIG=/data/james.toml \
    JAMES_DATA_DIR=/data \
    JAMES_HTTP__BIND=0.0.0.0:8087 \
    JAMES_HTTP__WEB_ROOT=/opt/james/web \
    JAMES_PLUGINS__HOST=/opt/james/plugin-host/src/main.ts \
    JAMES_TTS__PIPER__PYTHON=/opt/piper/bin/python \
    JAMES_TTS__PIPER__VOICES_DIR=/opt/james/voices \
    JAMES_STT__WHISPER__PYTHON=/opt/piper/bin/python \
    JAMES_STT__WHISPER__MODEL=/opt/james/stt/whisper-base \
    JAMES_LOG__FORMAT=json

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s \
    CMD curl -fsS http://127.0.0.1:8087/api/v1/health || exit 1
ENTRYPOINT ["/usr/bin/tini", "--", "/opt/james/bin/entrypoint.sh"]
