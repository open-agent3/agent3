/// agent — Agent module entry point
///
/// Session + MemoryStore architecture: session (voice I/O + tools + memory persistence)
/// Manages AgentState (Tauri Managed State), exposes agent_start / agent_stop / agent_restart commands.
/// Audio capture (cpal) and playback (cpal) are done natively in Rust; frontend only receives energy events for UI visualization.
pub mod ambient;
pub mod audio;
pub mod memory;
pub mod memory_injection;
pub mod playback;
pub mod realtime_ws;
pub mod scheduler;
pub mod session;
pub mod subagents;
pub mod task_manager;
pub mod tools;
pub mod wakeword;
pub mod web_tools;

use session::{SessionCommand, SessionHandle};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

// ============================================================
// Managed State
// ============================================================

/// Holds session handle + subsystem handles
pub struct AgentState {
    pub session: Mutex<Option<SessionHandle>>,
    pub audio: Mutex<Option<audio::AudioHandle>>,
    pub playback: Mutex<Option<playback::PlaybackHandle>>,
    pub ambient: Mutex<Option<ambient::AmbientHandle>>,
    pub scheduler: Mutex<Option<scheduler::SchedulerHandle>>,
    /// Microphone hot-plug monitoring task
    device_watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Bridge tasks that need to be aborted on reset
    pub bridge_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            audio: Mutex::new(None),
            playback: Mutex::new(None),
            ambient: Mutex::new(None),
            scheduler: Mutex::new(None),
            device_watcher: Mutex::new(None),
            bridge_tasks: Mutex::new(Vec::new()),
        }
    }
}

pub struct BoardState(pub Mutex<Option<tools::BoardContent>>);

impl Default for BoardState {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

// ============================================================
// Manual wake (global shortcut)
// ============================================================

/// Wake agent from sleep state via external trigger (e.g. global shortcut)
pub fn wake_agent(app: &AppHandle) {
    let state = app.state::<AgentState>();
    let cmd_tx = {
        let guard = state.session.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|h| h.cmd_tx.clone())
    };
    if let Some(tx) = cmd_tx {
        let _ = tx.try_send(SessionCommand::Wake);
    }
}

// ============================================================
// Start/stop two-layer architecture
// ============================================================

