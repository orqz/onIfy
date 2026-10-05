//! Low-latency audio output.
//!
//! librespot's bundled sinks keep 0.5–2 s of audio queued and drain all of it
//! before pausing, so pause, seek and volume changes lag behind the UI. This
//! sink keeps a short queue of its own, feeds PipeWire/PulseAudio in 10 ms
//! slices against a ~40 ms server buffer, and applies volume and fades at
//! output time. Pausing, seeking and volume changes are heard within a few tens
//! of milliseconds, and the fades mean they never click.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use librespot_core::Error;
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume, VolumeGetter};

const RATE: u32 = 44_100;
const CHANNELS: usize = 2;
/// Samples (not frames) kept queued ahead of the output thread.
const QUEUE_CAP: usize = RATE as usize * CHANNELS / 4; // 250 ms
/// Frames handed to the server per write.
const SLICE_FRAMES: usize = RATE as usize / 100; // 10 ms
/// Envelope step per frame: fades take 12 ms.
const FADE_STEP: f32 = 1.0 / (RATE as f32 * 0.012);
/// One-pole smoothing for volume changes (~10 ms time constant).
const VOLUME_SMOOTHING: f32 = 1.0 / (RATE as f32 * 0.010);
const SERVER_BUFFER_MS: u32 = 40;

#[derive(Default)]
struct State {
    queue: VecDeque<f32>,
    /// The player wants audio to be audible.
    playing: bool,
    /// Fade out, then drop everything queued (seek / skip).
    flush: bool,
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

impl Output {
    pub fn new(initial_volume: u16) -> Arc<Self> {
        let out = Arc::new(Self {
            state: Mutex::default(),
            wake: Condvar::new(),
            space: Condvar::new(),
            gain: AtomicU32::new(volume_to_gain(initial_volume).to_bits()),
            raw_volume: AtomicU16::new(initial_volume),
        });
        let thread_out = out.clone();
        std::thread::Builder::new()
            .name("onify-audio".into())
            .spawn(move || thread_out.run())
            .expect("spawn audio thread");
        out
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

    fn run(&self) {
        let mut server: Option<Simple> = None;
        let mut envelope = 0.0f32;
        let mut gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        let mut slice: Vec<f32> = Vec::with_capacity(SLICE_FRAMES * CHANNELS);
        let mut bytes: Vec<u8> = Vec::with_capacity(SLICE_FRAMES * CHANNELS * 4);

        loop {
            slice.clear();
            {
                let mut st = self.state.lock().unwrap();
                loop {
                    if st.flush && envelope == 0.0 {
                        st.queue.clear();
                        st.flush = false;
                        self.space.notify_all();
                    }
                    let audible = st.playing && !st.flush;
                    if st.queue.is_empty() {
                        // Underrun or idle: whatever comes next fades in.
                        envelope = 0.0;
                        st.flush = false;
                    } else if audible || envelope > 0.0 {
                        break;
                    }
                    st = self.wake.wait(st).unwrap();
                }

                let target = if st.playing && !st.flush { 1.0 } else { 0.0 };
                let target_gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
                while slice.len() < SLICE_FRAMES * CHANNELS && st.queue.len() >= CHANNELS {
                    if envelope < target {
                        envelope = (envelope + FADE_STEP).min(1.0);
                    } else if envelope > target {
                        envelope = (envelope - FADE_STEP).max(0.0);
                    }
                    gain += (target_gain - gain) * VOLUME_SMOOTHING;
                    // Smoothstep keeps the fade free of corners.
                    let g = gain * envelope * envelope * (3.0 - 2.0 * envelope);
                    for _ in 0..CHANNELS {
                        slice.push(st.queue.pop_front().unwrap_or(0.0) * g);
                    }
                    if envelope == 0.0 && target == 0.0 {
                        break;
                    }
                }
                self.space.notify_all();
            }

            if slice.is_empty() {
                continue;
            }
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
            bytes.extend(slice.iter().flat_map(|s| s.to_le_bytes()));
            if let Some(s) = &server {
                if let Err(e) = s.write(&bytes) {
                    log::error!("audio write failed: {e}");
                    server = None;
                }
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
    Simple::new(
        None,
        "onIfy",
        Direction::Playback,
        None,
        "Music",
        &spec,
        None,
        Some(&attr),
    )
}

/// Same curve as librespot's default logarithmic volume control (60 dB).
fn volume_to_gain(volume: u16) -> f32 {
    let v = volume as f32 / u16::MAX as f32;
    if v <= 0.0 {
        0.0
    } else {
        (1000f32.powf(v) - 1.0) / 999.0
    }
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
        self.0.raw_volume.store(volume, Ordering::Relaxed);
        self.0
            .gain
            .store(volume_to_gain(volume).to_bits(), Ordering::Relaxed);
    }

    fn get_soft_volume(&self) -> Box<dyn VolumeGetter + Send> {
        // Volume is applied at output time instead, so changes are instant.
        Box::new(NoOpVolume)
    }
}
