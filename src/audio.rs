// Audio: cpal output stream feeding a tiny in-callback mixer.
//
// On wasm we enable cpal's `audioworklet` feature, which runs the audio
// callback on a dedicated AudioWorkletProcessor thread. That keeps audio out
// of the main thread (no glitching while wgpu encodes a frame) and avoids
// cpal's reliance on a !Send cpal Stream stuffed into a thread_local.
//
// Mixer is single-buffer, lock-free for the hot path:
//   * BGM is pre-decoded to a Vec<f32> at init and looped in the callback.
//   * Pling spawn requests come through a crossbeam unbounded channel; the
//     callback drains it and pushes new voices into a small Vec.
//   * Voices are summed in float and written to every output channel.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use crossbeam_channel::{unbounded, Receiver, Sender};

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

/// Commands sent from main thread → audio callback.
enum Cmd {
    Pling { freq: f32 },
}

pub struct Audio {
    _stream: cpal::Stream,
    cmd_tx: Sender<Cmd>,
}

impl Audio {
    pub fn new(bgm_bytes: &'static [u8]) -> Result<Self, BoxErr> {
        // On wasm, `cpal::default_host()` is hard-coded to the main-thread
        // WebAudio host even when the `audioworklet` feature is enabled —
        // that path uses scheduled AudioBufferSourceNodes whose scheduling
        // runs on main, so wgpu work can starve audio. We explicitly pick
        // the AudioWorklet host so the callback runs on a dedicated thread.
        // On native we keep the platform default.
        #[cfg(target_arch = "wasm32")]
        let host = cpal::host_from_id(cpal::HostId::AudioWorklet)
            .map_err(|e| -> BoxErr { format!("AudioWorklet host unavailable: {e}").into() })?;
        #[cfg(not(target_arch = "wasm32"))]
        let host = cpal::default_host();

        log::info!("audio host: {:?}", host.id());

        let device = host
            .default_output_device()
            .ok_or("no default audio output device")?;
        let supported = device.default_output_config()?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let device_rate = config.sample_rate;
        let channels = config.channels;

        // Decode the BGM once, up front. Resample on the fly in the callback
        // via linear interpolation if the BGM rate doesn't match the device.
        let (bgm_samples, bgm_channels, bgm_rate) = decode_ogg_vorbis(bgm_bytes)?;
        log::info!(
            "audio: device={}Hz×{}ch ({:?}) | bgm={}Hz×{}ch ({} samples)",
            device_rate, channels, sample_format,
            bgm_rate, bgm_channels, bgm_samples.len()
        );

        let (cmd_tx, cmd_rx) = unbounded::<Cmd>();

        let mut state = MixerState {
            bgm: bgm_samples,
            bgm_channels,
            bgm_rate,
            bgm_pos: 0.0,
            bgm_volume: 0.5,
            plings: Vec::with_capacity(32),
            pling_volume: 0.08,
            device_rate,
            device_channels: channels,
            cmd_rx,
        };

        let err_cb = |e| log::error!("audio stream error: {e}");

        // cpal 0.17 only delivers F32 on the wasm backend, but match anyway so
        // a native (alsa/wasapi) build still works.
        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
                &config,
                move |out, _| state.fill_f32(out),
                err_cb,
                None,
            )?,
            SampleFormat::I16 => {
                let mut buf = Vec::<f32>::new();
                device.build_output_stream::<i16, _, _>(
                    &config,
                    move |out, _| {
                        buf.resize(out.len(), 0.0);
                        state.fill_f32(&mut buf);
                        for (i, s) in buf.iter().enumerate() {
                            out[i] = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        }
                    },
                    err_cb,
                    None,
                )?
            }
            other => return Err(format!("unsupported sample format: {other:?}").into()),
        };
        stream.play()?;

        Ok(Audio { _stream: stream, cmd_tx })
    }

    pub fn pling(&self, freq: f32) {
        let _ = self.cmd_tx.send(Cmd::Pling { freq });
    }
}

// ---- Mixer (audio thread) ---------------------------------------------------

struct MixerState {
    bgm: Vec<f32>,         // interleaved at `bgm_channels`
    bgm_channels: u16,
    bgm_rate: u32,
    bgm_pos: f64,          // fractional frame index for resampling
    bgm_volume: f32,
    plings: Vec<PlingVoice>,
    pling_volume: f32,
    device_rate: u32,
    device_channels: u16,
    cmd_rx: Receiver<Cmd>,
}

