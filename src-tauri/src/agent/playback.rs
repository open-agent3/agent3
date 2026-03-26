/// playback — Native cpal audio playback
///
/// Receives base64 PCM16 24kHz audio chunks, outputs directly to system default device via cpal.
/// Uses a shared ring buffer; cpal audio thread callback consumes data.
/// Calculates playback energy and emits to frontend to drive Edge Glow.
use base64::{engine::general_purpose::STANDARD, Engine};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use super::audio::SharedAudioFlags;

const SOURCE_SAMPLE_RATE: u32 = 24000;

// ============================================================
// Playback commands
// ============================================================

/// session → playback
pub enum PlaybackCommand {
    /// Enqueue audio chunk (base64 PCM16 24kHz mono)
    Enqueue(String),
    /// Fade out then clear queue (smooth transition, specified duration in ms)
    FadeOut(u64),
    /// Immediately clear queue (hard cut, only for forced reset)
    #[allow(dead_code)]
    Flush,
}

// ============================================================
// Playback Handle
// ============================================================

pub struct PlaybackHandle {
    _thread: std::thread::JoinHandle<()>,
    _bridge: tokio::task::JoinHandle<()>,
}

impl Drop for PlaybackHandle {
    fn drop(&mut self) {
        self._bridge.abort();
    }
}

// ============================================================
// Start playback
// ============================================================

pub fn start(
    app: AppHandle,
    playback_rx: mpsc::Receiver<PlaybackCommand>,
    flags: Arc<SharedAudioFlags>,
) -> Result<PlaybackHandle, String> {
    let (std_tx, std_rx) = std_mpsc::channel::<PlaybackCommand>();
    let flags_clone = flags.clone();

    let thread = std::thread::Builder::new()
        .name("playback".into())
        .spawn(move || playback_thread(app, std_rx, flags_clone))
        .map_err(|e| format!("Failed to spawn playback thread: {e}"))?;

    // Bridge task: tokio mpsc → std mpsc
    let bridge = tokio::spawn(async move {
        let mut rx = playback_rx;
        while let Some(cmd) = rx.recv().await {
            if std_tx.send(cmd).is_err() {
                break;
            }
        }
    });

    Ok(PlaybackHandle {
        _thread: thread,
        _bridge: bridge,
    })
}

// ============================================================
// Playback thread
// ============================================================

fn playback_thread(
    app: AppHandle,
    rx: std_mpsc::Receiver<PlaybackCommand>,
    flags: Arc<SharedAudioFlags>,
) {
    let mut device = match open_output_device(None, flags.clone()) {
        Some(d) => d,
        None => return,
    };

    let mut last_energy: f32 = 0.0;
    // Track device consumption to detect stalls
    let mut last_consume_snapshot: u64 = 0;
    let mut stall_since: Option<std::time::Instant> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            PlaybackCommand::Enqueue(base64) => {
                let pcm = match decode_pcm16(&base64) {
                    Some(p) => p,
                    None => {
                        log::warn!("[Playback] Failed to decode PCM16 from base64");
                        continue;
                    }
                };

                let energy = compute_playback_energy(&pcm);
                last_energy = last_energy * 0.6 + energy * 0.4;
                app.emit("agent-playback-energy", last_energy).ok();

                let mut samples: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();

                // If source sample rate differs from device, linear interpolation resample
                if SOURCE_SAMPLE_RATE != device.device_sample_rate {
                    samples = resample(&samples, SOURCE_SAMPLE_RATE, device.device_sample_rate);
                }

                let buf_len = {
                    let mut buf = device.buffer.lock().unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
                    buf.extend(samples);
                    flags.is_playing.store(true, Ordering::Relaxed);
                    buf.len()
                };

                // Detect device stall: cpal callback should be consuming samples.
                // WS delivers audio much faster than real-time, so large buffers
                // are normal. Only intervene if the device has truly stopped.
                let current_consume = device.consume_counter.load(Ordering::Relaxed);
                if current_consume != last_consume_snapshot {
                    // Device is alive — consuming samples normally
                    last_consume_snapshot = current_consume;
                    stall_since = None;
                } else if buf_len > device.device_sample_rate as usize * 2 {
                    // Buffer has >2s of audio and device hasn't consumed anything
                    let stall_start = stall_since.get_or_insert_with(std::time::Instant::now);
                    if stall_start.elapsed() > std::time::Duration::from_secs(3) {
                        log::warn!(
                            "[Playback] Device stall detected (no consumption for 3s, buffer {}), rebuilding stream",
                            buf_len
                        );
                        // Keep latest ~1s, discard stale head
                        let keep = device.device_sample_rate as usize;
                        {
                            let mut buf = device.buffer.lock().unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
                            if buf.len() > keep {
                                let drain = buf.len() - keep;
                                buf.drain(..drain);
                            }
                        }
                        if let Some(new_dev) =
                            open_output_device(Some(device.buffer.clone()), flags.clone())
                        {
                            device = new_dev;
                            last_consume_snapshot = device.consume_counter.load(Ordering::Relaxed);
                        }
                        stall_since = None;
                    }
                }
            }
            PlaybackCommand::FadeOut(duration_ms) => {
                let mut buf = device.buffer.lock().unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
                let fade_samples = (device.device_sample_rate as u64 * duration_ms / 1000) as usize;
                let fade_len = fade_samples.min(buf.len());
                for i in 0..fade_len {
                    buf[i] *= 1.0 - (i as f32 / fade_len as f32);
                }
                buf.truncate(fade_len);
                if fade_len == 0 {
                    flags.is_playing.store(false, Ordering::Relaxed);
                }
                last_energy = 0.0;
                app.emit("agent-playback-energy", 0.0_f32).ok();
            }
            PlaybackCommand::Flush => {
                device
                    .buffer
                    .lock()
                    .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); })
                    .clear();
                flags.is_playing.store(false, Ordering::Relaxed);
                last_energy = 0.0;
                app.emit("agent-playback-energy", 0.0_f32).ok();
            }
        }
    }

    log::info!("[Playback] Loop ended");
}

