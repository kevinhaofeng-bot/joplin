# Joplin Lite sync server (docs/research/sync-client-design-v1.md).
# Build context: the repository's packages/ directory.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY app-lite-protocol app-lite-protocol
COPY app-lite-server app-lite-server
RUN cargo build --release --locked --manifest-path app-lite-server/Cargo.toml --bin app-lite-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home /data applite && mkdir -p /data && chown applite /data
COPY --from=build /src/app-lite-server/target/release/app-lite-server /usr/local/bin/app-lite-server
USER applite
VOLUME /data
EXPOSE 8787
# APP_LITE_SERVER_TOKEN must come from the environment (compose env_file);
# APP_LITE_TLS_CERT and APP_LITE_TLS_KEY there switch the server to HTTPS.
ENTRYPOINT ["app-lite-server", "--root", "/data", "--listen", "0.0.0.0:8787"]
