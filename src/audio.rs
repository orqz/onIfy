//! Low-latency audio output.
//!
//! librespot's bundled sinks keep 0.5–2 s of audio queued and drain all of it
//! before pausing, so pause, seek and volume changes lag behind the UI. This
//! sink keeps a short queue of its own and applies volume and fades at output
//! time, against a device buffer of a few tens of milliseconds. Pausing,
//! seeking and volume changes are heard almost at once, and never click.
//!
//! Output goes to PipeWire/PulseAudio on Linux and through cpal (WASAPI,
//! CoreAudio) elsewhere.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use librespot_core::Error;
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume, VolumeGetter};

const RATE: u32 = 44_100;
const CHANNELS: usize = 2;
/// Samples (not frames) kept queued ahead of the device.
const QUEUE_CAP: usize = RATE as usize * CHANNELS / 4; // 250 ms
/// Envelope step per frame: fades take 12 ms.
const FADE_STEP: f32 = 1.0 / (RATE as f32 * 0.012);
/// One-pole smoothing for volume changes (~10 ms time constant).
const VOLUME_SMOOTHING: f32 = 1.0 / (RATE as f32 * 0.010);

#[derive(Default)]
struct State {
    queue: VecDeque<f32>,
    /// The player wants audio to be audible.
    playing: bool,
    /// Fade out, then drop everything queued (seek / skip).
    flush: bool,
    /// When the user skipped, and whether the flush for it has started yet:
    /// for logging how long until the new song is heard.
    skipped: Option<(std::time::Instant, bool)>,
}

pub struct Output {
    state: Mutex<State>,
    /// Signalled when samples arrive or flags change.
    wake: Condvar,
    /// Signalled when the queue shrinks.
    space: Condvar,
    gain: AtomicU32,
    raw_volume: AtomicU16,
}

/// Per-device rendering state: where the fade and the volume ramp are.
struct Renderer {
    envelope: f32,
    gain: f32,
}

impl Renderer {
    /// Moves up to `out.len()` samples from the queue into `out`, applying
    /// fades and volume. Returns how many samples were written.
    fn render(&mut self, out_gain: &AtomicU32, st: &mut State, out: &mut [f32]) -> usize {
        if let Some((_, flushing)) = &mut st.skipped {
            *flushing |= st.flush;
        }
        if st.flush && self.envelope == 0.0 {
            st.queue.clear();
            st.flush = false;
        }
        if st.queue.is_empty() {
            // Underrun or idle: whatever comes next fades in.
            self.envelope = 0.0;
            st.flush = false;
            return 0;
        }
        let target = if st.playing && !st.flush { 1.0 } else { 0.0 };
        if target == 0.0 && self.envelope == 0.0 {
            return 0;
        }
        let target_gain = f32::from_bits(out_gain.load(Ordering::Relaxed));
        let mut written = 0;
        while written + CHANNELS <= out.len() && st.queue.len() >= CHANNELS {
            if self.envelope < target {
                self.envelope = (self.envelope + FADE_STEP).min(1.0);
            } else if self.envelope > target {
                self.envelope = (self.envelope - FADE_STEP).max(0.0);
            }
            self.gain += (target_gain - self.gain) * VOLUME_SMOOTHING;
            // Smoothstep keeps the fade free of corners.
            let e = self.envelope;
            let g = self.gain * e * e * (3.0 - 2.0 * e);
            for _ in 0..CHANNELS {
                out[written] = st.queue.pop_front().unwrap_or(0.0) * g;
                written += 1;
            }
            if self.envelope == 0.0 && target == 0.0 {
                break;
            }
        }
        if written > 0 && target > 0.0 {
            if let Some((at, true)) = st.skipped {
                log::debug!("new song heard {} ms after the skip", at.elapsed().as_millis());
                st.skipped = None;
            }
        }
        written
    }
}

impl Output {
    pub fn new(initial_volume: u16) -> Arc<Self> {
        let out = Arc::new(Self {
            state: Mutex::default(),
            wake: Condvar::new(),
            space: Condvar::new(),
            gain: AtomicU32::new(volume_to_gain(initial_volume).to_bits()),
            raw_volume: AtomicU16::new(initial_volume),
        });
        backend::start(out.clone());
        out
    }

    fn renderer(&self) -> Renderer {
        Renderer {
            envelope: 0.0,
            gain: f32::from_bits(self.gain.load(Ordering::Relaxed)),
        }
    }

    /// Notes when the user skipped, for timing (see `State::skipped`).
    pub fn mark_skip(&self) {
        self.state.lock().unwrap().skipped = Some((std::time::Instant::now(), false));
    }

    /// Drop queued audio after a short fade so a seek or skip is heard at once.
    pub fn flush(&self) {
        let mut st = self.state.lock().unwrap();
        if !st.queue.is_empty() {
            st.flush = true;
            self.wake.notify_all();
        }
    }

