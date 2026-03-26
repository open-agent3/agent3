/// audio — Native Rust audio capture
///
/// Uses cpal to directly capture system microphone, bypassing WebView2 permission dialogs.
/// Audio stream dual-path distribution:
///   - Path A: Wakeword detection (always running) — rustpotter integration
///   - Path B: Stream to session (after wake) — resample to 24kHz base64 PCM16
///
/// State machine: Sleeping → Awakened → Listening → Sleeping
use base64::{engine::general_purpose::STANDARD, Engine};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

// ============================================================
// State definitions
// ============================================================

/// Wake state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WakeState {
    /// Sleeping — only wakeword detection, no streaming, no WS connection
    Sleeping = 0,
    /// Awakened — establishing WS connection
    Awakened = 1,
    /// Listening — WS connected, normal streaming
    Listening = 2,
}

impl WakeState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Awakened,
            2 => Self::Listening,
            _ => Self::Sleeping,
        }
    }
}

/// Wake events: audio → session
#[derive(Debug)]
pub enum WakeEvent {
    /// Wakeword detected (or auto-wake when wakeword is disabled)
    Detected,
    /// Silence timeout, disconnect WS
    Timeout,
    /// Scheduled task fired while session was sleeping — wake and greet with task context
    ScheduledTask(String),
}

// ============================================================
// Shared audio flags (lock-free cross-thread state)
// ============================================================

/// Consolidated audio flags shared across audio capture, playback, and session threads.
/// All fields are atomic — safe for lock-free read/write from cpal real-time callbacks.
pub struct SharedAudioFlags {
    /// Wake state machine (Sleeping=0, Awakened=1, Listening=2)
    pub wake_state: AtomicU8,
    /// Playback buffer has audio being consumed by cpal output
    pub is_playing: AtomicBool,
    /// AI response in progress (covers inter-chunk gaps)
    pub is_ai_speaking: AtomicBool,
    /// Session connectivity state (0=sleeping, 2=connected)
    pub session_state: AtomicU8,
    /// Current speech duration (ms) for backchannel/interruption detection
    pub speech_duration_ms: std::sync::atomic::AtomicU64,
    /// Peak RMS energy during the current speech event
    pub peak_rms: std::sync::atomic::AtomicU32,
}

impl SharedAudioFlags {
    pub fn new(initial_wake: WakeState) -> Self {
        Self {
            wake_state: AtomicU8::new(initial_wake as u8),
            is_playing: AtomicBool::new(false),
            is_ai_speaking: AtomicBool::new(false),
            session_state: AtomicU8::new(0),
            speech_duration_ms: std::sync::atomic::AtomicU64::new(0),
            peak_rms: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

// ============================================================
// Audio Handle
// ============================================================

/// Holds cpal Stream (via dedicated thread) and state control
pub struct AudioHandle {
    /// Dedicated thread that owns the cpal::Stream (drop _stop_tx to terminate)
    _thread: std::thread::JoinHandle<()>,
    /// Dropping this sender signals the capture thread to exit (stream is dropped)
    _stop_tx: std::sync::mpsc::Sender<()>,
    /// Shared audio flags (wake state, speaking, playing, session state)
    flags: Arc<SharedAudioFlags>,
    /// Processing task
    _task: tokio::task::JoinHandle<()>,
    /// Wakeword recording flag (set to true when Config UI is recording)
    recording: Arc<AtomicBool>,
    /// Recording buffer (capture-rate PCM i16)
    pub record_buffer: Arc<Mutex<Vec<i16>>>,
    /// Capture sample rate (needed for WAV header during recording)
    pub capture_rate: u32,
}

impl AudioHandle {
    #[allow(dead_code)]
    pub fn get_state(&self) -> WakeState {
        WakeState::from_u8(self.flags.wake_state.load(Ordering::Relaxed))
    }

    /// Get shared flags reference (passed to session, playback, etc.)
    #[allow(dead_code)]
    pub fn shared_flags(&self) -> Arc<SharedAudioFlags> {
        self.flags.clone()
    }

    /// Start recording wakeword sample
    pub fn start_recording(&self) {
        self.record_buffer
            .lock()
            .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); })
            .clear();
        self.recording.store(true, Ordering::Relaxed);
    }

    /// Stop recording, return PCM i16 buffer
    pub fn stop_recording(&self) -> Vec<i16> {
        self.recording.store(false, Ordering::Relaxed);
        let mut buf = self.record_buffer.lock().unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
        std::mem::take(&mut *buf)
    }
}

// ============================================================
// Start capture
// ============================================================

