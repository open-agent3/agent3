/// subagents — Background Chat Completions agent pool
///
/// Each subagent runs as an independent tokio task using standard REST Chat Completions API,
/// completely decoupled from the Realtime WS voice socket.
/// Communication: MPSC events → session (questions/completions), oneshot for user replies.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::agent::tools;
use crate::db::DbState;

// ============================================================
// Types
// ============================================================

/// Events sent from subagents to the voice session
#[derive(Debug)]
pub enum SubagentEvent {
    /// Subagent needs user input — voice AI should speak the question
    AskUser { task_id: String, question: String },
    /// Subagent completed its goal
    Completed { task_id: String, summary: String },
    /// Subagent failed
    Failed { task_id: String, error: String },
}

/// Log payload emitted to frontend for Ghost UI
#[derive(Clone, Serialize)]
pub struct SubagentLogPayload {
    pub task_id: String,
    pub status: String,
    pub message: String,
}

// ============================================================
// SubagentManager
// ============================================================

pub struct SubagentManager {
    app: AppHandle,
    event_tx: mpsc::Sender<SubagentEvent>,
    /// Running task abort handles
    tasks: Arc<Mutex<HashMap<String, AbortHandle>>>,
    /// Suspended tasks waiting for user reply
    pending_replies: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
}

impl SubagentManager {
    pub fn new(app: AppHandle, event_tx: mpsc::Sender<SubagentEvent>) -> Self {
        Self {
            app,
            event_tx,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            pending_replies: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a new background subagent. Returns the task ID immediately.
    pub async fn spawn(&self, goal: &str) -> Result<String, String> {
        let task_id = uuid::Uuid::new_v4().to_string()[..8].to_string();

        let provider = resolve_provider(&self.app).await?;

        let app = self.app.clone();
        let goal = goal.to_string();
        let tid = task_id.clone();
        let event_tx = self.event_tx.clone();
        let tasks = self.tasks.clone();
        let pending_replies = self.pending_replies.clone();

        let handle = tokio::spawn(subagent_loop(
            app,
            tid,
            goal,
            provider,
            event_tx,
            tasks.clone(),
            pending_replies,
        ));

        {
            let mut map = self.tasks.lock().unwrap();
            map.insert(task_id.clone(), handle.abort_handle());
        }

        log::info!("[Subagent] Spawned task {}", task_id);
        Ok(task_id)
    }

    /// Resume a suspended subagent with the user's reply
    pub fn reply(&self, task_id: &str, message: &str) -> Result<(), String> {
        let tx = {
            let mut map = self.pending_replies.lock().unwrap();
            map.remove(task_id)
        };
        match tx {
            Some(sender) => sender
                .send(message.to_string())
                .map_err(|_| "Subagent task already closed".to_string()),
            None => Err(format!("No pending reply for task {}", task_id)),
        }
    }

    /// Abort all running subagents
    pub fn abort_all(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        for (id, handle) in tasks.drain() {
            handle.abort();
            log::info!("[Subagent] Aborted task {}", id);
        }
        let mut replies = self.pending_replies.lock().unwrap();
        replies.clear();
    }
}

// ============================================================
// Provider resolution
// ============================================================

struct ChatProvider {
    api_key: String,
    chat_url: String,
    model: String,
}

async fn resolve_provider(app: &AppHandle) -> Result<ChatProvider, String> {
    let db_state = app.state::<DbState>();
    let pool = &db_state.0;

        let columns =
            sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('llm_providers')")
                .fetch_all(pool)
                .await
                .map_err(|e| e.to_string())?;

        let has_role_column = columns.iter().any(|name| name == "role");

        if !has_role_column {
            log::warn!(
                "[Subagent] llm_providers.role is missing, falling back to active realtime provider"
            );
        }

        // Try dedicated background provider first, fall back to active realtime provider
        let background_row: Option<(String, String, String, String)> = if has_role_column {
            sqlx::query_as(
                "SELECT base_url, api_key, model, provider_type FROM llm_providers \
                 WHERE role = 'background' AND is_active = 1 LIMIT 1",
            )
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
        } else {
            None
        };

        if background_row.is_none() {
            log::warn!(
                "[Subagent] No active background provider found, falling back to active realtime provider"
            );
        }

        let fallback_query = if has_role_column {
            "SELECT base_url, api_key, model, provider_type FROM llm_providers \
                 WHERE is_active = 1 AND role IN ('realtime', 'sensory') LIMIT 1"
        } else {
            "SELECT base_url, api_key, model, provider_type FROM llm_providers \
                 WHERE is_active = 1 LIMIT 1"
        };

        let row = match background_row {
            Some(row) => row,
            None => sqlx::query_as(fallback_query)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
                .ok_or_else(|| "No LLM provider available for subagent".to_string())?,
        };

        let (base_url, api_key, model, provider_type) = row;

    Ok(ChatProvider {
        api_key,
        chat_url: derive_chat_url(&base_url, &provider_type),
        model: derive_chat_model(&model),
    })
}

/// Derive Chat Completions REST URL from the Realtime WS base URL
fn derive_chat_url(base_url: &str, provider_type: &str) -> String {
    match provider_type.to_lowercase().as_str() {
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/chat/completions".to_string(),
        _ => {
            // OpenAI-compatible: wss://host/v1/realtime → https://host/v1/chat/completions
            let url = base_url
                .replace("wss://", "https://")
                .replace("ws://", "http://");
            let base = url.trim_end_matches('/');
            if let Some(pos) = base.rfind("/realtime") {
                format!("{}/chat/completions", &base[..pos])
            } else if base.ends_with("/chat/completions") {
                base.to_string()
            } else {
                format!("{}/chat/completions", base)
            }
        }
    }
}

/// Map realtime model names to their Chat Completions equivalents
fn derive_chat_model(realtime_model: &str) -> String {
    if realtime_model.contains("realtime") {
        if realtime_model.contains("mini") {
            "gpt-4o-mini".to_string()
        } else {
            "gpt-4o".to_string()
        }
    } else {
        realtime_model.to_string()
    }
}

// ============================================================
// Ghost UI logging
// ============================================================

fn emit_log(app: &AppHandle, task_id: &str, status: &str, message: &str) {
    log::info!("[Subagent] [{}] {}: {}", task_id, status, message);
    let _ = app.emit(
        "subagent-log",
        SubagentLogPayload {
            task_id: task_id.to_string(),
            status: status.to_string(),
            message: message.to_string(),
        },
    );
}

// ============================================================
// Subagent tool set (Chat Completions format)
// ============================================================

fn build_subagent_tools() -> Vec<Value> {
    let realtime_tools: Vec<Value> =
        serde_json::from_str(tools::AGENT_TOOLS_JSON).unwrap_or_default();

    // Tools excluded from subagent use (prevent recursive spawning + token explosion)
    let excluded = [
        "schedule_task",
        "spawn_subagent",
        "reply_to_subagent",
        "observe_screen",
    ];

    let mut chat_tools: Vec<Value> = realtime_tools
        .iter()
        .filter(|t| {
            let name = t["name"].as_str().unwrap_or("");
            !excluded.contains(&name)
        })
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["parameters"]
                }
            })
        })
        .collect();

    // Add ask_user tool (subagent-only)
    chat_tools.push(json!({
        "type": "function",
        "function": {
            "name": "ask_user",
            "description": "Ask the user a question via voice and wait for their verbal response. Use when you need clarification, a decision, or confirmation before proceeding.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user"
                    }
                },
                "required": ["question"]
            }
        }
    }));

    chat_tools
}

