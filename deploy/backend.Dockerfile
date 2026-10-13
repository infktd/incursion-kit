# syntax=docker/dockerfile:1
# Waitlist backend. Build from the repo root:  docker build -f deploy/backend.Dockerfile .

FROM rust:1.99-slim-trixie AS chef
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
# Dependencies only: cached until Cargo.toml / Cargo.lock change.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json --bin waitlist
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked --bin waitlist \
    && cp target/release/waitlist /usr/local/bin/waitlist

FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /app waitlist \
    && mkdir -p /app/data /app/sde && chown -R waitlist /app
COPY --from=builder /usr/local/bin/waitlist /usr/local/bin/waitlist
WORKDIR /app
USER waitlist
ENV WAITLIST_CONFIG=/app/config.toml \
    WAITLIST__DATA__DIR=/app/data \
    WAITLIST__DATA__SDE=/app/sde/sde.sqlite
VOLUME ["/app/data", "/app/sde"]
EXPOSE 5000
HEALTHCHECK --interval=30s --timeout=5s --start-period=600s \
    CMD ["waitlist", "healthcheck"]
ENTRYPOINT ["waitlist"]
CMD ["serve"]
