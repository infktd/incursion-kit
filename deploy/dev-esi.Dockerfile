# syntax=docker/dockerfile:1
# dev-esi, the fake EVE SSO and ESI. DEVELOPMENT ONLY (see compose.dev-esi.yaml).
# Build from the repo root:  docker build -f deploy/dev-esi.Dockerfile .

FROM rust:1.99-slim-trixie AS builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin dev-esi \
    && cp target/release/dev-esi /usr/local/bin/dev-esi

FROM debian:trixie-slim
RUN useradd --system --uid 10001 dev-esi && mkdir /state && chown dev-esi /state
COPY --from=builder /usr/local/bin/dev-esi /usr/local/bin/dev-esi
USER dev-esi
EXPOSE 8090
ENTRYPOINT ["dev-esi"]
CMD ["--bind", "0.0.0.0:8090"]
