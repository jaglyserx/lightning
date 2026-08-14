FROM rust:1.93.1-bookworm AS chef

WORKDIR /app

RUN cargo install cargo-chef --locked

FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher

COPY --from=planner /app/recipe.json recipe.json

RUN cargo chef cook --release --recipe-path recipe.json

FROM chef AS builder

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src

ENV SQLX_OFFLINE=true

COPY --from=cacher /app/target ./target
COPY --from=cacher /usr/local/cargo /usr/local/cargo

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/control-plane /usr/local/bin/control-plane
COPY --from=builder /app/target/release/migrate /usr/local/bin/migrate

EXPOSE 8000

CMD ["control-plane"]
