# Dedan services image: the nameserver, the auditor app and the aggregator/countersigner, one binary each.
# Build context is the repository root:
#   docker build -f crates/Dockerfile -t dedan-services:local .
# cargo-chef caches the dependency build, so a source change rebuilds only the crates. The runtime carries
# iproute2 for the simulated WAN delays of the auditor containers (deploy/wan/wan.sh).
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
RUN cargo build --release --workspace --bins

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl iproute2 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/dedan-resolver /app/target/release/dedan-auditor /app/target/release/dedan-aggregator /usr/local/bin/
CMD ["dedan-resolver"]
