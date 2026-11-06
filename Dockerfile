# syntax=docker/dockerfile:1
# Container image for the `tpt-kinetix` CLI. The binary is pure Rust with no
# system libraries beyond libc, so the runtime image only adds CA certificates
# (for `probe https://...`). Build: docker build -t tpt-kinetix .

FROM rust:1.82-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p tpt-kinetix-cli \
 && cp target/release/tpt-kinetix /tpt-kinetix

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home kinetix
COPY --from=build /tpt-kinetix /usr/local/bin/tpt-kinetix
USER 10001
WORKDIR /data
EXPOSE 8080
ENTRYPOINT ["tpt-kinetix"]
CMD ["--help"]