// ============================================================
// Subagent execution loop
// ============================================================

async fn subagent_loop(
    app: AppHandle,
    task_id: String,
    goal: String,
    provider: ChatProvider,
    event_tx: mpsc::Sender<SubagentEvent>,
    tasks: Arc<Mutex<HashMap<String, AbortHandle>>>,
    pending_replies: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
) {
    emit_log(&app, &task_id, "started", &format!("Goal: {}", goal));

    let client = reqwest::Client::new();
    let subagent_tools = build_subagent_tools();

    let mut messages: Vec<Value> = vec![
        json!({
            "role": "system",
            "content": "You are an autonomous background task-solver agent OS framework. Follow ReAct methodology to accomplish goals using available tools.\n\n## ReAct Loop\n1. Think first: Use `<thinking>` tags before every action to analyze current state, available tools, and next steps.\n2. Act: Call tools strategically to gather information or complete work.\n3. Observe: Analyze tool outputs and adjust your approach if needed.\n4. Repeat until the goal is complete.\n\n## Guidelines\n- Be resilient: when tools fail, try alternative approaches.\n- CRITICAL: DO NOT use `ask_user` before you attempt to gather information yourself using `observe_screen` or OS tools.\n- Keep questions extremely concise and natural. Ask only 1 question at a time. Action before Interrogation.\n- Provide a complete summary when finished."
        }),
        json!({
            "role": "user",
            "content": goal
        }),
    ];

    const MAX_ROUNDS: usize = 25;

    for round in 0..MAX_ROUNDS {
        emit_log(&app, &task_id, "thinking", &format!("Round {}", round + 1));

        let body = json!({
            "model": provider.model,
            "messages": messages,
            "tools": subagent_tools,
        });

        let response = match client
            .post(&provider.chat_url)
            .header("Authorization", format!("Bearer {}", provider.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let err = format!("HTTP request failed: {}", e);
                emit_log(&app, &task_id, "error", &err);
                let _ = event_tx
                    .send(SubagentEvent::Failed {
                        task_id: task_id.clone(),
                        error: err,
                    })
                    .await;
                cleanup(&tasks, &task_id);
                return;
            }
        };

        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();

        if !status.is_success() {
            let preview: String = body_text.chars().take(200).collect();
            let err = format!("API error {}: {}", status, preview);
            emit_log(&app, &task_id, "error", &err);
            let _ = event_tx
                .send(SubagentEvent::Failed {
                    task_id: task_id.clone(),
                    error: err,
                })
                .await;
            cleanup(&tasks, &task_id);
            return;
        }

        let resp: Value = match serde_json::from_str(&body_text) {
            Ok(v) => v,
            Err(e) => {
                let err = format!("JSON parse error: {}", e);
                emit_log(&app, &task_id, "error", &err);
                let _ = event_tx
                    .send(SubagentEvent::Failed {
                        task_id: task_id.clone(),
                        error: err,
                    })
                    .await;
                cleanup(&tasks, &task_id);
                return;
            }
        };

        let choice = &resp["choices"][0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");

        // Extract and emit thought process
        if let Some(content) = message["content"].as_str() {
            if !content.trim().is_empty() {
                let mut display_text = content.to_string();
                if let (Some(start), Some(end)) = (display_text.find("<thinking>"), display_text.find("</thinking>")) {
                    if end > start + 10 {
                        display_text = display_text[start+10..end].to_string();
                    }
                }
                // Clean up newlines for the ghost UI log
                display_text = display_text.replace('\n', " ").trim().to_string();
                if !display_text.is_empty() {
                    emit_log(
                        &app,
                        &task_id,
                        "thinking",
                        &display_text,
                    );
                }
            }
        }

        // Add assistant message to conversation history
        messages.push(message.clone());

        if finish_reason == "tool_calls" || message.get("tool_calls").is_some_and(|v| v.is_array())
        {
            if let Some(tool_calls) = message["tool_calls"].as_array() {
                for tc in tool_calls {
                    let tc_id = tc["id"].as_str().unwrap_or("").to_string();
                    let func_name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let func_args = tc["function"]["arguments"]
                        .as_str()
                        .unwrap_or("{}")
                        .to_string();

                    let preview: String = func_args.chars().take(100).collect();
                    emit_log(
                        &app,
                        &task_id,
                        "tool",
                        &format!("{}({})", func_name, preview),
                    );

                    let output = if func_name == "ask_user" {
                        handle_ask_user(&app, &task_id, &func_args, &event_tx, &pending_replies)
                            .await
                    } else if tools::is_ui_tool(&func_name) {
                        tools::dispatch_ui_tool(&app, &func_name, &func_args).await
                    } else {
                        let name = func_name.clone();
                        let args = func_args.clone();
                        let app_clone = app.clone();
                        tokio::task::spawn_blocking(move || {
                            tools::dispatch_tool_with_app(&app_clone, &name, &args)
                        })
                        .await
                        .unwrap_or_else(|e| format!("Error: {}", e))
                    };

                    emit_log(
                        &app,
                        &task_id,
                        "result",
                        &format!("{}: {} bytes", func_name, output.len()),
                    );

                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tc_id,
                        "content": output
                    }));
                }
            }
        } else {
            // Final response — task complete
            let content = message["content"]
                .as_str()
                .unwrap_or("Task completed.")
                .to_string();
            emit_log(&app, &task_id, "completed", &content);
            let _ = event_tx
                .send(SubagentEvent::Completed {
                    task_id: task_id.clone(),
                    summary: content,
                })
                .await;
            cleanup(&tasks, &task_id);
            return;
        }
    }

    // Exceeded max rounds
    let msg = "Reached maximum execution rounds";
    emit_log(&app, &task_id, "completed", msg);
    let _ = event_tx
        .send(SubagentEvent::Completed {
            task_id: task_id.clone(),
            summary: msg.into(),
        })
        .await;
    cleanup(&tasks, &task_id);
}