/// Start native Rust audio capture
///
/// - `wake_tx`: Wake/timeout event sender (→ session)
/// - `audio_tx`: base64 PCM16 24kHz audio sender (→ session)
/// - `wakeword_enabled`: Whether wakeword detection is enabled
/// - `wake_model_path`: Wakeword model file path (.rpw)
/// - `flags`: Shared audio flags (wake state, is_playing, is_ai_speaking, session_state)
pub fn start(
    app: AppHandle,
    wake_tx: mpsc::Sender<WakeEvent>,
    audio_tx: mpsc::Sender<String>,
    wakeword_enabled: bool,
    wake_model_path: Option<String>,
    flags: Arc<SharedAudioFlags>,
) -> Result<AudioHandle, String> {
    let initial = if wakeword_enabled {
        WakeState::Sleeping
    } else {
        WakeState::Awakened
    };
    flags.wake_state.store(initial as u8, Ordering::Relaxed);

    let recording = Arc::new(AtomicBool::new(false));
    let record_buffer: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));

    // cpal callback → async processing task
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>(128);

    // Oneshot: capture thread sends back sample rate on success
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    // Stop signal: dropping _stop_tx causes the capture thread to exit
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    // Dedicated thread owns the cpal::Stream (cpal::Stream is !Send on some platforms)
    let thread = std::thread::Builder::new()
        .name("audio-capture".into())
        .spawn(move || {
            let host = cpal::default_host();
            let device = match host.default_input_device() {
                Some(d) => d,
                None => {
                    let _ = result_tx.send(Err(crate::i18n::t("audio.no_mic")));
                    return;
                }
            };
            log::info!("[Audio] Device: {}", device.name().unwrap_or_default());

            let (config, fmt) = match find_input_config(&device) {
                Ok(r) => r,
                Err(e) => {
                    let _ = result_tx.send(Err(e));
                    return;
                }
            };
            let rate = config.sample_rate.0;
            let ch = config.channels as usize;
            log::info!("[Audio] Config: {}Hz {}ch {:?}", rate, ch, fmt);

            let stream = match build_stream(&device, &config, fmt, ch, raw_tx) {
                Ok(s) => s,
                Err(e) => {
                    let _ = result_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = result_tx.send(Err(format!(
                    "{}: {e}",
                    crate::i18n::t("audio.start_stream_failed")
                )));
                return;
            }
            log::info!("[Audio] Capture started");
            let _ = result_tx.send(Ok(rate));

            // Keep stream alive — block until stop signal (sender dropped)
            let _ = stop_rx.recv();
            // stream dropped here → capture stops
            log::info!("[Audio] Capture thread exiting");
        })
        .map_err(|e| format!("Failed to spawn audio capture thread: {e}"))?;

    // Wait for the thread to report success or error
    let rate = result_rx
        .recv()
        .map_err(|_| "Audio capture thread exited unexpectedly".to_string())?
        .map_err(|e| e)?;

    let flags_clone = flags.clone();
    let app_panic = app.clone();
    let recording_clone = recording.clone();
    let record_buffer_clone = record_buffer.clone();
    let task = tokio::spawn(async move {
        process_loop(
            app,
            raw_rx,
            wake_tx,
            audio_tx,
            flags_clone,
            rate,
            wakeword_enabled,
            wake_model_path,
            recording_clone,
            record_buffer_clone,
        )
        .await;
        log::info!("[Audio] Process loop ended");
    });

    // Monitor task for panics — log and emit error status
    let task_monitor = tokio::spawn(async move {
        if let Err(e) = task.await {
            if e.is_panic() {
                log::error!("[Audio] Process loop panicked: {}", e);
                let _ = app_panic.emit(
                    "agent-status",
                    serde_json::json!({
                        "state": "error",
                        "message": "Audio capture crashed unexpectedly"
                    }),
                );
            }
        }
    });

    Ok(AudioHandle {
        _thread: thread,
        _stop_tx: stop_tx,
        flags,
        _task: task_monitor,
        recording,
        record_buffer,
        capture_rate: rate,
    })
}

// ============================================================
// Device configuration
// ============================================================

