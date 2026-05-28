# syntax=docker/dockerfile:1.7

# Multi-stage build for the unified think-and-ship MCP server.
#
# Build:    docker build -f docs/deploy/Dockerfile -t think-and-ship:0.2.0 .
# Run:      docker run --rm -p 8080:8080 -v ts-data:/data think-and-ship:0.2.0
# Verify:   curl -i -X POST http://localhost:8080/mcp \
#               -H 'Accept: application/json, text/event-stream' \
#               -H 'Content-Type: application/json' \
#               -d '{"jsonrpc":"2.0","id":1,"method":"initialize",
#                    "params":{"protocolVersion":"2025-06-18",
#                              "capabilities":{},
#                              "clientInfo":{"name":"curl","version":"1"}}}'
#
# Override CORS / host validation at run time:
#   docker run -e THINK_AND_SHIP_HTTP_ALLOWED_HOSTS=mcp.example.com,localhost \
#              -e THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS=https://app.example.com \
#              -p 8080:8080 think-and-ship:0.2.0

ARG RUST_VERSION=1.88
ARG DEBIAN_VERSION=bookworm

# ─── Stage 1: build ──────────────────────────────────────────────
FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS builder

WORKDIR /src
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

# Copy the whole workspace (the .dockerignore at the repo root keeps
# target/, .git, node_modules, etc. out of the build context).
COPY . .

RUN cargo build --release -p think-and-ship --locked

# ─── Stage 2: runtime ────────────────────────────────────────────
FROM debian:${DEBIAN_VERSION}-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --home-dir /data --shell /usr/sbin/nologin think \
 && mkdir -p /data \
 && chown -R think:think /data

COPY --from=builder /src/target/release/think-and-ship /usr/local/bin/think-and-ship

USER think
WORKDIR /data

# Persistence on by default; data dir overridable.
ENV THINK_AND_SHIP_PERSIST=true \
    THINK_AND_SHIP_DATA_DIR=/data \
    RUST_LOG=info

EXPOSE 8080
VOLUME ["/data"]

# Bind 0.0.0.0:8080 so the port mapping works. If the operator overrides
# THINK_AND_SHIP_HTTP_ALLOWED_HOSTS, they should include any hostname they
# expect inbound requests to use (the default list — localhost / 127.0.0.1
# / ::1 — gets *replaced*, not extended).
ENTRYPOINT ["think-and-ship"]
CMD ["serve", "--http", "0.0.0.0:8080"]
