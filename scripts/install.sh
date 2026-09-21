#!/usr/bin/env bash
# One local deployment: lemmajevgaun in Docker. No native daemon, no second MCP.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--uninstall" ]; then
  docker compose down
  exit 0
fi
: "${TYPESAFE_API_KEY:?set TYPESAFE_API_KEY before starting lemmajevgaun}"
docker compose up -d --build
echo "lemmajevgaun MCP: http://127.0.0.1:8765/mcp"
echo "Register only this endpoint in the local client or Docker Cloudflare MCP."