/// Select best input config: prefer 16kHz mono, then 48kHz
fn find_input_config(
    device: &cpal::Device,
) -> Result<(cpal::StreamConfig, cpal::SampleFormat), String> {
    let configs: Vec<_> = device
        .supported_input_configs()
        .map_err(|e| format!("{}: {e}", crate::i18n::t("audio.enum_config_failed")))?
        .collect();
    if configs.is_empty() {
        return Err(crate::i18n::t("audio.no_config"));
    }

    for &target in &[16000u32, 48000, 44100, 24000] {
        // Prefer mono
        for cfg in &configs {
            if cfg.channels() == 1
                && cfg.min_sample_rate().0 <= target
                && cfg.max_sample_rate().0 >= target
            {
                return Ok((
                    cfg.with_sample_rate(cpal::SampleRate(target)).config(),
                    cfg.sample_format(),
                ));
            }
        }
        // Accept multi-channel
        for cfg in &configs {
            if cfg.min_sample_rate().0 <= target && cfg.max_sample_rate().0 >= target {
                return Ok((
                    cfg.with_sample_rate(cpal::SampleRate(target)).config(),
                    cfg.sample_format(),
                ));
            }
        }
    }

    // Fallback: first config with highest sample rate
    let cfg = &configs[0];
    Ok((cfg.with_max_sample_rate().config(), cfg.sample_format()))
}

/// Build cpal input stream (supports i16 / f32 sample formats)
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    fmt: cpal::SampleFormat,
    channels: usize,
    tx: mpsc::Sender<Vec<i16>>,
) -> Result<cpal::Stream, String> {
    let err_fn = |e: cpal::StreamError| log::error!("[Audio] Stream error: {}", e);

    let build_err = crate::i18n::t("audio.build_stream_failed");

    match fmt {
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let _ = tx.try_send(to_mono_i16(data, channels));
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("{build_err}: {e}")),

        cpal::SampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<i16> = if channels > 1 {
                        data.chunks(channels)
                            .map(|f| (f[0].clamp(-1.0, 1.0) * 32767.0) as i16)
                            .collect()
                    } else {
                        data.iter()
                            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                            .collect()
                    };
                    let _ = tx.try_send(mono);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("{build_err}: {e}")),

        cpal::SampleFormat::U8 => device
            .build_input_stream(
                config,
                move |data: &[u8], _: &cpal::InputCallbackInfo| {
                    // U8: 0-255, center=128 → i16: -32768..32767
                    let mono: Vec<i16> = if channels > 1 {
                        data.chunks(channels)
                            .map(|f| (f[0] as i16 - 128) * 256)
                            .collect()
                    } else {
                        data.iter().map(|&s| (s as i16 - 128) * 256).collect()
                    };
                    let _ = tx.try_send(mono);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("{build_err}: {e}")),

        other => Err(format!(
            "{}: {:?}",
            crate::i18n::t("audio.unsupported_format"),
            other
        )),
    }
}

/// Multi-channel i16 → mono (take first channel)
fn to_mono_i16(data: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks(channels).map(|f| f[0]).collect()
}

// ============================================================
// Async processing loop
// ============================================================

