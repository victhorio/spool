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

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/spool-server /usr/local/bin/spool-server
COPY --from=web /build/web/dist /opt/spool/web
USER 10001:10001
ENV BIND_ADDR=0.0.0.0 PORT=8080 ASSETS_DIR=/opt/spool/web
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/spool-server"]