/// Stop existing session + subsystems
pub async fn stop_existing(state: &AgentState) {
    let session_handle = {
        let mut guard = state.session.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    // Stop session first, then audio and ambient (let channels close naturally)
    if let Some(h) = session_handle {
        h.stop().await;
    }
    let audio_handle = {
        let mut guard = state.audio.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    drop(audio_handle);
    let playback_handle = {
        let mut guard = state.playback.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    drop(playback_handle);
    let ambient_handle = {
        let mut guard = state.ambient.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(h) = ambient_handle {
        h.stop();
    }
    let scheduler_handle = {
        let mut guard = state.scheduler.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(h) = scheduler_handle {
        h.stop();
    }
    // Stop device watcher
    let watcher = {
        let mut guard = state
            .device_watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(h) = watcher {
        h.abort();
    }
    // Stop all bridge tasks
    let bridge_tasks = {
        let mut guard = state
            .bridge_tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    for task in bridge_tasks {
        task.abort();
    }
}

/// Start agent: session (voice + tools + memory) with optional ambient/scheduler
pub async fn start_all(app: AppHandle) -> Result<(), String> {
    // 1. Read wakeword configuration
    let (wakeword_enabled, wake_model_path) = {
        let db_state = app.state::<crate::db::DbState>();
        let pool = &db_state.0;

        let enabled: Option<String> = sqlx::query_scalar(
            "SELECT value FROM app_settings WHERE key = 'wake_word_enabled'",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        let model_path: Option<String> = sqlx::query_scalar(
            "SELECT value FROM app_settings WHERE key = 'wake_word_model_path'",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        (
            enabled.map(|v| v == "true" || v == "1").unwrap_or(false),
            model_path.filter(|p: &String| !p.is_empty()),
        )
    };

    // 3. Create audio channels
    let (wake_tx, wake_rx) = mpsc::channel::<audio::WakeEvent>(8);
    let wake_tx_sched = wake_tx.clone(); // Clone for scheduler dual-path
    let (audio_tx, audio_rx) = mpsc::channel::<String>(256);
    let (playback_tx, playback_rx) = mpsc::channel::<playback::PlaybackCommand>(256);

    // 4. Create shared audio flags (lock-free cross-thread state)
    let flags = Arc::new(audio::SharedAudioFlags::new(if wakeword_enabled {
        audio::WakeState::Sleeping
    } else {
        audio::WakeState::Awakened
    }));

    // 5. Start audio capture
    let audio_handle = match audio::start(
        app.clone(),
        wake_tx,
        audio_tx,
        wakeword_enabled,
        wake_model_path,
        flags.clone(),
    ) {
        Ok(h) => h,
        Err(e) => {
            // Microphone unavailable — start device watcher, auto-reconnect when plugged in
            log::warn!(
                "[Agent] Audio start failed: {} — starting device watcher",
                e
            );
            app.emit(
                "agent-status",
                session::StatusPayload {
                    state: "waiting-for-mic".into(),
                    message: Some(crate::i18n::t("status.no_mic")),
                },
            )
            .ok();
            spawn_device_watcher(app);
            return Ok(());
        }
    };
    // Microphone ready, clear possibly lingering device watcher task
    {
        let state = app.state::<AgentState>();
        let old = state
            .device_watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = old {
            h.abort();
        }
    }

    // 6. Start audio playback (shares flags with audio capture)
    let playback_handle = playback::start(app.clone(), playback_rx, flags.clone())?;

    // 7. Create inject channel (ambient/scheduler → session)
    let (inject_tx, inject_rx) = mpsc::channel::<String>(64);

    // 8. Start session (voice I/O + direct tool execution + memory persistence)
    let session_handle = session::start(
        app.clone(),
        inject_rx,
        audio_rx,
        wake_rx,
        flags.clone(),
        playback_tx,
        wakeword_enabled,
    )
    .await?;

    // 8. Optional: start ambient awareness (screen observation via HTTP LLM)
    let ambient_handle = {
        let ambient_enabled = {
            let db_state = app.state::<crate::db::DbState>();
            let pool = &db_state.0;
            let val: Option<String> = sqlx::query_scalar(
                "SELECT value FROM app_settings WHERE key = 'ambient_enabled'",
            )
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            val.map(|v| v == "true" || v == "1").unwrap_or(false)
        };

        if ambient_enabled {
            let (ambient_tx, mut ambient_rx) = mpsc::channel::<ambient::AmbientEvent>(8);
            let handle = ambient::start(app.clone(), ambient_tx);

            // Bridge ambient events → session inject channel
            let inject_tx_ambient = inject_tx.clone();
            let ambient_bridge = tokio::spawn(async move {
                while let Some(event) = ambient_rx.recv().await {
                    match event {
                        ambient::AmbientEvent::ProactivePrompt(text) => {
                            let _ = inject_tx_ambient.send(text).await;
                        }
                        ambient::AmbientEvent::ContextUpdate(_ctx) => {
                            // Future: can be used to update session's environment context
                        }
                    }
                }
            });
            let app_state = app.state::<AgentState>();
            app_state.bridge_tasks.lock().unwrap_or_else(|e| e.into_inner()).push(ambient_bridge);

            log::info!("[Agent] Ambient watcher started");
            Some(handle)
        } else {
            log::info!("[Agent] Ambient watcher disabled");
            None
        }
    };

    // 9. Start scheduled task scheduler
    let scheduler_handle = {
        let (sched_tx, mut sched_rx) = mpsc::channel::<scheduler::SchedulerEvent>(16);
        let handle = scheduler::start(app.clone(), sched_tx);

        // Bridge scheduler events → session (dual-path based on session state)
        let inject_tx_sched = inject_tx;
        let flags_sched = flags;
        let sched_bridge = tokio::spawn(async move {
            while let Some(event) = sched_rx.recv().await {
                match event {
                    scheduler::SchedulerEvent::TaskDue { id: _, description } => {
                        let state = flags_sched.session_state.load(Ordering::Relaxed);
                        if state == 2 {
                            // Session connected: inject directly into current conversation
                            let prompt = format!(
                                "[Scheduled Reminder] You previously set a timed task, and it is now due: \"{}\". Please remind the user in a natural tone.",
                                description
                            );
                            let _ = inject_tx_sched.send(prompt).await;
                        } else {
                            // Session sleeping: wake up with task context
                            let _ = wake_tx_sched
                                .send(audio::WakeEvent::ScheduledTask(description))
                                .await;
                        }
                    }
                }
            }
        });
        let app_state = app.state::<AgentState>();
        app_state.bridge_tasks.lock().unwrap_or_else(|e| e.into_inner()).push(sched_bridge);

        log::info!("[Agent] Scheduler started");
        handle
    };

    // 10. Save handles
    let state = app.state::<AgentState>();
    {
        let mut guard = state.audio.lock().map_err(|e| e.to_string())?;
        *guard = Some(audio_handle);
    }
    {
        let mut guard = state.playback.lock().map_err(|e| e.to_string())?;
        *guard = Some(playback_handle);
    }
    {
        let mut guard = state.session.lock().map_err(|e| e.to_string())?;
        *guard = Some(session_handle);
    }
    {
        let mut guard = state.ambient.lock().map_err(|e| e.to_string())?;
        *guard = ambient_handle;
    }
    {
        let mut guard = state.scheduler.lock().map_err(|e| e.to_string())?;
        *guard = Some(scheduler_handle);
    }

    Ok(())
}

// ============================================================
// Microphone hot-plug monitoring
// ============================================================

/// Polls the system default microphone every 3 seconds, auto-triggers start_all when detected
fn spawn_device_watcher(app: AppHandle) {
    let state = app.state::<AgentState>();
    // Clear old watcher first
    let old = state
        .device_watcher
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(h) = old {
        h.abort();
    }

    let app_clone = app.clone();
    let handle = tokio::spawn(async move {
        log::info!("[Agent] Device watcher started — polling for microphone");
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
        interval.tick().await; // Skip immediate trigger

        loop {
            interval.tick().await;

            // Check device in blocking thread (cpal enumeration may block)
            let found = tokio::task::spawn_blocking(|| {
                use cpal::traits::HostTrait;
                cpal::default_host().default_input_device().is_some()
            })
            .await
            .unwrap_or(false);

            if found {
                log::info!("[Agent] Microphone detected — attempting startup");
                match start_all(app_clone.clone()).await {
                    Ok(()) => {
                        log::info!("[Agent] Auto-started after microphone reconnection");
                        app_clone
                            .emit(
                                "agent-status",
                                session::StatusPayload {
                                    state: "connected".into(),
                                    message: Some(crate::i18n::t("status.mic_connected")),
                                },
                            )
                            .ok();
                        break;
                    }
                    Err(e) => {
                        log::warn!("[Agent] Auto-start failed: {} — will retry", e);
                    }
                }
            }
        }
        log::info!("[Agent] Device watcher stopped");
    });

    let mut guard = state
        .device_watcher
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(handle);
}

// ============================================================
// Tauri Commands
// ============================================================

#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    state: tauri::State<'_, AgentState>,
) -> Result<(), String> {
    stop_existing(&state).await;
    start_all(app).await
}

#[tauri::command]
pub async fn agent_stop(state: tauri::State<'_, AgentState>) -> Result<(), String> {
    stop_existing(&state).await;
    Ok(())
}

#[tauri::command]
pub async fn agent_restart(
    app: AppHandle,
    state: tauri::State<'_, AgentState>,
) -> Result<(), String> {
    stop_existing(&state).await;
    start_all(app).await
}

#[tauri::command]
pub fn get_board_content(state: tauri::State<'_, BoardState>) -> Option<tools::BoardContent> {
    let guard = state.0.lock().ok()?;
    guard.clone()
}

/// Check if there is a valid active provider (frontend decides whether to start microphone)
#[tauri::command]
pub async fn check_config_ready(app: AppHandle) -> bool {
    let db_state = app.state::<crate::db::DbState>();
    let pool = &db_state.0;
    let count: Option<i64> = sqlx::query_scalar(
        "SELECT COUNT(*) FROM llm_providers WHERE is_active = 1 AND api_key != ''",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    count.unwrap_or(0) > 0
}

/// Switch voice and trigger WS reconnection
#[tauri::command]
pub async fn agent_switch_voice(
    app: AppHandle,
    state: tauri::State<'_, AgentState>,
    voice: String,
) -> Result<(), String> {
    // Persist to DB
    let db_state = app.state::<crate::db::DbState>();
    let pool = &db_state.0;
    sqlx::query("INSERT OR REPLACE INTO app_settings (key, value) VALUES ('voice_name', ?1)")
        .bind(&voice)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    // Send Reconnect signal to session
    let cmd_tx = {
        let guard = state.session.lock().map_err(|e| e.to_string())?;
        guard.as_ref().map(|h| h.cmd_tx.clone())
    };
    if let Some(tx) = cmd_tx {
        tx.send(session::SessionCommand::Reconnect(voice.clone()))
            .await
            .map_err(|e| e.to_string())?;
    }

    log::info!("[Agent] Voice switch requested: {}", voice);
    Ok(())
}
