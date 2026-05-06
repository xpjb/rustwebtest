# rustwebtest

**Itch.io:** [https://skretble.itch.io/rust-web-demo](https://skretble.itch.io/rust-web-demo)

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

wasm-bindgen --target web --split-linked-modules --out-dir pkg --out-name rustwebtest \
    target/wasm32-unknown-unknown/release/rustwebtest.wasm
```

## Itch.io (HTML5)

itch.io **does** send the COOP/COEP headers once you opt in — unlike GitHub Pages — so wasm threads work there.

1. `./package-itch.sh` — runs `./build.sh`, then writes `pkg/rustwebtest-itch-YYYYMMDD.zip` (local date, same calendar day overwrites). Uses `7z` (`7z a -tzip`). Requires [7-Zip](https://www.7-zip.org/) on `PATH`. Override path with `ZIP_NAME=…`.
2. On itch: **Edit game** → **Uploads** → upload the zip as an HTML5 project (or replace an existing HTML build).
3. Set **This file will be played in the browser** to `index.html` if it is not picked automatically.
4. In the project’s **Dangerous / advanced** (or embed) settings, enable **SharedArrayBuffer support**. Without this, the page is not `crossOriginIsolated` and workers will not start.

Re-pack only (reuse current `pkg/`): `SKIP_BUILD=1 ./package-itch.sh`

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
| [package-itch.sh](package-itch.sh) | Build + zip for itch.io HTML5 upload |
| [serve.mjs](serve.mjs) | Static server with COOP/COEP headers |

## Threading model

- 4 workers, each owning 12 sprites.
- Each worker simulates physics at 60 Hz, packs vertex data, and pushes a `Snapshot` through a bounded crossbeam channel.
- The main thread **never blocks** on `recv()` — it only `try_recv()`s, draining each channel down to the latest snapshot, and renders whatever it has.
- Cross-partition overlap detection runs on the main thread against the merged snapshots; new overlaps trigger a synthesized pling.

