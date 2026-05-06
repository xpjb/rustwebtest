#!/usr/bin/env bash
# Start a local server with the COOP/COEP headers needed for SharedArrayBuffer.
exec node serve.mjs "${1:-8080}"