#[allow(clippy::too_many_arguments)]
async fn process_loop(
    app: AppHandle,
    mut raw_rx: mpsc::Receiver<Vec<i16>>,
    wake_tx: mpsc::Sender<WakeEvent>,
    audio_tx: mpsc::Sender<String>,
    flags: Arc<SharedAudioFlags>,
    capture_rate: u32,
    wakeword_enabled: bool,
    wake_model_path: Option<String>,
    recording: Arc<AtomicBool>,
    record_buffer: Arc<Mutex<Vec<i16>>>,
) {
    // No wakeword → immediately notify session to start connection
    if !wakeword_enabled {
        let _ = wake_tx.send(WakeEvent::Detected).await;
        log::info!("[Audio] No wake word configured — auto-awakened");
    }

    // Initialize rustpotter detector
    let mut detector = if wakeword_enabled {
        create_detector(capture_rate, wake_model_path.as_deref())
    } else {
        None
    };

    // rustpotter requires fixed frame size, accumulate with buffer
    let frame_size = detector
        .as_ref()
        .map(|d| d.get_samples_per_frame())
        .unwrap_or(0);
    let mut detect_buf: Vec<i16> = if frame_size > 0 {
        Vec::with_capacity(frame_size * 2)
    } else {
        Vec::new()
    };

    let silence_timeout = std::time::Duration::from_secs(15);
    let mut last_speech = std::time::Instant::now();
    // Echo suppression: track when playback stopped, add tail delay before resuming capture
    let mut playback_stopped_at: Option<std::time::Instant> = None;
    let mut was_playing = false;
    // VERY IMPORTANT: The OS audio stack and wireless speakers can have up to 300-500ms latency.
    // The tail MUST be long enough to cover the actual physical speaker emit after buffer empties.
    const ECHO_TAIL_MS: u64 = 400;
    let mut current_speech_start: Option<std::time::Instant> = None;
    let mut current_peak_rms = 0.0f32;
    let mut prev_ws = WakeState::Sleeping;
    // Energy emission throttle: ~30fps (33ms between emits)
    let mut last_energy_emit = std::time::Instant::now() - std::time::Duration::from_millis(100);
    const ENERGY_EMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

    while let Some(samples) = raw_rx.recv().await {
        let ws = WakeState::from_u8(flags.wake_state.load(Ordering::Relaxed));

        // Reset silence timer when entering Listening state (new connection)
        if ws == WakeState::Listening && prev_ws != WakeState::Listening {
            last_speech = std::time::Instant::now();
        }
        prev_ws = ws;

        // 0. Recording mode: write to buffer (doesn't affect normal flow)
        if recording.load(Ordering::Relaxed) {
            if let Ok(mut buf) = record_buffer.lock() {
                buf.extend_from_slice(&samples);
            }
        }

        // 1. Calculate RMS energy → frontend Edge Glow (throttled to ~30fps)
        let rms = compute_rms(&samples);
        if last_energy_emit.elapsed() >= ENERGY_EMIT_INTERVAL {
            if let Err(e) = app.emit("agent-audio-energy", rms) {
                log::warn!("[Audio] Emit error: {}", e);
            }
            last_energy_emit = std::time::Instant::now();
        }

        // 2. Wakeword detection (Sleeping state only)
        if ws == WakeState::Sleeping {
            if let Some(ref mut det) = detector {
                detect_buf.extend_from_slice(&samples);
                // Feed detector at frame size
                while detect_buf.len() >= frame_size {
                    let frame: Vec<i16> = detect_buf.drain(..frame_size).collect();

                    let detection_result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            det.process_i16(&frame)
                        }))
                        .unwrap_or_else(|e| {
                            log::error!("[Audio] Rustpotter panic: {:?}", e);
                            None
                        });

                    if let Some(detection) = detection_result {
                        log::info!(
                            "[Audio] Wake word detected: {} (score={:.2})",
                            detection.name,
                            detection.score
                        );
                        flags
                            .wake_state
                            .store(WakeState::Awakened as u8, Ordering::Relaxed);
                        if let Err(e) = app.emit("agent-wake-state", "awakened") {
                            log::warn!("[Audio] Emit error: {}", e);
                        }
                        wake_tx.send(WakeEvent::Detected).await.ok();
                        last_speech = std::time::Instant::now();
                        detect_buf.clear();
                        break;
                    }
                }
            }
            // No detector and wakeword_enabled → already handled by fallback above
            continue;
        }

        // 3. Audio streaming (Awakened / Listening)
        // Echo suppression strategy:
        //   - While AI is playing audio, we use a HIGHER energy gate rather than full mute.
        //     This lets loud user speech through to the server for interrupt detection,
        //     while filtering out speaker echo picked up by the mic.
        //   - After playback stops, a short tail period uses the higher gate to absorb reverb.
        //   - When idle, a low noise gate filters ambient noise.
        //
        // Two flags work together:
        //   is_ai_speaking: set by session on ResponseStart, cleared on ResponseDone
        //   is_playing: set by playback on buffer enqueue, cleared when cpal drains
        let speaking = flags.is_ai_speaking.load(Ordering::Relaxed);
        let playing = flags.is_playing.load(Ordering::Relaxed);
        let active = speaking || playing;

        if !active && was_playing {
            log::info!(
                "[Audio] AI finished speaking, echo suppression tail ({}ms)",
                ECHO_TAIL_MS
            );
            playback_stopped_at = Some(std::time::Instant::now());
        } else if active && !was_playing {
            log::info!("[Audio] AI speaking, echo gate active");
            playback_stopped_at = None;
        }
        was_playing = active;

        let in_echo_tail = if active {
            false // handled by the `active` branch below
        } else if let Some(stopped) = playback_stopped_at {
            if stopped.elapsed() < std::time::Duration::from_millis(ECHO_TAIL_MS) {
                true
            } else {
                log::info!("[Audio] Echo suppression tail finished, mic open");
                playback_stopped_at = None;
                false
            }
        } else {
            false
        };

        // Energy thresholds:
        //   During AI playback / echo tail: higher gate to pass only real speech, block echo
        //   Idle: low gate to filter ambient noise
        const NOISE_GATE_RMS: f32 = 0.008;
        const ECHO_GATE_RMS: f32 = 0.05;

        let gate = if active || in_echo_tail {
            ECHO_GATE_RMS
        } else {
            NOISE_GATE_RMS
        };

        if rms > gate {
            last_speech = std::time::Instant::now();
            
            if current_speech_start.is_none() {
                current_speech_start = Some(std::time::Instant::now());
                current_peak_rms = rms;
                flags.speech_duration_ms.store(0, Ordering::Relaxed);
                flags.peak_rms.store(rms.to_bits(), Ordering::Relaxed);
            } else {
                current_peak_rms = current_peak_rms.max(rms);
                let duration = current_speech_start.unwrap().elapsed().as_millis() as u64;
                flags.speech_duration_ms.store(duration, Ordering::Relaxed);
                flags.peak_rms.store(current_peak_rms.to_bits(), Ordering::Relaxed);
            }
        } else if current_speech_start.is_some() {
            // Speech ended
            current_speech_start = None;
            // We can optionally leave the last duration/peak in the flags so session can read it shortly after
        }

        // Only send audio frames when Listening (WS connected and consuming)
        if ws == WakeState::Listening {
            let pcm24k = resample(&samples, capture_rate, 24000);
            let b64 = if rms < gate {
                encode_pcm16_base64(&vec![0i16; pcm24k.len()])
            } else {
                encode_pcm16_base64(&pcm24k)
            };
            if let Err(_e) = audio_tx.try_send(b64) {
                static DROP_COUNT: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let count = DROP_COUNT.fetch_add(1, Ordering::Relaxed);
                if count.is_multiple_of(500) {
                    log::warn!(
                        "[Audio] Dropped audio frame (channel full), total: {}",
                        count + 1
                    );
                }
            }
        }

        // 4. Silence timeout (only when Listening + wakeword enabled)
        if ws == WakeState::Listening && wakeword_enabled && last_speech.elapsed() > silence_timeout
        {
            log::info!("[Audio] Silence timeout — entering sleep");
            flags
                .wake_state
                .store(WakeState::Sleeping as u8, Ordering::Relaxed);
            if let Err(e) = app.emit("agent-wake-state", "sleeping") {
                log::warn!("[Audio] Emit error: {}", e);
            }
            let _ = wake_tx.send(WakeEvent::Timeout).await;
            detect_buf.clear();
        }
    }

    log::info!("[Audio] Process loop ended");
}

