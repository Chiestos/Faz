FROM rust:1.85-slim AS builder
WORKDIR /app

# Copy only the relay crate (standalone, no workspace deps)
COPY Cargo.toml ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/faz-relay /usr/local/bin/faz-relay
ENV RUST_LOG=kaku_relay=info
EXPOSE 8080
CMD ["faz-relay"]
