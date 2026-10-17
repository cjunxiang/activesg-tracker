# syntax=docker/dockerfile:1
# Multi-stage: compile the tracker on alpine, ship it with a musl
# curl-impersonate binary (Cloudflare rejects non-browser TLS fingerprints).
FROM rust:1-alpine AS build
WORKDIR /app
RUN apk add --no-cache musl-dev gcc
COPY service/Cargo.toml service/Cargo.lock* ./
COPY service/src ./src
RUN cargo build --release

FROM alpine:3.20
RUN apk add --no-cache ca-certificates
# Railway builds on amd64; TARGETARCH is set by buildkit when available.
ARG TARGETARCH=amd64
RUN case "$TARGETARCH" in \
      arm64) CI_ARCH=aarch64 ;; \
      amd64) CI_ARCH=x86_64 ;; \
      *) echo "unsupported arch: $TARGETARCH" && exit 1 ;; \
    esac; \
    wget -qO /tmp/ci.tar.gz \
      "https://github.com/lexiforest/curl-impersonate/releases/download/v2.2.3/curl-impersonate-v2.2.3.${CI_ARCH}-linux-musl.tar.gz" \
      && tar xzf /tmp/ci.tar.gz -C /usr/local/bin curl-impersonate \
      && rm /tmp/ci.tar.gz \
      && /usr/local/bin/curl-impersonate --version >/dev/null
COPY --from=build /app/target/release/activesg-tracker /usr/local/bin/tracker
# Seed CSV must live OUTSIDE /data: the volume shadows anything baked there.
COPY data/activesg-capacities.csv /opt/seed/activesg-capacities.csv
WORKDIR /data
ENV DB_PATH=/data/capacities.db \
    SEED_CSV=/opt/seed/activesg-capacities.csv \
    IMPERSONATE_BIN=/usr/local/bin/curl-impersonate \
    IMPERSONATE_TARGET=chrome124
EXPOSE 3000
CMD ["tracker"]