/// Create rustpotter detector (prints warning and returns None on failure)
fn create_detector(sample_rate: u32, model_path: Option<&str>) -> Option<rustpotter::Rustpotter> {
    use rustpotter::{Endianness, RustpotterConfig, WavFmt};

    let model_path = model_path?;
    if !std::path::Path::new(model_path).exists() {
        log::warn!("[Audio] Wake word model not found: {}", model_path);
        return None;
    }

    let mut config = RustpotterConfig {
        fmt: WavFmt {
            sample_rate: sample_rate as usize,
            sample_format: hound::SampleFormat::Int,
            bits_per_sample: 16,
            channels: 1,
            endianness: Endianness::Little,
        },
        ..Default::default()
    };
    // Enable gain normalization to improve detection consistency across different microphones/volumes
    config.filters.gain_normalizer.enabled = true;
    config.filters.gain_normalizer.min_gain = 0.5;
    config.filters.gain_normalizer.max_gain = 2.0;

    let mut detector = match rustpotter::Rustpotter::new(&config) {
        Ok(d) => d,
        Err(e) => {
            log::error!("[Audio] Failed to create rustpotter: {}", e);
            return None;
        }
    };

    if let Err(e) = detector.add_wakeword_from_file(model_path) {
        log::error!("[Audio] Failed to load wake word model: {}", e);
        return None;
    }

    log::info!(
        "[Audio] Rustpotter initialized (rate={}Hz, frame={})",
        sample_rate,
        detector.get_samples_per_frame()
    );
    Some(detector)
}

// ============================================================
// Utility functions
// ============================================================

/// Calculate RMS energy of PCM i16 samples (0.0 ~ 1.0)
fn compute_rms(samples: &[i16]) -> f32 {
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

/// Linear interpolation resampling (any in_rate → out_rate)
fn resample(input: &[i16], in_rate: u32, out_rate: u32) -> Vec<i16> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = (input.len() as f64 / ratio) as usize;
    let last = input.len() - 1;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = (pos as usize).min(last);
        let frac = pos - idx as f64;
        let s0 = input[idx] as f64;
        let s1 = input[(idx + 1).min(last)] as f64;
        output.push((s0 + (s1 - s0) * frac).round() as i16);
    }
    output
}

/// PCM i16 → base64 (little-endian)
fn encode_pcm16_base64(samples: &[i16]) -> String {
    let bytes: Vec<u8> = samples.iter().flat_map(|&s| s.to_le_bytes()).collect();
    STANDARD.encode(&bytes)
}