impl MixerState {
    fn fill_f32(&mut self, out: &mut [f32]) {
        // Drain incoming commands. Bounded work — at most one pling per
        // overlap-start-event per frame.
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                Cmd::Pling { freq } => self
                    .plings
                    .push(PlingVoice::new(freq, 0.25, self.device_rate)),
            }
        }

        let oc = self.device_channels as usize;
        let frames = out.len() / oc;

        // Resampling step in fractional source-frames per output-frame.
        let step = self.bgm_rate as f64 / self.device_rate as f64;

        for f in 0..frames {
            // BGM sample (linearly interpolated, downmixed to mono).
            let bgm_mono = if self.bgm.is_empty() {
                0.0
            } else {
                sample_bgm(&self.bgm, self.bgm_channels as usize, self.bgm_pos)
            };
            self.bgm_pos += step;
            if !self.bgm.is_empty() {
                let total = (self.bgm.len() / self.bgm_channels as usize) as f64;
                if self.bgm_pos >= total {
                    self.bgm_pos -= total; // loop
                }
            }

            let mut sfx = 0.0f32;
            for v in self.plings.iter_mut() {
                sfx += v.next_sample();
            }

            let mixed = bgm_mono * self.bgm_volume + sfx * self.pling_volume;

            for c in 0..oc {
                out[f * oc + c] = mixed;
            }
        }

        self.plings.retain(|v| !v.finished());
    }
}

/// Linear-interpolated mono sample from interleaved BGM at fractional index.
fn sample_bgm(samples: &[f32], channels: usize, pos: f64) -> f32 {
    let total = samples.len() / channels;
    if total == 0 {
        return 0.0;
    }
    let i0 = pos.floor() as usize % total;
    let i1 = (i0 + 1) % total;
    let frac = (pos - pos.floor()) as f32;
    let mut s0 = 0.0;
    let mut s1 = 0.0;
    for c in 0..channels {
        s0 += samples[i0 * channels + c];
        s1 += samples[i1 * channels + c];
    }
    let inv = 1.0 / channels as f32;
    s0 * inv * (1.0 - frac) + s1 * inv * frac
}

// ---- Pling synth ------------------------------------------------------------

struct PlingVoice {
    sample_idx: u32,
    total_samples: u32,
    freq: f32,
    decay: f32,
    sample_rate: u32,
}

impl PlingVoice {
    fn new(freq: f32, duration_secs: f32, sample_rate: u32) -> Self {
        let total_samples = (sample_rate as f32 * duration_secs) as u32;
        let decay = -(0.01_f32.ln()) / duration_secs;
        Self { sample_idx: 0, total_samples, freq, decay, sample_rate }
    }
    fn next_sample(&mut self) -> f32 {
        if self.sample_idx >= self.total_samples {
            return 0.0;
        }
        let t = self.sample_idx as f32 / self.sample_rate as f32;
        let env = (-self.decay * t).exp();
        let phase = std::f32::consts::TAU * self.freq * t;
        self.sample_idx += 1;
        phase.sin() * env
    }
    fn finished(&self) -> bool {
        self.sample_idx >= self.total_samples
    }
}

// ---- BGM decode (lewton: Ogg/Vorbis) ---------------------------------------

fn decode_ogg_vorbis(bytes: &'static [u8]) -> Result<(Vec<f32>, u16, u32), BoxErr> {
    use lewton::inside_ogg::OggStreamReader;
    let mut sr = OggStreamReader::new(std::io::Cursor::new(bytes))?;
    let channels = sr.ident_hdr.audio_channels as u16;
    let rate = sr.ident_hdr.audio_sample_rate;
    let mut out = Vec::<f32>::with_capacity(rate as usize * 80 * channels as usize);

    while let Some(packet) = sr.read_dec_packet()? {
        // packet: Vec<Vec<i16>>, one inner vec per channel.
        if packet.is_empty() || packet[0].is_empty() {
            continue;
        }
        let frame_count = packet[0].len();
        for f in 0..frame_count {
            for ch in 0..channels as usize {
                let s = packet.get(ch).map(|c| c[f]).unwrap_or(0);
                out.push(s as f32 / i16::MAX as f32);
            }
        }
    }
    Ok((out, channels, rate))
}

// ---- Public API + thread-local handle --------------------------------------

thread_local! {
    static AUDIO: RefCell<Option<Audio>> = const { RefCell::new(None) };
}
static AUDIO_READY: AtomicBool = AtomicBool::new(false);

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
                AUDIO_READY.store(true, Ordering::Release);
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
    AUDIO_READY.load(Ordering::Acquire)
}
