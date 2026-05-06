// Audio: BGM via rodio + a synthesized exponentially-decaying pling for
// overlap events.
//
// SFX uses one submix: [`rodio::dynamic_mixer`] sums all pling voices, fed by
// a single [`Sink`] on the same [`OutputStreamHandle`] as BGM. That avoids a
// fresh [`Sink`] + detach per event; overlap still adds in float (no automatic
// limiting), so heavy polyphony may need a lower master SFX volume.
//
// On web, rodio's cpal backend needs the `wasm-bindgen` feature so it can
// route through Web Audio. Browsers also forbid starting audio before a user
// gesture; `unlock_audio` from the tap-to-start overlay and canvas input
// both call `try_init`.
//
// `OutputStream` (and the cpal Stream it wraps) is `!Send` on most backends,
// so we keep audio state in a thread_local accessed only from the main thread.

use std::cell::RefCell;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use rodio::source::{Source, Zero};
use rodio::{dynamic_mixer, OutputStream, Sink};

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

pub struct Audio {
    _stream: OutputStream,
    _bgm_sink: Sink,
    _sfx_sink: Sink,
    sfx_mix: Arc<dynamic_mixer::DynamicMixerController<f32>>,
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

        let (sfx_mix, sfx_mix_out) = dynamic_mixer::mixer(1, 44_100);
        sfx_mix.add(Zero::<f32>::new(1, 44_100));
        let sfx_sink = Sink::try_new(&handle)?;
        sfx_sink.append(sfx_mix_out);
        sfx_sink.set_volume(0.08);
        sfx_sink.play();

        Ok(Audio {
            _stream,
            _bgm_sink: bgm_sink,
            _sfx_sink: sfx_sink,
            sfx_mix,
        })
    }

    pub fn pling(&self, freq: f32) {
        self.sfx_mix.add(Pling::new(freq, 0.25));
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
