# Build both artifacts, then ship without either toolchain.

FROM node:22-bookworm-slim AS web-build
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# The migration runner reads this directory at compile time, so it has to be
# present in the build context even while it is empty.
COPY migrations ./migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim
WORKDIR /app
# ca-certificates for outbound TLS to providers; curl is the container health
# check and nothing else.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# Never run the gateway as root: it holds provider credentials.
RUN useradd --system --create-home --uid 10001 gateway
# Create the spool mount point here, owned by that user. An empty named volume
# inherits the image's ownership for its mount point; without this the volume
# arrives owned by root and the gateway cannot write its usage ledger.
RUN mkdir -p /var/lib/agent-pool-gateway/ledger-spool \
    && chown -R gateway:gateway /var/lib/agent-pool-gateway
USER gateway
COPY --from=build /src/target/release/agent-pool-gateway /usr/local/bin/
COPY --from=web-build /web/dist ./web/dist
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/agent-pool-gateway"]