// ============================================================
// Output device
// ============================================================

struct OutputDevice {
    _stream: cpal::Stream,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    device_sample_rate: u32,
    /// Monotonically increasing counter updated by cpal callback whenever it consumes samples.
    /// Used to detect device stalls (callback stopped firing).
    consume_counter: Arc<AtomicU64>,
}

fn open_output_device(
    existing_buffer: Option<Arc<Mutex<VecDeque<f32>>>>,
    flags: Arc<SharedAudioFlags>,
) -> Option<OutputDevice> {
    let consume_counter = Arc::new(AtomicU64::new(0));
    let host = cpal::default_host();
    let device = host.default_output_device();
    let device = match device {
        Some(d) => d,
        None => {
            log::error!("[Playback] No default output device found");
            return None;
        }
    };
    let name = device.name().unwrap_or_default();
    let config = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            log::error!("[Playback] Failed to get output config: {}", e);
            return None;
        }
    };

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();

    log::info!(
        "[Playback] Opening device: {} ({}Hz {}ch {:?})",
        name,
        sample_rate,
        channels,
        sample_format
    );

    let stream_config: cpal::StreamConfig = config.into();
    let buffer = existing_buffer.unwrap_or_else(|| Arc::new(Mutex::new(VecDeque::new())));

    let stream = match sample_format {
        SampleFormat::F32 => build_stream_typed::<f32>(
            &device,
            &stream_config,
            buffer.clone(),
            channels,
            flags.clone(),
            consume_counter.clone(),
        ),
        SampleFormat::I16 => build_stream_typed::<i16>(
            &device,
            &stream_config,
            buffer.clone(),
            channels,
            flags.clone(),
            consume_counter.clone(),
        ),
        SampleFormat::U16 => build_stream_typed::<u16>(
            &device,
            &stream_config,
            buffer.clone(),
            channels,
            flags.clone(),
            consume_counter.clone(),
        ),
        other => {
            log::error!("[Playback] Unsupported sample format: {:?}", other);
            return None;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            log::error!("[Playback] Failed to build output stream: {}", e);
            return None;
        }
    };

    if let Err(e) = stream.play() {
        log::error!("[Playback] Failed to start output stream: {}", e);
        return None;
    }

    log::info!("[Playback] Output stream opened and playing");

    Some(OutputDevice {
        _stream: stream,
        buffer,
        device_sample_rate: sample_rate,
        consume_counter,
    })
}

/// Build cpal output stream of the specified sample type
fn build_stream_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    channels: usize,
    flags: Arc<SharedAudioFlags>,
    consume_counter: Arc<AtomicU64>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            if let Ok(mut buf) = buffer.try_lock() {
                let had_data = !buf.is_empty();
                for frame in data.chunks_mut(channels) {
                    let sample_f32 = buf.pop_front().unwrap_or(0.0);
                    let sample = T::from_sample(sample_f32);
                    for s in frame.iter_mut() {
                        *s = sample;
                    }
                }
                // Signal that the device is alive and consuming
                if had_data {
                    consume_counter.fetch_add(1, Ordering::Relaxed);
                }
                // Update is_playing: false when buffer fully drained
                if buf.is_empty() {
                    flags.is_playing.store(false, Ordering::Relaxed);
                }
            } else {
                // Failed to acquire lock — output silence to avoid blocking audio thread
                let zero = T::from_sample(0.0f32);
                for s in data.iter_mut() {
                    *s = zero;
                }
            }
        },
        |err| log::error!("[Playback] Stream error: {}", err),
        None,
    )
}

// ============================================================
// Resample (linear interpolation)
// ============================================================

fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = samples[idx.min(samples.len() - 1)];
            let b = samples[(idx + 1).min(samples.len() - 1)];
            a * (1.0 - frac) + b * frac
        })
        .collect()
}

// ============================================================
// Utility functions
// ============================================================

/// base64 → PCM i16 (little-endian)
fn decode_pcm16(base64: &str) -> Option<Vec<i16>> {
    let bytes = STANDARD.decode(base64).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(samples)
}

/// Calculate RMS energy of PCM i16 samples (0.0 ~ 1.0)
fn compute_playback_energy(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let f = s as f64 / 32768.0;
            f * f
        })
        .sum();
    (sum / samples.len() as f64).sqrt() as f32
}
