# Dedan nameserver image. Build context is the repository root:
#   docker build -f crates/resolver/Dockerfile -t dedan-resolver:local .
# cargo-chef caches the dependency build, so a source change rebuilds only the crates.
FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p dedan-resolver

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/dedan-resolver /usr/local/bin/dedan-resolver
CMD ["dedan-resolver"]
