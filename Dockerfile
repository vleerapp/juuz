FROM rust:1.99-bookworm AS build
WORKDIR /src
RUN cargo install topcoat-cli --version 0.10.0 --locked
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
COPY web ./web
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && topcoat asset bundle --release \
    && cp target/release/juuz /juuz \
    && cp -r target/release/assets /assets

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /data /music \
    && chown 1000:1000 /data /music
COPY --from=build /juuz /usr/local/bin/juuz
COPY --from=build /assets /usr/local/bin/assets
USER 1000:1000
WORKDIR /
EXPOSE 4533
VOLUME /data
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s \
    CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/4533' || exit 1
ENTRYPOINT ["juuz"]
