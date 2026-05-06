// rustwebtest — wgpu + rodio + web-thread test harness, targeting wasm32 with
// SharedArrayBuffer / atomics enabled.
//
// Layout:
//   atlas.rs     — generated sprite atlas + PNG bytes
//   sim.rs       — sprite physics (lives on workers)
//   workers.rs   — spawn web-thread workers + crossbeam channels
//   audio.rs     — rodio BGM + synth pling
//   render.rs    — wgpu sprite renderer + winit event loop

#![allow(clippy::needless_range_loop)]

pub mod atlas;
pub mod audio;
pub mod render;
pub mod sim;
pub mod workers;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// BGM bytes — included at compile time.
///
/// NOTE: source file is staticeulogy.opus, but symphonia 0.5.x (rodio's
/// decoder backend) doesn't ship an Opus codec, so we transcode to Ogg/Vorbis
/// for runtime playback. Re-run the ffmpeg command in the README if the
/// source ever changes.
pub const BGM_OGG: &[u8] = include_bytes!("../assets/staticeulogy.ogg");

/// Wasm entrypoint. The `start` attribute tells wasm-bindgen to invoke this
/// automatically once the module is instantiated.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn wasm_main() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    log::info!("rustwebtest starting");

    // Drive winit's async setup. winit on web must own the event loop on the
    // main thread, so we kick it off here and never return.
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = render::run().await {
            log::error!("render::run failed: {e:?}");
        }
    });
    Ok(())
}

/// Native entrypoint — useful for development without rebuilding for wasm.
#[cfg(not(target_arch = "wasm32"))]
pub fn native_main() {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    pollster::block_on(async {
        if let Err(e) = render::run().await {
            log::error!("render::run failed: {e:?}");
        }
    });
}
