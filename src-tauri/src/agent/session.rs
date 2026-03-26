/// session — Voice I/O + direct tool execution via Realtime API WebSocket
///
/// The session layer handles:
/// 1. Managing Realtime API WebSocket connection lifecycle
/// 2. Native audio I/O (cpal capture → WS → cpal playback)
/// 3. All function calling — tools are registered on the WS session and executed locally
/// 4. Persisting transcripts/tool results via MemoryStore
use crate::agent::audio::{self, WakeEvent, WakeState};
use crate::agent::memory::MemoryStore;
use crate::agent::memory_injection::{self, DefaultMemoryInjectionPolicy, InjectionScenario};
use crate::agent::playback::PlaybackCommand;
use crate::agent::{realtime_ws, subagents, task_manager, tools};
use crate::db::DbState;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use std::sync::{atomic::Ordering, Arc};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Maximum consecutive tool-triggered response rounds before forcing the model to stop calling tools
const MAX_TOOL_ROUNDS: usize = 10;
const DEFAULT_MEMORY_TOOL_EXCLUDE_RECENT_SECONDS: i64 = 2;
const MAX_MEMORY_TOOL_LOOKBACK_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_1008_RETRIES: usize = 3;

// ============================================================
// Session event-loop state (encapsulates mutable per-connection state)
// ============================================================

/// Mutable state accumulated during a single WS connection's event loop.
/// Encapsulates what was previously 9 loose `&mut` parameters to `handle_ws_message`.
struct SessionLoopState {
    /// Accumulate assistant transcript delta; persisted on ResponseDone
    transcript_buf: String,
    /// Accumulate user transcript chunks; persisted on turn boundary
    user_transcript_buf: String,
    /// Pending tool calls accumulated within a single response (batch submit on ResponseDone)
    pending_tool_calls: Vec<task_manager::TaskRequest>,
    /// Number of outstanding tools being executed
    tools_in_flight: usize,
    /// Safety: count consecutive tool-triggered response rounds to prevent infinite loops
    consecutive_tool_rounds: usize,
    /// Count user transcript turns for post-session consolidation guard
    user_turn_count: usize,
    /// Timestamp of the current user turn start (first transcript chunk),
    /// used as default cutoff for memory evidence queries.
    user_turn_started_at: Option<i64>,
    /// Some providers can emit duplicated completion markers for one logical turn.
    response_done_handled_for_turn: bool,
    /// Guard for per-connection wakeword nudge check to avoid repeated DB queries.
    wakeword_nudge_checked: bool,
}

impl SessionLoopState {
    fn new() -> Self {
        Self {
            transcript_buf: String::new(),
            user_transcript_buf: String::new(),
            pending_tool_calls: Vec::new(),
            tools_in_flight: 0,
            consecutive_tool_rounds: 0,
            user_turn_count: 0,
            user_turn_started_at: None,
            response_done_handled_for_turn: false,
            wakeword_nudge_checked: false,
        }
    }

    /// Flush accumulated user transcript to memory persistence.
    async fn flush_user_transcript(&mut self, memory: &MemoryStore) {
        if self.user_transcript_buf.trim().is_empty() {
            self.user_transcript_buf.clear();
            return;
        }
        let text = std::mem::take(&mut self.user_transcript_buf);
        if let Err(e) = memory.persist("user", &text).await {
            log::error!("[Session] Failed to persist user text: {}", e);
        }
        self.user_turn_count += 1;
    }
}

// ============================================================
// Event payloads (emitted to frontend)
// ============================================================

