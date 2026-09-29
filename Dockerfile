# syntax=docker/dockerfile:1
FROM rust:1.90-slim-bookworm AS build
WORKDIR /src
# Capa de caché de deps: manifiestos + stubs. Tocar src/ no la invalida, así
# que ureq/serde_json y las deps no se recompilan en cada cambio de código.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin tests examples \
    && echo 'fn main() {}' > src/bin/lemmalog-mcp.rs \
    && echo 'fn main() {}' > src/bin/revise.rs \
    && echo 'fn main() {}' > src/bin/lemmalog-cli.rs \
    && echo 'fn main() {}' > src/bin/lemmalog.rs \
    && echo 'fn main() {}' > src/bin/lemmalog-bench.rs \
    && echo '' > src/lib.rs \
    && echo '' > tests/jev_test.rs \
    && echo '' > examples/jev_align.rs \
    && echo '' > examples/lmstudio.rs \
    && echo '' > examples/llm_rules.rs \
    && echo '' > examples/longmemeval.rs \
    && cargo build --release --features gate --bin lemmajevgaun-mcp --bin lemmajevgaun-revise
COPY . .
RUN touch src/lib.rs src/bin/lemmalog-mcp.rs src/bin/revise.rs \
    && cargo build --release --features gate --bin lemmajevgaun-mcp --bin lemmajevgaun-revise

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
