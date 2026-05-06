# rustwebtest

Reference Rust → wasm32 app exercising:

- **Rendering**: `wgpu` via `winit` (auto-targets WebGL2 / WebGPU on the web)
- **Audio**: `rodio` (background `.opus` track + a synthed exponentially-decaying pling on sprite overlap)
- **Threading**: `web-thread` (drop-in for `std::thread`) + `crossbeam-channel` MPMC queues
- **Asset bundling**: a build-time atlas packer in `build.rs` and `include_bytes!()` for both atlas PNG and BGM `.opus`

Designed to run on hosts with strict Cross-Origin headers (`COOP: same-origin`, `COEP: require-corp`) — i.e. itch.io with the SharedArrayBuffer option enabled.

## Build

```sh
./build.sh           # release build -> pkg/
PROFILE=dev ./build.sh
```

The build script runs:

```sh
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals" \
cargo +nightly build \
    --target wasm32-unknown-unknown \
    -Z build-std=panic_abort,std \
    --release --lib

wasm-bindgen --target web --out-dir pkg --out-name rustwebtest \
    target/wasm32-unknown-unknown/release/rustwebtest.wasm
```

## Run locally

```sh
./serve.sh           # node serve.mjs on :8080 with COOP/COEP set
```

Open <http://localhost:8080>.

## Layout

| File | Role |
|------|------|
| [build.rs](build.rs) | Pack everything in `assets/*.{png,webp}` into a single PNG atlas at compile time; emits `atlas_data.rs` |
| [src/atlas.rs](src/atlas.rs) | `include!`s the generated metadata + atlas PNG bytes |
| [src/sim.rs](src/sim.rs) | Sprite types, physics step, quad vertex builder |
| [src/workers.rs](src/workers.rs) | `web_thread::spawn` workers, crossbeam channels, overlap detection |
| [src/audio.rs](src/audio.rs) | rodio BGM playback + synth `Pling` `Source` |
| [src/render.rs](src/render.rs) | wgpu sprite renderer + winit `ApplicationHandler` |
| [src/lib.rs](src/lib.rs) | wasm-bindgen `start` entrypoint |
| [index.html](index.html) | Host page that loads `pkg/rustwebtest.js` |
| [serve.mjs](serve.mjs) | Static server with COOP/COEP headers |

## Threading model

- 4 workers, each owning 12 sprites.
- Each worker simulates physics at 60 Hz, packs vertex data, and pushes a `Snapshot` through a bounded crossbeam channel.
- The main thread **never blocks** on `recv()` — it only `try_recv()`s, draining each channel down to the latest snapshot, and renders whatever it has.
- Cross-partition overlap detection runs on the main thread against the merged snapshots; new overlaps trigger a synthesized pling.

## Itch.io notes

Upload the contents of this directory (after `./build.sh`) — `index.html`, `pkg/`, plus anything the page references. Enable the **"SharedArrayBuffer support"** option on the project page. itch.io then serves the COOP/COEP headers; without them, `crossOriginIsolated` is false and `web_thread::spawn` cannot create real workers.