#[derive(Clone, Serialize)]
pub struct TranscriptPayload {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
pub struct StatusPayload {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ============================================================
// Control commands
// ============================================================

pub enum SessionCommand {
    Stop,
    /// Switch voice and reconnect WebSocket
    Reconnect(String),
    /// Graceful disconnect after AI farewell
    Disconnect,
    /// Wake from sleep (global shortcut)
    Wake,
}

// ============================================================
// Session Handle — holds channel senders and task handle
// ============================================================

pub struct SessionHandle {
    pub cmd_tx: mpsc::Sender<SessionCommand>,
    task: tokio::task::JoinHandle<()>,
}

impl SessionHandle {
    pub async fn stop(self) {
        let _ = self.cmd_tx.send(SessionCommand::Stop).await;
        let _ = self.task.await;
    }
}

/// Build the full list of tools to register on the Realtime WS session.
/// Combines all_tools (system tools) + change_voice tool.
fn build_ws_tools(wakeword_enabled: bool) -> Vec<Value> {
    let mut all_tools = tools::all_tools_realtime_json();
    if !wakeword_enabled {
        all_tools.retain(|t| t["name"].as_str() != Some("disconnect_session"));
    }
    // Add change_voice tool (handled specially in pipeline, not dispatched via task_manager)
    all_tools.push(serde_json::json!({
        "type": "function",
        "name": "change_voice",
        "description": "Call when the user asks to change your voice, tone, or timbre. Examples: 'switch to a male voice', 'use a deeper voice'.",
        "parameters": {
            "type": "object",
            "properties": {
                "voice_name": {
                    "type": "string",
                    "description": "Target voice name"
                }
            },
            "required": ["voice_name"]
        }
    }));
    all_tools
}

// ============================================================
// Start session
// ============================================================

pub async fn start(
    app: AppHandle,
    inject_tx: mpsc::Sender<String>,
    inject_rx: mpsc::Receiver<String>,
    audio_rx: mpsc::Receiver<String>,
    wake_rx: mpsc::Receiver<WakeEvent>,
    flags: Arc<audio::SharedAudioFlags>,
    playback_tx: mpsc::Sender<PlaybackCommand>,
    wakeword_enabled: bool,
) -> Result<SessionHandle, String> {
    // Read realtime provider from DB
    let db_state = app.state::<DbState>();
    let pool = &db_state.0;

    let provider = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "SELECT id, name, base_url, api_key, model, provider_type FROM llm_providers WHERE is_active = 1 AND role IN ('realtime', 'sensory') LIMIT 1"
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let v: Option<String> = sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'voice_name'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

    let onboarded: Option<String> = sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'voice_onboarded'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

    let (provider_info, voice, is_voice_onboarded) = (
        provider,
        v.unwrap_or_default(),
        onboarded.map(|o| o == "true" || o == "1").unwrap_or(false),
    );

    let (provider_id, name, base_url, raw_api_key, model, provider_type_str) = match provider_info {
        Some(p) => p,
        None => {
            if let Err(e) = app.emit(
                "agent-status",
                StatusPayload {
                    state: "no-provider".into(),
                    message: Some(crate::i18n::t("status.no_provider")),
                },
            ) {
                log::warn!("[Session] Emit event error: {}", e);
            }
            return Err("No active realtime provider configured".into());
        }
    };

    let api_key = crate::keystore::resolve_api_key(&provider_id, &raw_api_key);

    if api_key.is_empty() {
        if let Err(e) = app.emit(
            "agent-status",
            StatusPayload {
                state: "no-provider".into(),
                message: Some(crate::i18n::t("status.no_api_key")),
            },
        ) {
            log::warn!("[Session] Emit event error: {}", e);
        }
        return Err("API key is empty".into());
    }

    let protocol = realtime_ws::protocol_for(&provider_type_str);

    // Build full tool list for the WS session (all system tools + change_voice)
    let ws_tools = if protocol.supports_function_calling() {
        build_ws_tools(wakeword_enabled)
    } else {
        vec![]
    };

    log::info!(
        "[Session] Starting with provider: {} (type={}, model={}, tools={}, voice={})",
        name,
        provider_type_str,
        model,
        ws_tools.len(),
        if voice.is_empty() {
            "(default)"
        } else {
            &voice
        },
    );

    let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(1024);

    let task = tokio::spawn(session_loop(
        app.clone(),
        protocol,
        api_key,
        base_url,
        model,
        voice,
        is_voice_onboarded,
        ws_tools,
        audio_rx,
        cmd_rx,
        inject_tx,
        inject_rx,
        wake_rx,
        flags,
        playback_tx,
    ));

    Ok(SessionHandle { cmd_tx, task })
}

// ============================================================
// Core event loop
// ============================================================

/// Event loop exit reason
enum LoopExit {
    /// WS disconnected (needs reconnection)
    Disconnected,
    /// WS disconnected due to Gemini 1008 (audio+tool conflict) — needs retry with tool-only hint
    Disconnected1008,
    /// Received Stop command
    Stopped,
    /// Wake timeout (return to sleep); carries user turn count for consolidation
    WakeTimeout(usize),
    /// Voice switch reconnection (carries new voice name)
    Reconnect(String),
}

/// How we're connecting this iteration — determines greeting + context injection behavior
#[derive(Clone, PartialEq)]
enum ConnectMode {
    /// New session: wakeword or scheduled task. Greeting + system-instruction summary only.
    NewSession,
    /// Voice switch: greeting FIRST (clean context), then inject raw history for follow-up.
    VoiceSwitch,
    /// WS error reconnect: inject raw history only, no greeting (seamless continuation).
    SilentReconnect,
    /// Retry after Gemini 1008 crash: inject history + tool-only hint to complete the failed action.
    ToolRetry,
}

#[allow(clippy::too_many_arguments)]
async fn session_loop(
    app: AppHandle,
    protocol: Box<dyn realtime_ws::RealtimeProtocol>,
    api_key: String,
    base_url: String,
    model: String,
    mut voice: String,
    mut is_voice_onboarded: bool,
    ws_tools: Vec<Value>,
    mut audio_rx: mpsc::Receiver<String>,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    inject_tx: mpsc::Sender<String>,
    mut inject_rx: mpsc::Receiver<String>,
    mut wake_rx: mpsc::Receiver<WakeEvent>,
    flags: Arc<audio::SharedAudioFlags>,
    playback_tx: mpsc::Sender<PlaybackCommand>,
) {
    let memory = MemoryStore::new(app.state::<crate::db::DbState>().0.clone());
    let injection_policy = DefaultMemoryInjectionPolicy;
    let (subagent_event_tx, mut subagent_event_rx) = mpsc::channel::<subagents::SubagentEvent>(1024);
    let subagent_mgr = subagents::SubagentManager::new(app.clone(), subagent_event_tx);
    let mut first_boot = true;
    'outer: loop {
        // ── Phase 1: Wait for wake signal ──
        let mut connect_mode = ConnectMode::NewSession;
        let mut scheduled_task_desc: Option<String> = None;
        let mut crash_1008_retries: usize = 0;
        flags.session_state.store(0, Ordering::Relaxed); // 0 = sleeping

        // On first boot, skip waiting for wakeword — connect immediately
        if first_boot {
            first_boot = false;
            log::info!("[Session] First boot — connecting immediately");
        } else {
            log::info!("[Session] Waiting for wake event\u{2026}");
            if let Err(e) = app.emit(
                "agent-status",
                StatusPayload {
                    state: "sleeping".into(),
                    message: None,
                },
            ) {
                log::warn!("[Session] Emit event error: {}", e);
            }

            loop {
                tokio::select! {
                    wake = wake_rx.recv() => {
                        match wake {
                            Some(WakeEvent::Detected) => {
                                log::info!("[Session] Wake event received, connecting\u{2026}");
                                break;
                            }
                            Some(WakeEvent::ScheduledTask(desc)) => {
                                log::info!("[Session] Scheduled task wake: {}", desc);
                                scheduled_task_desc = Some(desc);
                                break;
                            }
                            Some(WakeEvent::Timeout) => {} // Timeout in waiting phase is meaningless, ignore
                            None => break 'outer,
                        }
                    }
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(SessionCommand::Stop) | None => break 'outer,
                            Some(SessionCommand::Reconnect(new_voice)) => {
                                // Voice switch while sleeping — wake up and connect with new voice
                                log::info!("[Session] Voice switch while sleeping: {}", new_voice);
                                voice = new_voice;
                                connect_mode = ConnectMode::VoiceSwitch;
                                break;
                            }
                            Some(SessionCommand::Wake) => {
                                log::info!("[Session] Manual wake (global shortcut)");
                                break;
                            }
                            Some(SessionCommand::Disconnect) => {} // Already sleeping, ignore
                        }
                    }
                }
            }
        } // end else (not first_boot)

