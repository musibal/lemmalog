# syntax=docker/dockerfile:1
FROM rust:1.90-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --features gate --bins

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home lemmajevgaun \
    && mkdir -p /data && chown lemmajevgaun:lemmajevgaun /data
COPY --from=build /src/target/release/lemmajevgaun-mcp /usr/local/bin/lemmajevgaun-mcp
COPY --from=build /src/target/release/lemmajevgaun-revise /usr/local/bin/lemmajevgaun-revise
USER lemmajevgaun
ENV HOME=/data \
    LEMMALOG_MCP_HTTP=0.0.0.0:8765 \
    LEMMALOG_MCP_PATH=/data/lemmalog.snapshot \
    LEMMALOG_GATE_DIR=/data/gates \
    LEMMALOG_GATE_CALIBRATION=/data/gate-calibration.jsonl \
    LEMMALOG_MCP_LOG_BODIES=1
VOLUME ["/data"]
EXPOSE 8765
ENTRYPOINT ["/usr/local/bin/lemmajevgaun-mcp"]