    pub fn sink(self: &Arc<Self>) -> Box<dyn Sink> {
        Box::new(OutputSink(self.clone()))
    }

    pub fn mixer(self: &Arc<Self>) -> Arc<dyn Mixer> {
        Arc::new(OutputMixer(self.clone()))
    }

    pub fn volume(&self) -> u16 {
        self.raw_volume.load(Ordering::Relaxed)
    }

    /// Takes effect on the next audio slice, ahead of the player hearing of it.
    pub fn set_volume(&self, volume: u16) {
        self.raw_volume.store(volume, Ordering::Relaxed);
        self.gain.store(volume_to_gain(volume).to_bits(), Ordering::Relaxed);
    }
}

/// PipeWire/PulseAudio: a thread pushes 10 ms slices into a ~40 ms server
/// buffer, sleeping on a condvar whenever there's nothing to play.
#[cfg(target_os = "linux")]
mod backend {
    use std::sync::Arc;
    use std::time::Duration;

    use libpulse_binding::def::BufferAttr;
    use libpulse_binding::sample::{Format, Spec};
    use libpulse_binding::stream::Direction;
    use libpulse_simple_binding::Simple;

    use super::{CHANNELS, Output, RATE};

    const SLICE_SAMPLES: usize = RATE as usize / 100 * CHANNELS;
    const SERVER_BUFFER_MS: u32 = 40;

    pub fn start(out: Arc<Output>) {
        std::thread::Builder::new()
            .name("onify-audio".into())
            .spawn(move || run(&out))
            .expect("spawn audio thread");
    }

    fn run(out: &Output) {
        let mut server: Option<Simple> = None;
        let mut renderer = out.renderer();
        let mut slice = vec![0.0f32; SLICE_SAMPLES];
        let mut bytes: Vec<u8> = Vec::with_capacity(SLICE_SAMPLES * 4);
        loop {
            let written = {
                let mut st = out.state.lock().unwrap();
                loop {
                    let n = renderer.render(&out.gain, &mut st, &mut slice);
                    out.space.notify_all();
                    if n > 0 {
                        break n;
                    }
                    st = out.wake.wait(st).unwrap();
                }
            };
            if server.is_none() {
                match open_server() {
                    Ok(s) => server = Some(s),
                    Err(e) => {
                        log::error!("audio output unavailable: {e}");
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                }
            }
            bytes.clear();
            bytes.extend(slice[..written].iter().flat_map(|s| s.to_le_bytes()));
            if let Some(s) = &server {
                if let Err(e) = s.write(&bytes) {
                    log::error!("audio write failed: {e}");
                    server = None;
                }
            }
        }
    }

    fn open_server() -> Result<Simple, libpulse_binding::error::PAErr> {
        let spec = Spec {
            format: Format::F32le,
            channels: CHANNELS as u8,
            rate: RATE,
        };
        let bytes_per_ms = RATE * CHANNELS as u32 * 4 / 1000;
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: bytes_per_ms * SERVER_BUFFER_MS,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        Simple::new(None, "onIfy", Direction::Playback, None, "Music", &spec, None, Some(&attr))
    }
}

/// WASAPI / CoreAudio through cpal: the device pulls from the queue, resampled
/// if the device doesn't run at 44.1 kHz.
#[cfg(not(target_os = "linux"))]
mod backend {
    use std::sync::Arc;
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    use super::{CHANNELS, Output, RATE, Renderer};