        // ── Phase 2: Connect + event loop (with auto-reconnection) ──
        'conn: loop {
            let ws = match realtime_ws::connect(protocol.as_ref(), &api_key, &base_url, &model)
                .await
            {
                Ok(ws) => ws,
                Err(e) => {
                    log::error!("[Session] Connect failed: {}", e);
                    if let Err(e) = app.emit(
                        "agent-status",
                        StatusPayload {
                            state: "error".into(),
                            message: Some(format!(
                                "{}: {}",
                                crate::i18n::t("status.connection_failed"),
                                e
                            )),
                        },
                    ) {
                        log::warn!("[Session] Emit event error: {}", e);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => continue 'conn,
                        cmd = cmd_rx.recv() => {
                            if matches!(cmd, Some(SessionCommand::Stop) | None) { break 'outer; }
                            continue 'conn;
                        }
                        wake = wake_rx.recv() => {
                            if matches!(wake, Some(WakeEvent::Timeout)) { break 'conn; }
                            continue 'conn;
                        }
                    }
                }
            };

            let (mut ws_sink, mut ws_stream) = ws.split();

            // Send session.update / setup
            let mut instructions = memory.build_instructions().await.unwrap_or_default();
            let voice_prompt = protocol.supported_voices_prompt();
            if !voice_prompt.is_empty() {
                instructions.push_str("\n\nVOICE CAPABILITIES:\n");
                instructions.push_str(&voice_prompt);
            }

            let session_update =
                protocol.build_session_update(&instructions, &ws_tools, &model, &voice);
            let setup_preview: String = session_update.to_string().chars().take(300).collect();
            log::info!("[Session] Sending setup: {}...", setup_preview);
            if let Err(e) = ws_sink
                .send(Message::Text(session_update.to_string().into()))
                .await
            {
                log::error!("[Session] Failed to send session.update: {}", e);
                continue 'conn;
            }

            // Gemini requires waiting for setupComplete before sending data
            if protocol.requires_setup_ack() {
                let ack_timeout = tokio::time::sleep(std::time::Duration::from_secs(10));
                tokio::pin!(ack_timeout);
                let mut got_ack = false;
                loop {
                    tokio::select! {
                        _ = &mut ack_timeout => {
                            log::error!("[Session] Timed out waiting for setupComplete");
                            break;
                        }
                        ws_msg = ws_stream.next() => {
                            let raw_text = match &ws_msg {
                                Some(Ok(Message::Text(t))) => Some(t.to_string()),
                                Some(Ok(Message::Binary(b))) => std::str::from_utf8(b).ok().map(|s| s.to_string()),
                                _ => None,
                            };
                            if let Some(raw) = raw_text {
                                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                                    if v.get("setupComplete").is_some() {
                                        log::info!("[Session] Received setupComplete");
                                        got_ack = true;
                                        break;
                                    }
                                    log::debug!("[Session] While waiting for setupComplete, got: {}", &raw[..raw.len().min(120)]);
                                }
                            } else if let Some(Ok(Message::Close(close_frame))) = &ws_msg {
                                log::warn!("[Session] WebSocket closed while waiting for setupComplete. Frame: {:?}", close_frame);
                                break;
                            } else if ws_msg.is_none() {
                                log::warn!("[Session] WebSocket closed while waiting for setupComplete (None)");
                                break;
                            }
                        }
                        cmd = cmd_rx.recv() => {
                            if matches!(cmd, Some(SessionCommand::Stop) | None) { break 'outer; }
                        }
                    }
                }
                if !got_ack {
                    continue 'conn;
                }
            }

            // Switch state to Listening
            flags
                .wake_state
                .store(WakeState::Listening as u8, Ordering::Relaxed);
            flags.session_state.store(2, Ordering::Relaxed); // 2 = connected
            app.emit("agent-wake-state", "listening").ok();

            if let Err(e) = app.emit(
                "agent-status",
                StatusPayload {
                    state: "connected".into(),
                    message: None,
                },
            ) {
                log::warn!("[Session] Emit event error: {}", e);
            }

            // Build provider-agnostic memory context pack based on scenario,
            // then let protocol encode it into provider-specific wire messages.
            let scenario = match connect_mode {
                ConnectMode::VoiceSwitch => {
                    let greeting = memory.contextual_greeting("voice_switch", None);
                    InjectionScenario::VoiceSwitch { greeting }
                }
                ConnectMode::SilentReconnect => InjectionScenario::SilentReconnect { max_turns: 3 },
                ConnectMode::ToolRetry => InjectionScenario::ToolRetry {
                    max_turns: 3,
                    retry_hint: "[System] Your previous response was interrupted because you tried to speak and call a tool at the same time. This time, ONLY execute the tool/function call silently — do NOT generate any spoken audio. After the tool result comes back, you may speak to the user.".to_string(),
                },
                ConnectMode::NewSession => {
                    let greeting = if !is_voice_onboarded {
                        // Mark as onboarded in memory so we don't ask again if we reconnect in this session
                        is_voice_onboarded = true;

                        "First boot. Give a warm one-to-one introduction. Avoid group/broadcast-style wording. Tell the user you can switch voice and ask whether they want to switch now. WAIT for the user to answer. DO NOT call any tool in this response. In FUTURE conversational turns, if they say yes, call `change_voice`; if no, call `set_agent_config` with key 'voice_onboarded' and value 'true'."
                        .to_string()
                    } else if let Some(ref desc) = scheduled_task_desc {
                        memory.contextual_greeting("scheduled_task", Some(desc))
                    } else {
                        memory.contextual_greeting("wakeword", None)
                    };
                    InjectionScenario::NewSession { greeting }
                }
            };

            let pack = injection_policy.build_pack(&memory, scenario).await;
            let encoded = memory_injection::encode_pack(protocol.as_ref(), &pack);

            if encoded.timeline_items > 0 {
                log::info!(
                    "[Session] Context injection: timeline items={}, dropped={}, provider_supports_timeline={}, continuity_fallback={}",
                    encoded.timeline_items,
                    encoded.dropped_timeline_items,
                    protocol.supports_timeline_injection(),
                    if encoded.dropped_timeline_items > 0 && !protocol.supports_timeline_injection() {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
            }

            for msg in encoded.messages {
                if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                    log::error!("[Session] Failed to inject memory context: {}", e);
                    continue 'conn;
                }
            }

            // Create task manager for tool execution
            let (task_event_tx, task_event_rx) = mpsc::channel::<task_manager::TaskEvent>(1024);
            let task_mgr = task_manager::TaskManager::new(app.clone(), task_event_tx, inject_tx.clone());

            // Main event loop
            let exit = run_event_loop(
                &app,
                protocol.as_ref(),
                &mut ws_sink,
                &mut ws_stream,
                &mut audio_rx,
                &mut cmd_rx,
                &memory,
                &mut inject_rx,
                &mut wake_rx,
                &playback_tx,
                task_mgr,
                task_event_rx,
                &flags,
                &subagent_mgr,
                &mut subagent_event_rx,
            )
            .await;

            if !matches!(exit, LoopExit::Disconnected1008) {
                crash_1008_retries = 0;
            }

            // Close WS — clear speaking flag so mic reopens
            flags.is_ai_speaking.store(false, Ordering::Relaxed);
            flags.session_state.store(0, Ordering::Relaxed); // 0 = sleeping
            let _ = ws_sink.close().await;
            log::info!("[Session] Disconnected");

            // Emit lifecycle event
            let reason = match &exit {
                LoopExit::WakeTimeout(_) => "timeout",
                LoopExit::Stopped => "stopped",
                LoopExit::Reconnect(_) => "voice_switch",
                LoopExit::Disconnected => "disconnected",
                LoopExit::Disconnected1008 => "disconnected_1008",
            };
            app.emit(
                "session-lifecycle",
                serde_json::json!({
                    "event": "disconnected",
                    "reason": reason,
                }),
            )
            .ok();

            match exit {
                LoopExit::WakeTimeout(user_turns) => {
                    log::info!(
                        "[Session] Wake timeout — returning to sleep (user_turns={})",
                        user_turns
                    );

                    // Post-session memory consolidation: if meaningful conversation happened,
                    // spawn a background subagent to extract key facts
                    if user_turns >= 3 {
                        // Check privacy gate: user can disable consolidation
                        let consolidation_enabled = {
                            let pool = &app.state::<DbState>().0;
                            sqlx::query_scalar::<_, String>(
                                "SELECT value FROM app_settings WHERE key = 'memory_consolidation_enabled'",
                            )
                            .fetch_optional(pool)
                            .await
                            .ok()
                            .flatten()
                            .map(|v| v != "false" && v != "0")
                            .unwrap_or(true) // default: enabled
                        };

                        if !consolidation_enabled {
                            log::info!("[Session] Memory consolidation disabled by user setting");
                        } else {
                            let context = memory.recent_context(20).await.unwrap_or_default();
                            if !context.is_empty() {
                                let conversation_text: String = context
                                    .iter()
                                    .filter(|(role, _)| role == "user" || role == "assistant")
                                    .map(|(role, content)| {
                                        let label = if role == "user" { "User" } else { "Assistant" };
                                        format!("{}: {}", label, content)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let goal = format!(
                                    "Review this conversation and extract key facts worth remembering about the user. \
                                     For each important fact (personal info, preferences, habits, requests), \
                                     call remember_fact with a concise statement. \
                                     Only store NEW information — skip greetings and small talk.\n\n\
                                     Conversation:\n{}",
                                    conversation_text
                                );
                                match subagent_mgr.spawn(&goal).await {
                                    Ok(task_id) => log::info!(
                                        "[Session] Spawned memory consolidation subagent: {}",
                                        task_id
                                    ),
                                    Err(e) => log::warn!(
                                        "[Session] Failed to spawn consolidation subagent: {}",
                                        e
                                    ),
                                }
                            }
                        }
                    }

                    break 'conn; // Return to Phase 1 to wait for wake
                }
                LoopExit::Stopped => break 'outer,
                LoopExit::Reconnect(new_voice) => {
                    log::info!("[Session] Switching voice to: {}", new_voice);
                    if let Err(e) = app.emit(
                        "agent-status",
                        StatusPayload {
                            state: "switching-voice".into(),
                            message: Some(format!(
                                "{}: {}",
                                crate::i18n::t("status.switching_voice"),
                                new_voice
                            )),
                        },
                    ) {
                        log::warn!("[Session] Emit event error: {}", e);
                    }
                    voice = new_voice;
                    connect_mode = ConnectMode::VoiceSwitch;
                    continue 'conn; // Reconnect directly with new voice
                }
                LoopExit::Disconnected => {
                    if let Err(e) = app.emit(
                        "agent-status",
                        StatusPayload {
                            state: "disconnected".into(),
                            message: Some(crate::i18n::t("status.reconnecting")),
                        },
                    ) {
                        log::warn!("[Session] Emit event error: {}", e);
                    }
                    log::info!("[Session] Disconnected, reconnecting in 3s\u{2026}");
                    connect_mode = ConnectMode::SilentReconnect;
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => continue 'conn,
                        cmd = cmd_rx.recv() => {
                            if matches!(cmd, Some(SessionCommand::Stop) | None) { break 'outer; }
                            continue 'conn;
                        }
                        wake = wake_rx.recv() => {
                            if matches!(wake, Some(WakeEvent::Timeout)) { break 'conn; }
                            continue 'conn;
                        }
                    }
                }
                LoopExit::Disconnected1008 => {
                    crash_1008_retries = crash_1008_retries.saturating_add(1);
                    if crash_1008_retries > MAX_1008_RETRIES {
                        log::error!(
                            "[Session] Gemini 1008 crash exceeded retry limit ({}), returning to sleep",
                            MAX_1008_RETRIES
                        );
                        if let Err(e) = app.emit(
                            "agent-status",
                            StatusPayload {
                                state: "error".into(),
                                message: Some(crate::i18n::t("status.connection_failed")),
                            },
                        ) {
                            log::warn!("[Session] Emit event error: {}", e);
                        }
                        break 'conn;
                    }

                    let backoff_secs = (1u64 << crash_1008_retries.min(4))
                        .saturating_mul(2)
                        .min(16);
                    log::warn!(
                        "[Session] Gemini 1008 crash (audio+tool conflict) — retrying with tool-only mode in {}s (attempt {}/{})",
                        backoff_secs,
                        crash_1008_retries,
                        MAX_1008_RETRIES
                    );
                    if let Err(e) = app.emit(
                        "agent-status",
                        StatusPayload {
                            state: "disconnected".into(),
                            message: Some(crate::i18n::t("status.reconnecting")),
                        },
                    ) {
                        log::warn!("[Session] Emit event error: {}", e);
                    }
                    connect_mode = ConnectMode::ToolRetry;
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => continue 'conn,
                        cmd = cmd_rx.recv() => {
                            if matches!(cmd, Some(SessionCommand::Stop) | None) { break 'outer; }
                            continue 'conn;
                        }
                    }
                }
            }
        }
    }

    // Session fully stopped — abort all background subagents
    subagent_mgr.abort_all();

    if let Err(e) = app.emit(
        "agent-status",
        StatusPayload {
            state: "stopped".into(),
            message: None,
        },
    ) {
        log::warn!("[Session] Emit event error: {}", e);
    }
    log::info!("[Session] Stopped");
}

/// Event loop core — handles audio I/O, WS events, tool execution, and inject commands
#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    app: &AppHandle,
    protocol: &dyn realtime_ws::RealtimeProtocol,
    ws_sink: &mut futures_util::stream::SplitSink<realtime_ws::WsStream, Message>,
    ws_stream: &mut futures_util::stream::SplitStream<realtime_ws::WsStream>,
    audio_rx: &mut mpsc::Receiver<String>,
    cmd_rx: &mut mpsc::Receiver<SessionCommand>,
    memory: &MemoryStore,
    inject_rx: &mut mpsc::Receiver<String>,
    wake_rx: &mut mpsc::Receiver<WakeEvent>,
    playback_tx: &mpsc::Sender<PlaybackCommand>,
    task_mgr: task_manager::TaskManager,
    mut task_rx: mpsc::Receiver<task_manager::TaskEvent>,
    flags: &Arc<audio::SharedAudioFlags>,
    subagent_mgr: &subagents::SubagentManager,
    subagent_rx: &mut mpsc::Receiver<subagents::SubagentEvent>,
) -> LoopExit {
    let mut state = SessionLoopState::new();

    loop {
        tokio::select! {
            // Native Rust audio → WS
            audio = audio_rx.recv() => {
                match audio {
                    Some(base64) => {
                        let msg = protocol.audio_append_msg(&base64);
                        if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                            log::error!("[Session] Failed to send audio: {}", e);
                            return LoopExit::Disconnected;
                        }
                    }
                    None => {
                        state.flush_user_transcript(memory).await;
                        return LoopExit::Stopped;
                    }
                }
            }

            // Wake timeout
            wake = wake_rx.recv() => {
                if matches!(wake, Some(WakeEvent::Timeout)) {
                    task_mgr.abort_all();
                    state.flush_user_transcript(memory).await;
                    return LoopExit::WakeTimeout(state.user_turn_count);
                }
            }

            // WS message — dispatch
            ws_msg = ws_stream.next() => {
                match ws_msg {
                    Some(Ok(Message::Text(raw))) => {
                        let preview: String = raw.chars().take(200).collect();
                        log::debug!("[Session] WS Text: {}", preview);
                        let (replies, exit) = state.handle_ws_message(
                            app, protocol, memory, playback_tx,
                            &raw, &task_mgr, flags, subagent_mgr,
                        ).await;
                        for reply in replies {
                            if let Err(e) = ws_sink.send(Message::Text(reply.into())).await {
                                log::error!("[Session] Failed to send reply: {}", e);
                                return LoopExit::Disconnected;
                            }
                        }
                        if let Some(e) = exit {
                            return e;
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        log::debug!("[Session] WS Binary frame: {} bytes", data.len());
                        if let Ok(text) = std::str::from_utf8(&data) {
                            let (replies, exit) = state.handle_ws_message(
                                app, protocol, memory, playback_tx,
                                text, &task_mgr, flags, subagent_mgr,
                            ).await;
                            for reply in replies {
                                if let Err(e) = ws_sink.send(Message::Text(reply.into())).await {
                                    log::error!("[Session] Failed to send reply: {}", e);
                                    return LoopExit::Disconnected;
                                }
                            }
                            if let Some(e) = exit {
                                state.flush_user_transcript(memory).await;
                                return e;
                            }
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(f) = frame.as_ref() {
                            log::info!("[Session] WebSocket closed: code={}, reason={}", f.code, f.reason);
                            // Gemini 1008 (Policy): audio + tool call conflict
                            if f.code == tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy {
                                state.flush_user_transcript(memory).await;
                                return LoopExit::Disconnected1008;
                            }
                        } else {
                            log::info!("[Session] WebSocket closed (no close frame)");
                        }
                        state.flush_user_transcript(memory).await;
                        return LoopExit::Disconnected;
                    }
                    None => {
                        log::info!("[Session] WebSocket stream ended");
                        state.flush_user_transcript(memory).await;
                        return LoopExit::Disconnected;
                    }
                    Some(Err(e)) => {
                        log::error!("[Session] WebSocket error: {}", e);
                        if let Err(e) = app.emit("agent-status", StatusPayload {
                            state: "error".into(),
                            message: Some(format!("{}", e)),
                        }) { log::warn!("[Session] Emit event error: {}", e); }
                        state.flush_user_transcript(memory).await;
                        return LoopExit::Disconnected;
                    }
                    _ => {}
                }
            }

            // Tool execution results — send back to WS as function_call_output
            task_event = task_rx.recv() => {
                match task_event {
                    Some(task_manager::TaskEvent::Completed(result)) => {
                        log::info!("[Session] Tool {} completed: {} bytes", result.tool_name, result.output.len());

                        // Persist tool result to memory
                        if let Err(e) = memory.persist_tool_result(&result.tool_name, &result.output).await {
                            log::error!("[Session] Failed to persist tool result: {}", e);
                        }

                        // Send function_call_output back to WS
                        let output_msg = protocol.function_call_output_msg(
                            &result.call_id, &result.tool_name, &result.output,
                        );
                        if let Err(e) = ws_sink.send(Message::Text(output_msg.into())).await {
                            log::error!("[Session] Failed to send function_call_output: {}", e);
                            return LoopExit::Disconnected;
                        }

                        state.tools_in_flight = state.tools_in_flight.saturating_sub(1);

                        // When all tools for this batch complete, trigger response generation
                        if state.tools_in_flight == 0 {
                            state.consecutive_tool_rounds += 1;
                            if state.consecutive_tool_rounds >= MAX_TOOL_ROUNDS {
                                log::warn!("[Session] Reached max tool rounds ({}), forcing final response", MAX_TOOL_ROUNDS);
                            } else {
                                log::info!("[Session] Tool round {}/{} complete, triggering next response", state.consecutive_tool_rounds, MAX_TOOL_ROUNDS);
                            }
                            if let Some(resp) = protocol.response_create_msg() {
                                if let Err(e) = ws_sink.send(Message::Text(resp.into())).await {
                                    log::error!("[Session] Failed to send response.create: {}", e);
                                    return LoopExit::Disconnected;
                                }
                            }
                        }
                    }
                    Some(task_manager::TaskEvent::Progress { call_id, message }) => {
                        log::debug!("[Session] Task {} progress: {} bytes", call_id, message.len());
                    }
                    None => {}
                }
            }

            // Inject speech commands (from ambient/scheduler)
            inject_text = inject_rx.recv() => {
                match inject_text {
                    Some(text) => {
                        log::info!("[Session] Injecting speech ({} chars)", text.len());
                        let msg = protocol.inject_speech_msg(&text);
                        if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                            log::error!("[Session] Failed to inject speech: {}", e);
                            return LoopExit::Disconnected;
                        }
                    }
                    None => {
                        // inject channel dropped — keep running
                    }
                }
            }

            // Subagent events (background task notifications → inject into voice)
            subagent_event = subagent_rx.recv() => {
                match subagent_event {
                    Some(subagents::SubagentEvent::AskUser { task_id, question }) => {
                        log::info!("[Session] Subagent {} asking user: {}", task_id, question);
                        let prompt = format!(
                            "[System: To complete your current task, ask the user the following question naturally: {}]",
                            question
                        );
                        let msg = protocol.inject_speech_msg(&prompt);
                        if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                            log::error!("[Session] Failed to inject subagent question: {}", e);
                            return LoopExit::Disconnected;
                        }
                    }
                    Some(subagents::SubagentEvent::Completed { task_id, summary }) => {
                        log::info!("[Session] Subagent {} completed: {}", task_id, summary);
                        let prompt = format!(
                            "[System: The operation is complete. Tell the user exactly what was done based on this result: {}]",
                            summary
                        );
                        let msg = protocol.inject_speech_msg(&prompt);
                        if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                            log::error!("[Session] Failed to inject subagent completion: {}", e);
                            return LoopExit::Disconnected;
                        }
                    }
                    Some(subagents::SubagentEvent::Failed { task_id, error }) => {
                        log::info!("[Session] Subagent {} failed: {}", task_id, error);
                        let prompt = format!(
                            "[System: The operation failed with error: {}. Apologize and explain briefly. DO NOT mention 'subagent' or 'background task'.]",
                            error
                        );
                        let msg = protocol.inject_speech_msg(&prompt);
                        if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                            log::error!("[Session] Failed to inject subagent failure: {}", e);
                            return LoopExit::Disconnected;
                        }
                    }
                    None => {}
                }
            }

            // Control commands
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Stop) | None => {
                        task_mgr.abort_all();
                        subagent_mgr.abort_all();
                        state.flush_user_transcript(memory).await;
                        return LoopExit::Stopped;
                    }
                    Some(SessionCommand::Reconnect(new_voice)) => {
                        task_mgr.abort_all();
                        state.flush_user_transcript(memory).await;
                        return LoopExit::Reconnect(new_voice);
                    }
                    Some(SessionCommand::Disconnect) => {
                        log::info!("[Session] Disconnect requested, waiting for AI farewell...");
                        // Wait up to 3s for AI to finish speaking, then disconnect
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        state.flush_user_transcript(memory).await;
                        return LoopExit::WakeTimeout(state.user_turn_count); // Return to sleeping
                    }
                    Some(SessionCommand::Wake) => {} // Already connected, ignore
                }
            }
        }
    }
}

