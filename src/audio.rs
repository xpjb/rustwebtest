// Audio: BGM via rodio + a synthesized exponentially-decaying pling for
// overlap events.
//
// On web, rodio's cpal backend needs the `wasm-bindgen` feature so it can
// route through Web Audio. Browsers also forbid starting audio before a user
// gesture; the page wires up a click handler that calls `try_init`.
//
// `OutputStream` (and the cpal Stream it wraps) is `!Send` on most backends,
// so we keep audio state in a thread_local accessed only from the main thread.

use std::cell::RefCell;
use std::io::Cursor;
use std::time::Duration;

use rodio::source::Source;
use rodio::{OutputStream, OutputStreamHandle, Sink};

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

pub struct Audio {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    _bgm_sink: Sink,
}

impl Audio {
    pub fn new(bgm_bytes: &'static [u8]) -> Result<Self, BoxErr> {
        let (_stream, handle) = OutputStream::try_default()?;
        let bgm_sink = Sink::try_new(&handle)?;
        let cursor = Cursor::new(bgm_bytes);
        let decoder = rodio::Decoder::new(cursor)?;
        log::info!(
            "bgm decoder: channels={}, sample_rate={}, total={:?}",
            decoder.channels(),
            decoder.sample_rate(),
            decoder.total_duration(),
        );
        bgm_sink.append(decoder.repeat_infinite());
        bgm_sink.set_volume(0.5);
        bgm_sink.play();

        Ok(Audio {
            _stream,
            handle,
            _bgm_sink: bgm_sink,
        })
    }

    pub fn pling(&self, freq: f32) {
        let sink = match Sink::try_new(&self.handle) {
            Ok(s) => s,
            Err(_) => return,
        };
        sink.set_volume(0.08);
        sink.append(Pling::new(freq, 0.25));
        sink.detach();
    }
}

/// rodio Source synthesizing a single exponentially-decaying sine pling.
pub struct Pling {
    sample_rate: u32,
    sample_idx: u32,
    total_samples: u32,
    freq: f32,
    decay: f32,
}

impl Pling {
    pub fn new(freq: f32, duration_secs: f32) -> Self {
        let sample_rate = 44_100;
        let total_samples = (sample_rate as f32 * duration_secs) as u32;
        let decay = -(0.01_f32.ln()) / duration_secs;
        Pling {
            sample_rate,
            sample_idx: 0,
            total_samples,
            freq,
            decay,
        }
    }
}

impl Iterator for Pling {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.sample_idx >= self.total_samples {
            return None;
        }
        let t = self.sample_idx as f32 / self.sample_rate as f32;
        let env = (-self.decay * t).exp();
        let phase = std::f32::consts::TAU * self.freq * t;
        self.sample_idx += 1;
        Some(phase.sin() * env)
    }
}

impl Source for Pling {
    fn current_frame_len(&self) -> Option<usize> {
        Some((self.total_samples - self.sample_idx) as usize)
    }
    fn channels(&self) -> u16 {
        1
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f32(
            self.total_samples as f32 / self.sample_rate as f32,
        ))
    }
}

thread_local! {
    static AUDIO: RefCell<Option<Audio>> = const { RefCell::new(None) };
}

pub fn try_init(bgm: &'static [u8]) {
    AUDIO.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_some() {
            return;
        }
        match Audio::new(bgm) {
            Ok(a) => {
                log::info!("audio initialized");
                *slot = Some(a);
            }
            Err(e) => log::warn!("audio init failed (will retry on next gesture): {e}"),
        }
    });
}

pub fn pling(freq: f32) {
    AUDIO.with(|cell| {
        if let Some(audio) = cell.borrow().as_ref() {
            audio.pling(freq);
        }
    });
}

pub fn is_ready() -> bool {
    AUDIO.with(|cell| cell.borrow().is_some())
}
