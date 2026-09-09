# Two stages: build with the toolchain, ship without it.

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# The migration runner reads this directory at compile time, so it has to be
# present in the build context even while it is empty.
COPY migrations ./migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim
# ca-certificates for outbound TLS to providers; curl is the container health
# check and nothing else.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# Never run the gateway as root: it holds provider credentials.
RUN useradd --system --create-home --uid 10001 gateway
USER gateway
COPY --from=build /src/target/release/agent-pool-gateway /usr/local/bin/
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/agent-pool-gateway"]