impl SessionLoopState {
    /// Handle a single WS message — audio playback + transcript forwarding + tool call dispatch
    /// Returns (replies, optional_exit): replies need to be sent back via WS, optional_exit carries reconnect signal when change_voice is triggered
    #[allow(clippy::too_many_arguments)]
    async fn handle_ws_message(
        &mut self,
        app: &AppHandle,
        protocol: &dyn realtime_ws::RealtimeProtocol,
        memory: &MemoryStore,
        playback_tx: &mpsc::Sender<PlaybackCommand>,
        raw: &str,
        task_mgr: &task_manager::TaskManager,
        flags: &Arc<audio::SharedAudioFlags>,
        subagent_mgr: &subagents::SubagentManager,
    ) -> (Vec<String>, Option<LoopExit>) {
    let events = protocol.parse_events(raw);
    let mut replies: Vec<String> = Vec::new();

    if events.is_empty() {
        return (replies, None);
    }

    for event in events {
        match event {
            realtime_ws::WsEvent::AudioDelta(base64) => {
                if let Err(e) = playback_tx.try_send(PlaybackCommand::Enqueue(base64)) {
                    log::warn!("[Session] Failed to send audio to playback: {}", e);
                }
            }
            realtime_ws::WsEvent::AudioDone => {}
            realtime_ws::WsEvent::ResponseStart => {
                self.response_done_handled_for_turn = false;
                // User turn ended. Persist one consolidated user message before assistant reply starts.
                self.flush_user_transcript(memory).await;
                // Mark AI as speaking — suppresses mic immediately via is_ai_speaking flag
                flags.is_ai_speaking.store(true, Ordering::Relaxed);
                // Clear server-side audio buffer to discard any echo that leaked before local gating
                if let Some(clear_msg) = protocol.input_audio_clear_msg() {
                    replies.push(clear_msg);
                }
                // New reply starting: fade out previous audio (200ms) instead of hard cut
                let _ = playback_tx.try_send(PlaybackCommand::FadeOut(200));
            }
            realtime_ws::WsEvent::Transcript(text) => {
                log::info!("[Session] 🤖 Assistant: {}", text);
                self.transcript_buf.push_str(&text);
                if let Err(e) = app.emit(
                    "agent-transcript",
                    TranscriptPayload {
                        role: "assistant".into(),
                        text: text.clone(),
                    },
                ) {
                    log::warn!("[Session] Emit event error: {}", e);
                }
            }
            realtime_ws::WsEvent::TranscriptDone(text) => {
                self.transcript_buf.clear();
                if let Err(e) = memory.persist("assistant", &text).await {
                    log::error!("[Session] Failed to persist assistant text: {}", e);
                }

                // Show a one-time wakeword setup reminder after the first successful
                // user<->assistant exchange when setup was previously skipped.
                if self.user_turn_count > 0
                    && !text.trim().is_empty()
                    && !self.wakeword_nudge_checked
                {
                    self.wakeword_nudge_checked = true;
                    let pool = &app.state::<DbState>().0;
                    let wake_enabled: Option<String> = sqlx::query_scalar(
                        "SELECT value FROM app_settings WHERE key = 'wake_word_enabled'",
                    )
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten();
                    let setup_skipped: Option<String> = sqlx::query_scalar(
                        "SELECT value FROM app_settings WHERE key = 'wakeword_setup_skipped'",
                    )
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten();
                    let nudge_shown: Option<String> = sqlx::query_scalar(
                        "SELECT value FROM app_settings WHERE key = 'wakeword_nudge_shown'",
                    )
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten();

                    let wakeword_enabled = wake_enabled
                        .as_deref()
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    let skipped = setup_skipped
                        .as_deref()
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    let shown = nudge_shown
                        .as_deref()
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);

                    if !wakeword_enabled && skipped && !shown {
                        if let Err(e) = app.emit(
                            "agent-status",
                            StatusPayload {
                                state: "wakeword-nudge".into(),
                                message: Some(crate::i18n::t("status.wakeword_nudge")),
                            },
                        ) {
                            log::warn!("[Session] Emit wakeword nudge error: {}", e);
                        }
                        if let Err(e) = sqlx::query(
                            "INSERT INTO app_settings (key, value) VALUES ('wakeword_nudge_shown', 'true') \
                             ON CONFLICT(key) DO UPDATE SET value='true'",
                        )
                        .execute(pool)
                        .await
                        {
                            log::warn!("[Session] Failed to persist wakeword_nudge_shown: {}", e);
                        }
                    }
                }
            }
            realtime_ws::WsEvent::InputTranscript(text) => {
                if text.trim().is_empty() {
                    log::debug!("[Session] Ignored blank user transcript chunk");
                    continue;
                }
                self.response_done_handled_for_turn = false;
                log::info!("[Session] 🎤 User: {}", text);
                // New user speech resets the tool round counter
                self.consecutive_tool_rounds = 0;
                if self.user_transcript_buf.is_empty() && !text.trim().is_empty() {
                    self.user_turn_started_at = Some(chrono::Utc::now().timestamp());
                }
                if let Err(e) = app.emit(
                    "agent-transcript",
                    TranscriptPayload {
                        role: "user".into(),
                        text: text.clone(),
                    },
                ) {
                    log::warn!("[Session] Emit event error: {}", e);
                }
                // Merge incremental transcription chunks and defer persistence until turn end.
                merge_user_transcript_chunk(&mut self.user_transcript_buf, &text);
            }
            realtime_ws::WsEvent::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let mut effective_arguments = arguments;
                if name == "change_voice" {
                    let args: Value =
                        serde_json::from_str(&effective_arguments).unwrap_or(serde_json::json!({}));
                    let voice_name = args["voice_name"].as_str().unwrap_or("").to_string();
                    log::info!("[Session] Change voice requested: {}", voice_name);

                    // Validate voice name against known providers
                    let valid = protocol.is_valid_voice(&voice_name);
                    if !valid {
                        log::warn!("[Session] Rejected invalid voice name: {}", voice_name);
                        replies.push(protocol.function_call_output_msg(
                        &call_id, &name,
                        &format!("{{\"error\":\"Unknown voice '{}'. Use one of the supported voice names.\"}}", voice_name),
                    ));
                        return (replies, None);
                    }

                    // Write to DB
                    let pool = &app.state::<DbState>().0;
                    if let Ok(mut tx) = pool.begin().await {
                        let res1 = sqlx::query("INSERT OR REPLACE INTO app_settings (key, value) VALUES ('voice_name', ?1)")
                            .bind(&voice_name)
                            .execute(&mut *tx)
                            .await;
                        let res2 = sqlx::query("INSERT OR REPLACE INTO app_settings (key, value) VALUES ('voice_onboarded', 'true')")
                            .execute(&mut *tx)
                            .await;
                        if res1.is_ok() && res2.is_ok() {
                            let _ = tx.commit().await;
                        } else {
                            log::error!("[Session] Failed to persist voice settings");
                            let _ = tx.rollback().await;
                        }
                    } else {
                        log::error!("[Session] Failed to start transaction for voice settings");
                    }

                    // Complete FC
                    replies.push(protocol.function_call_output_msg(
                        &call_id,
                        &name,
                        "{\"status\":\"switching\"}",
                    ));

                    return (replies, Some(LoopExit::Reconnect(voice_name)));
                }

                // Subagent tools — handled immediately (bypass task_manager)
                if name == "spawn_subagent" {
                    let args: Value =
                        serde_json::from_str(&effective_arguments).unwrap_or(serde_json::json!({}));
                    let goal = args["goal"].as_str().unwrap_or("").to_string();
                    let result = match subagent_mgr.spawn(&goal).await {
                        Ok(task_id) => {
                            log::info!("[Session] Spawned subagent {} for: {}", task_id, goal);
                            serde_json::json!({
                                "task_id": task_id, 
                                "status": "spawned", 
                                "sys_instruction": "Task started in background. CRITICAL: You MUST now immediately speak a brief natural filler like 'Let me check on that' or '我现在去看看' to let the user know you are working on it. Keep your internal mechanics strictly hidden."
                            }).to_string()
                        }
                        Err(e) => {
                            log::error!("[Session] Failed to spawn subagent: {}", e);
                            serde_json::json!({"error": e}).to_string()
                        }
                    };
                    replies.push(protocol.function_call_output_msg(&call_id, &name, &result));
                } else if name == "reply_to_subagent" {
                    let args: Value =
                        serde_json::from_str(&effective_arguments).unwrap_or(serde_json::json!({}));
                    let task_id = args["task_id"].as_str().unwrap_or("").to_string();
                    let message = args["message"].as_str().unwrap_or("").to_string();
                    let result = match subagent_mgr.reply(&task_id, &message) {
                        Ok(()) => {
                            log::info!("[Session] Delivered reply to subagent {}", task_id);
                            serde_json::json!({"status": "delivered", "task_id": task_id})
                                .to_string()
                        }
                        Err(e) => serde_json::json!({"error": e}).to_string(),
                    };
                    replies.push(protocol.function_call_output_msg(&call_id, &name, &result));
                } else if self.consecutive_tool_rounds >= MAX_TOOL_ROUNDS {
                    // Safety: reject tool calls if we've exceeded max consecutive rounds
                    log::warn!(
                        "[Session] Rejecting tool call {} — max rounds ({}) reached",
                        name,
                        MAX_TOOL_ROUNDS
                    );
                    replies.push(protocol.function_call_output_msg(
                    &call_id, &name, "{\"error\": \"Maximum tool execution rounds reached. Please summarize your progress and tell the user.\"}"
                ));
                } else {
                    // Root fix: time/evidence checks should default to "before now"
                    // to avoid using current-turn utterances as evidence.
                    if name == "query_memory_evidence" || name == "get_last_chat_time" {
                        if let Ok(mut args) = serde_json::from_str::<Value>(&effective_arguments) {
                            let now_ts = chrono::Utc::now().timestamp();
                            let cutoff = self.user_turn_started_at.unwrap_or(now_ts);
                            let min_allowed = now_ts - MAX_MEMORY_TOOL_LOOKBACK_SECONDS;

                            let sanitized_before = args
                                .get("before_unix_ts")
                                .and_then(|v| v.as_i64())
                                .filter(|ts| *ts >= min_allowed && *ts <= now_ts + 60)
                                .unwrap_or(cutoff);
                            args["before_unix_ts"] = serde_json::json!(sanitized_before);

                            if args.get("exclude_recent_seconds").is_none() {
                                args["exclude_recent_seconds"] =
                                    serde_json::json!(DEFAULT_MEMORY_TOOL_EXCLUDE_RECENT_SECONDS);
                            }
                            effective_arguments = args.to_string();
                        }
                    }

                    // All other tools: accumulate for batch submission
                    log::info!(
                        "[Session] Tool call: {} (args_bytes={})",
                        name,
                        effective_arguments.len()
                    );
                    log::debug!("[Session] Tool args {}: {}", name, effective_arguments);
                    self.pending_tool_calls.push(task_manager::TaskRequest {
                        call_id,
                        tool_name: name,
                        arguments: effective_arguments,
                    });
                }
            }
            realtime_ws::WsEvent::ResponseDone => {
                if self.response_done_handled_for_turn {
                    log::debug!("[Session] Ignoring duplicated ResponseDone in same turn");
                    continue;
                }
                self.response_done_handled_for_turn = true;
                log::info!("[Session] Response completed");
                // Fallback flush for providers/events that may not emit ResponseStart.
                self.flush_user_transcript(memory).await;
                // Clear AI speaking flag — playback buffer + echo tail handle the remaining audio
                flags.is_ai_speaking.store(false, Ordering::Relaxed);
                // Persist accumulated transcript to memory
                if !self.transcript_buf.is_empty() {
                    let full_text = std::mem::take(&mut self.transcript_buf);
                    if let Err(e) = memory.persist("assistant", &full_text).await { log::error!("[Session] Failed to persist output: {}", e); }
                }
                // Submit any pending tool calls as a batch
                if !self.pending_tool_calls.is_empty() {
                    let calls: Vec<task_manager::TaskRequest> = std::mem::take(&mut self.pending_tool_calls);
                    self.tools_in_flight += calls.len();
                    log::info!("[Session] Submitting {} tool calls", self.tools_in_flight);
                    task_mgr.submit(calls);
                }
            }
            realtime_ws::WsEvent::Error(msg) => {
                log::error!("[Session] Server error: {}", msg);
                if let Err(e) = app.emit(
                    "agent-status",
                    StatusPayload {
                        state: "error".into(),
                        message: Some(msg),
                    },
                ) {
                    log::warn!("[Session] Emit event error: {}", e);
                }
            }
            realtime_ws::WsEvent::UserSpeechStarted => {
                let duration = flags.speech_duration_ms.load(Ordering::Relaxed);
                let peak = f32::from_bits(flags.peak_rms.load(Ordering::Relaxed));
                
                log::info!(
                    "[Session] User speech started detected. Local metrics: duration={}ms, peak_rms={:.3}",
                    duration, peak
                );

                if duration < 500 && peak < 0.08 {
                    // Soft interruption / Backchannel
                    log::info!("[Session] Soft interruption detected (backchannel). Not interrupting AI.");
                } else {
                    // Hard interruption -> full abort
                    log::info!("[Session] Hard interruption. Stopping playback and aborting tasks.");
                    flags.is_ai_speaking.store(false, Ordering::Relaxed);
                    let _ = playback_tx.try_send(PlaybackCommand::FadeOut(100));
                    if self.tools_in_flight > 0 {
                        log::info!(
                            "[Session] Aborting {} running tools due to interrupt",
                            self.tools_in_flight
                        );
                        task_mgr.abort_all();
                        self.tools_in_flight = 0;
                    }
                    if !self.transcript_buf.is_empty() {
                        let partial = self.transcript_buf.clone();
                        let sys_msg = format!(
                            "SYSTEM: The user just interrupted you. Your last partial sentence was '{}'. Please listen to what they say next and respond naturally.",
                            partial
                        );
                        let inject_msg = protocol.inject_speech_msg(&sys_msg);
                        replies.push(inject_msg);
                        // Clear the buffer since it was interrupted
                        self.transcript_buf.clear();
                    }
                }
            }
            realtime_ws::WsEvent::Other(event_type) => {
                let preview: String = raw.chars().take(300).collect();
                log::debug!("[Session] WS event '{}': {}", event_type, preview);
            }
        }
    }
    (replies, None)
    }
}

fn merge_user_transcript_chunk(buffer: &mut String, chunk: &str) {
    let chunk = chunk.trim();
    if chunk.is_empty() {
        return;
    }

    if buffer.is_empty() {
        buffer.push_str(chunk);
        return;
    }

    let current = buffer.as_str();

    // Some providers resend cumulative transcript; keep the latest full form instead of duplicating.
    if chunk.starts_with(current) {
        *buffer = chunk.to_string();
        return;
    }

    // Ignore duplicate/replayed tails.
    if current.ends_with(chunk) {
        return;
    }

    let needs_space = current
        .chars()
        .last()
        .map(|c| c.is_alphanumeric())
        .unwrap_or(false)
        && chunk
            .chars()
            .next()
            .map(|c| c.is_alphanumeric())
            .unwrap_or(false);
    if needs_space {
        buffer.push(' ');
    }
    buffer.push_str(chunk);
}
