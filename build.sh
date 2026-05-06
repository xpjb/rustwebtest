#!/usr/bin/env bash
# Build for wasm32-unknown-unknown with thread support, then run wasm-bindgen.
#
# Output goes to ./pkg/, served alongside index.html.

set -euo pipefail

PROFILE="${PROFILE:-release}"
PROFILE_DIR="$PROFILE"
[ "$PROFILE" = "dev" ] && PROFILE_DIR="debug"

# Atomics + bulk-memory + mutable-globals enable wasm threads. The two
# --link-arg flags also tell lld to *emit shared memory* with a fixed max
# size — without these the resulting WebAssembly.Memory is unshared, and
# `postMessage`ing it to a Worker fails with DataCloneError.
export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
  -C link-arg=--shared-memory \
  -C link-arg=--import-memory \
  -C link-arg=--max-memory=4294967296 \
  -C link-arg=--export=__wasm_init_tls \
  -C link-arg=--export=__tls_size \
  -C link-arg=--export=__tls_align \
  -C link-arg=--export=__tls_base"

CARGO_FLAGS=()
[ "$PROFILE" = "release" ] && CARGO_FLAGS+=("--release")

cargo +nightly build \
  --target wasm32-unknown-unknown \
  -Z build-std=panic_abort,std \
  --lib \
  "${CARGO_FLAGS[@]}"

WASM="target/wasm32-unknown-unknown/${PROFILE_DIR}/rustwebtest.wasm"

mkdir -p pkg
# --split-linked-modules emits the worker entrypoint as a separate JS file,
# which wasm_thread / wasm-bindgen's thread shim load via `new Worker(url)`.
wasm-bindgen \
  --target web \
  --split-linked-modules \
  --out-dir pkg \
  --out-name rustwebtest \
  "$WASM"

echo
echo "build OK -> pkg/"
echo "now run:  ./serve.sh   (sets COOP/COEP for SharedArrayBuffer)"
echo "itch.io:  ./package-itch.sh   (pkg/rustwebtest-itch-YYYYMMDDTHHMMSSZ.zip, UTC)"
