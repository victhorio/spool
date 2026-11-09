FROM oven/bun:1.4.3 AS web
WORKDIR /build
COPY package.json bun.lock tsconfig.json ./
COPY plugin/package.json ./plugin/package.json
COPY web/package.json ./web/package.json
RUN bun install --frozen-lockfile
COPY web ./web
RUN bun run web:build

FROM rust:1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY server ./server
RUN cargo build --locked --release --package spool-server

FROM tailscale/tailscale:v1.104.4@sha256:ed6480b33c6eb67c9cbd203ed42b9fb57e361986725b6856def71b7a3d9e276c AS tailscale

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl gosu jq tini \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -m 0700 -o 10001 -g 10001 /run/spool /var/lib/tailscale
COPY --from=build /build/target/release/spool-server /usr/local/bin/spool-server
COPY --from=web /build/web/dist /opt/spool/web
COPY --from=tailscale /usr/local/bin/containerboot /usr/local/bin/tailscaled /usr/local/bin/tailscale /usr/local/bin/
COPY --chmod=755 deploy/reader-start deploy/reader-health /usr/local/bin/
COPY deploy/reader-serve.json /etc/spool/serve.json
ENV PORT=9002 TS_HOSTNAME=spool-archive
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=15s --timeout=10s --start-period=90s --retries=3 CMD ["gosu", "10001:10001", "/usr/local/bin/reader-health"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/reader-start"]