/// Handle ask_user tool: suspend via oneshot, inject question, await reply
async fn handle_ask_user(
    app: &AppHandle,
    task_id: &str,
    args_json: &str,
    event_tx: &mpsc::Sender<SubagentEvent>,
    pending_replies: &Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
) -> String {
    let args: Value = serde_json::from_str(args_json).unwrap_or_default();
    let question = args["question"].as_str().unwrap_or("").to_string();

    if question.is_empty() {
        return "Error: question is empty".to_string();
    }

    emit_log(
        app,
        task_id,
        "waiting",
        &format!("Asking user: {}", question),
    );

    let (reply_tx, reply_rx) = oneshot::channel::<String>();

    // Store the sender so the voice session can deliver the user's reply
    {
        let mut map = pending_replies.lock().unwrap();
        map.insert(task_id.to_string(), reply_tx);
    }

    // Notify the voice session to ask the user
    let _ = event_tx
        .send(SubagentEvent::AskUser {
            task_id: task_id.to_string(),
            question,
        })
        .await;

    // Suspend until user replies
    match reply_rx.await {
        Ok(answer) => {
            emit_log(
                app,
                task_id,
                "resumed",
                &format!("User replied: {}", answer),
            );
            answer
        }
        Err(_) => {
            emit_log(app, task_id, "error", "Reply channel closed");
            "Error: reply channel was closed".to_string()
        }
    }
}

fn cleanup(tasks: &Arc<Mutex<HashMap<String, AbortHandle>>>, task_id: &str) {
    let mut map = tasks.lock().unwrap();
    map.remove(task_id);
}

impl Drop for SubagentManager {
    fn drop(&mut self) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        for (id, handle) in tasks.drain() {
            handle.abort();
            log::info!("[Subagent] Aborted task {} on Drop", id);
        }
    }
}