    pub fn start(out: Arc<Output>) {
        // cpal streams aren't Send on every platform; own one on its own thread.
        std::thread::Builder::new()
            .name("onify-audio".into())
            .spawn(move || loop {
                match open(out.clone()) {
                    Ok((stream, device)) => {
                        let _ = stream.play();
                        // Keep the stream alive. Reopen if its device goes away,
                        // or when the system's default output changes (headphones
                        // plugged in): a stream stays on the device it opened on.
                        while !out.device_lost() && default_device().as_deref() == Some(device.as_str()) {
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        log::info!("audio output changed, reopening");
                    }
                    Err(e) => {
                        log::error!("audio output unavailable: {e}");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            })
            .expect("spawn audio thread");
    }

    /// Linear-interpolating resampler from 44.1 kHz to the device rate.
    struct Puller {
        renderer: Renderer,
        step: f64,
        position: f64,
        previous: [f32; CHANNELS],
        next: [f32; CHANNELS],
        chunk: Vec<f32>,
        chunk_at: usize,
        chunk_len: usize,
    }

    impl Puller {
        fn next_frame(&mut self, out: &Output) -> [f32; CHANNELS] {
            if self.chunk_at >= self.chunk_len {
                let mut st = out.state.lock().unwrap();
                self.chunk_len = self.renderer.render(&out.gain, &mut st, &mut self.chunk);
                self.chunk_at = 0;
                out.space.notify_all();
                if self.chunk_len == 0 {
                    return [0.0; CHANNELS];
                }
            }
            let frame = [self.chunk[self.chunk_at], self.chunk[self.chunk_at + 1]];
            self.chunk_at += CHANNELS;
            frame
        }

        fn fill(&mut self, out: &Output, data: &mut [f32], device_channels: usize) {
            for frame in data.chunks_mut(device_channels) {
                let sample = if self.step == 1.0 {
                    self.next_frame(out)
                } else {
                    while self.position >= 1.0 {
                        self.previous = self.next;
                        self.next = self.next_frame(out);
                        self.position -= 1.0;
                    }
                    let t = self.position as f32;
                    self.position += self.step;
                    [0, 1].map(|c| self.previous[c] + (self.next[c] - self.previous[c]) * t)
                };
                for (i, s) in frame.iter_mut().enumerate() {
                    *s = if i < CHANNELS { sample[i] } else { 0.0 };
                }
            }
        }
    }

    /// The name of the system's default output device.
    fn default_device() -> Option<String> {
        cpal::default_host().default_output_device()?.name().ok()
    }

    /// A stream on the default output device, and that device's name.
    fn open(out: Arc<Output>) -> Result<(cpal::Stream, String), String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no output device")?;
        let name = device.name().map_err(|e| e.to_string())?;
        let wanted = device
            .supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|c| c.sample_format() == cpal::SampleFormat::F32 && c.channels() as usize >= CHANNELS)
            .find_map(|c| c.try_with_sample_rate(cpal::SampleRate(RATE)));
        let config = match wanted {
            Some(c) => c,
            None => device.default_output_config().map_err(|e| e.to_string())?,
        };
        if config.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("unsupported sample format {:?}", config.sample_format()));
        }
        let device_rate = config.sample_rate().0;
        let device_channels = config.channels() as usize;
        let mut puller = Puller {
            renderer: out.renderer(),
            step: RATE as f64 / device_rate as f64,
            position: 1.0,
            previous: [0.0; CHANNELS],
            next: [0.0; CHANNELS],
            chunk: vec![0.0; 512 * CHANNELS],
            chunk_at: 0,
            chunk_len: 0,
        };
        out.set_device_lost(false);
        let lost = out.clone();
        let stream = device
            .build_output_stream(
                &config.config(),
                move |data: &mut [f32], _| puller.fill(&out, data, device_channels),
                move |e| {
                    log::error!("audio device error: {e}");
                    lost.set_device_lost(true);
                },
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok((stream, name))
    }
}

#[cfg(not(target_os = "linux"))]
static DEVICE_LOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(not(target_os = "linux"))]
impl Output {
    fn device_lost(&self) -> bool {
        DEVICE_LOST.load(Ordering::Relaxed)
    }

    fn set_device_lost(&self, lost: bool) {
        DEVICE_LOST.store(lost, Ordering::Relaxed);
    }
}

/// A cubic curve, like desktop volume sliders: even steps in loudness all the
/// way along. (A 60 dB log curve left the bottom third of the slider silent.)
fn volume_to_gain(volume: u16) -> f32 {
    let v = volume as f32 / u16::MAX as f32;
    v * v * v
}

struct OutputSink(Arc<Output>);

impl Sink for OutputSink {
    fn start(&mut self) -> SinkResult<()> {
        self.0.state.lock().unwrap().playing = true;
        self.0.wake.notify_all();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        // Keep the queue: resuming continues exactly where we paused.
        self.0.state.lock().unwrap().playing = false;
        self.0.wake.notify_all();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        let Ok(samples) = packet.samples() else {
            return Ok(());
        };
        let out = &self.0;
        let mut st = out.state.lock().unwrap();
        while st.queue.len() + samples.len() > QUEUE_CAP && st.playing {
            st = out.space.wait(st).unwrap();
        }
        st.queue.extend(samples.iter().map(|&s| s as f32));
        drop(st);
        out.wake.notify_all();
        Ok(())
    }
}

struct OutputMixer(Arc<Output>);

impl Mixer for OutputMixer {
    fn open(_: MixerConfig) -> Result<Self, Error> {
        Err(Error::unimplemented("constructed through Output::mixer"))
    }

    fn volume(&self) -> u16 {
        self.0.raw_volume.load(Ordering::Relaxed)
    }

    fn set_volume(&self, volume: u16) {
        self.0.set_volume(volume);
    }

    fn get_soft_volume(&self) -> Box<dyn VolumeGetter + Send> {
        // Volume is applied at output time instead, so changes are instant.
        Box::new(NoOpVolume)
    }
}
