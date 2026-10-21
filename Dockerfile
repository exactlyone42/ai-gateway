FROM rust:1.87-slim AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release && rm -rf src target/release/.fingerprint/agent-pool-gateway-*
COPY src/ src/
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/agent-pool-gateway /usr/local/bin/
ENV LISTEN_ADDRESS=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["agent-pool-gateway"]
