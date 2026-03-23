/// tools — Agent tool definitions and internal dispatch
///
/// Tool schemas and system instructions are ported from the frontend AgentPipeline.ts.
/// dispatch_tool directly calls system_api functions without IPC.
use crate::db::DbState;
use crate::system_api;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Tool definition JSON (OpenAI Realtime API function calling format)
pub const AGENT_TOOLS_JSON: &str = r#"[
  {
    "type": "function",
    "name": "exec_shell",
    "description": "Execute a shell command and return stdout. On Windows this runs PowerShell; on macOS/Linux it runs sh. Execute exactly the command needed, and avoid chaining multiple tool calls unless absolutely necessary.",
    "parameters": {
      "type": "object",
      "properties": {
        "command": { "type": "string", "description": "The shell command to execute" }
      },
      "required": ["command"]
    }
  },
  {
    "type": "function",
    "name": "type_text",
    "description": "Simulate keyboard typing. Types the given text string.",
    "parameters": {
      "type": "object",
      "properties": {
        "text": { "type": "string", "description": "The text to type" }
      },
      "required": ["text"]
    }
  },
  {
    "type": "function",
    "name": "press_key",
    "description": "Press a single key (e.g. 'enter', 'tab', 'escape', 'f5', or a single character).",
    "parameters": {
      "type": "object",
      "properties": {
        "key": { "type": "string", "description": "Key name" }
      },
      "required": ["key"]
    }
  },
  {
    "type": "function",
    "name": "move_mouse",
    "description": "Move the mouse cursor to absolute screen coordinates (x, y).",
    "parameters": {
      "type": "object",
      "properties": {
        "x": { "type": "integer", "description": "X coordinate" },
        "y": { "type": "integer", "description": "Y coordinate" }
      },
      "required": ["x", "y"]
    }
  },
  {
    "type": "function",
    "name": "click_mouse",
    "description": "Click a mouse button: 'left', 'right', or 'middle'.",
    "parameters": {
      "type": "object",
      "properties": {
        "button": {
          "type": "string",
          "enum": ["left", "right", "middle"],
          "description": "Mouse button"
        }
      },
      "required": ["button"]
    }
  },
  {
    "type": "function",
    "name": "render_local_ui",
    "description": "Display content in the agent's local board window. Use for short text, code snippets, simple lists, or brief explanations. For large documents or websites, use open_external instead.",
    "parameters": {
      "type": "object",
      "properties": {
        "content_type": {
          "type": "string",
          "enum": ["text", "code", "html"],
          "description": "Type of content to render"
        },
        "content": {
          "type": "string",
          "description": "The content to display"
        },
        "title": {
          "type": "string",
          "description": "Optional window title"
        }
      },
      "required": ["content_type", "content"]
    }
  },
  {
    "type": "function",
    "name": "open_external",
    "description": "Open a URL or file path with the OS default application. Use for websites, large documents, complex code projects, etc.",
    "parameters": {
      "type": "object",
      "properties": {
        "target": {
          "type": "string",
          "description": "URL (https://...) or absolute file path to open"
        }
      },
      "required": ["target"]
    }
  },
  {
    "type": "function",
    "name": "set_agent_config",
    "description": "Set a configuration item for the agent. The user may ask you to change your name or other settings. Use key 'agent_name' to set the agent display name.",
    "parameters": {
      "type": "object",
      "properties": {
        "key": {
          "type": "string",
          "description": "Config key, e.g. 'agent_name'"
        },
        "value": {
          "type": "string",
          "description": "Config value"
        }
      },
      "required": ["key", "value"]
    }
  },
  {
    "type": "function",
    "name": "update_core_profile",
    "description": "Update or insert a key-value pair in the user's Core Profile (high-priority memory). Use this for names, strict preferences, or vital context. e.g. key: 'language_preference', value: 'Rust'.",
    "parameters": {
      "type": "object",
      "properties": {
        "key": { "type": "string", "description": "Profile key" },
        "value": { "type": "string", "description": "Profile value" }
      },
      "required": ["key", "value"]
    }
  },
  {
    "type": "function",
    "name": "add_knowledge_node",
    "description": "Add a new entity node to the Knowledge Graph.",
    "parameters": {
      "type": "object",
      "properties": {
        "id": { "type": "string", "description": "Node ID, snake_case" },
        "label": { "type": "string", "description": "Display name of the node" },
        "node_type": { "type": "string", "description": "e.g., 'person', 'concept', 'place'" }
      },
      "required": ["id", "label", "node_type"]
    }
  },
  {
    "type": "function",
    "name": "add_knowledge_edge",
    "description": "Add a relationship between two existing Knowledge Graph nodes.",
    "parameters": {
      "type": "object",
      "properties": {
        "source": { "type": "string", "description": "Source Node ID" },
        "target": { "type": "string", "description": "Target Node ID" },
        "relation": { "type": "string", "description": "Relationship, e.g. 'likes', 'works_at'" }
      },
      "required": ["source", "target", "relation"]
    }
  },
  {
    "type": "function",
    "name": "observe_screen",
    "description": "Capture a screenshot of the primary monitor. Use this BEFORE performing GUI actions to see the current screen state and locate targets. Use it AFTER actions to verify they succeeded. Returns the image with screen dimensions (width, height in pixels). Prefer calling this frequently during multi-step GUI tasks.",
    "parameters": { "type": "object", "properties": {} }
  },
  {
    "type": "function",
    "name": "schedule_task",
    "description": "Set a reminder or scheduled task. The agent will proactively notify the user when the time comes. Use when the user asks to be reminded about something or wants to schedule a future action.",
    "parameters": {
      "type": "object",
      "properties": {
        "description": {
          "type": "string",
          "description": "What to remind the user about"
        },
        "due_in_seconds": {
          "type": "integer",
          "description": "Number of seconds from now until the reminder triggers"
        }
      },
      "required": ["description", "due_in_seconds"]
    }
  },
  {
    "type": "function",
    "name": "spawn_subagent",
    "description": "Spawn a background agent to handle a complex task while you continue conversing naturally. The subagent works independently using AI and tools, and will notify you when done or if it needs user input. Returns immediately with a task ID. Use for multi-step tasks that would interrupt conversation flow.",
    "parameters": {
      "type": "object",
      "properties": {
        "goal": {
          "type": "string",
          "description": "Clear description of the task for the background agent to accomplish"
        }
      },
      "required": ["goal"]
    }
  },
  {
    "type": "function",
    "name": "reply_to_subagent",
    "description": "Send a reply to a background task that asked a question. Use to provide answers, decisions, or confirmations to suspended subagents.",
    "parameters": {
      "type": "object",
      "properties": {
        "task_id": {
          "type": "string",
          "description": "The task ID of the subagent to reply to"
        },
        "message": {
          "type": "string",
          "description": "Your reply message"
        }
      },
      "required": ["task_id", "message"]
    }
  },
  {
    "type": "function",
    "name": "search_knowledge",
    "description": "Search the Knowledge Graph for entities and their relationships.",
    "parameters": {
      "type": "object",
      "properties": {
        "keyword": { "type": "string", "description": "Search keyword or phrase" },
        "limit": { "type": "integer", "description": "Max results to return" }
      },
      "required": ["keyword"]
    }
  },
  {
    "type": "function",
    "name": "disconnect_session",
    "description": "Gracefully close the voice connection. Call this when the user says goodbye, asks you to leave, or says something like '退下吧'. Say your farewell BEFORE calling this tool — once called, the connection will close shortly after.",
    "parameters": { "type": "object", "properties": {} }
  }
]"#;

// ============================================================
// Board display content
// ============================================================

#[derive(Clone, Serialize)]
pub struct BoardContent {
    pub content_type: String,
    pub content: String,
}

/// Check if a tool requires AppHandle (UI tool)
pub fn is_ui_tool(name: &str) -> bool {
    matches!(
        name,
        "render_local_ui"
            | "open_external"
            | "set_agent_config"
            | "schedule_task"
            | "disconnect_session"
            | "update_core_profile"
            | "add_knowledge_node"
            | "add_knowledge_edge"
            | "search_knowledge"
    )
}

/// Dispatch UI tool (requires AppHandle to create windows / open external apps)
pub async fn dispatch_ui_tool(app: &AppHandle, name: &str, args_json: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or_default();
    log::info!("[Tools] UI dispatch: {}({})", name, args_json);

    match name {
        "render_local_ui" => {
            let content_type = args["content_type"].as_str().unwrap_or("text").to_string();
            let content = args["content"].as_str().unwrap_or("").to_string();
            let title = args["title"].as_str().unwrap_or("Agent Board").to_string();

            // Store in managed state for the board window to fetch on mount
            let board_state = app.state::<super::BoardState>();
            {
                let mut guard = board_state.0.lock().unwrap();
                *guard = Some(BoardContent {
                    content_type: content_type.clone(),
                    content: content.clone(),
                });
            }

            // Get or create board window
            match app.get_webview_window("board") {
                Some(w) => {
                    let _ = w.show();
                    let _ = w.set_focus();
                    let _ = app.emit(
                        "agent-render-ui",
                        BoardContent {
                            content_type,
                            content,
                        },
                    );
                }
                None => {
                    if let Err(e) = WebviewWindowBuilder::new(
                        app,
                        "board",
                        WebviewUrl::App("/board.html".into()),
                    )
                    .title(&title)
                    .inner_size(640.0, 520.0)
                    .build()
                    {
                        return format!("Error creating board window: {}", e);
                    }
                    // New window will call get_board_content on mount
                }
            };

            format!("OK: Content displayed in '{}'", title)
        }
        "open_external" => {
            let target = args["target"].as_str().unwrap_or("");
            if target.is_empty() {
                return "Error: target is empty".to_string();
            }
            log::info!("[Tools] Opening external: {}", target);
            match open::that(target) {
                Ok(()) => format!("OK: Opened {}", target),
                Err(e) => format!("Error opening {}: {}", target, e),
            }
        }
        "set_agent_config" => {
            let key = args["key"].as_str().unwrap_or("");
            let value = args["value"].as_str().unwrap_or("");
            if key.is_empty() {
                return "Error: key is empty".to_string();
            }
            let db_state = app.state::<DbState>();
            let pool = &db_state.0;
          match sqlx::query(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
          )
          .bind(key)
          .bind(value)
          .execute(pool)
          .await {
            Ok(_) => {
              log::info!("[Tools] Config set: {} = {}", key, value);
              format!("OK: {} = {}", key, value)
            }
            Err(e) => format!("Error saving config: {}", e),
          }
        }
        "update_core_profile" => {
            let key = args["key"].as_str().unwrap_or("");
            let value = args["value"].as_str().unwrap_or("");
            if key.is_empty() || value.is_empty() {
                return "Error: key or value is empty".to_string();
            }
            let db_state = app.state::<DbState>();
            let pool = &db_state.0;
            match crate::db::upsert_core_profile(pool, key, value).await {
              Ok(_) => {
                log::info!("[Tools] Core profile updated: {} = {}", key, value);
                format!("OK: Core profile {} = {}", key, value)
              }
              Err(e) => format!("Error updating core profile: {}", e),
            }
        }
        "add_knowledge_node" => {
            let id = args["id"].as_str().unwrap_or("");
            let label = args["label"].as_str().unwrap_or("");
            let node_type = args["node_type"].as_str().unwrap_or("");
            if id.is_empty() {
                return "Error: id is empty".to_string();
            }
            let db_state = app.state::<DbState>();
            let pool = &db_state.0;
            match crate::db::add_kg_node(pool, id, label, node_type).await {
              Ok(_) => {
                log::info!("[Tools] KG node added: {}", id);
                format!("OK: Added knowledge node '{}'", id)
              }
              Err(e) => format!("Error adding knowledge node: {}", e),
            }
        }
        "add_knowledge_edge" => {
            let source = args["source"].as_str().unwrap_or("");
            let target = args["target"].as_str().unwrap_or("");
            let relation = args["relation"].as_str().unwrap_or("");
            if source.is_empty() || target.is_empty() {
                return "Error: source or target is empty".to_string();
            }
            let db_state = app.state::<DbState>();
            let pool = &db_state.0;
            match crate::db::add_kg_edge(pool, source, target, relation).await {
              Ok(_) => {
                log::info!(
                  "[Tools] KG edge added: {} -[{}]-> {}",
                  source,
                  relation,
                  target
                );
                format!("OK: Added edge {} -[{}]-> {}", source, relation, target)
              }
              Err(e) => format!("Error adding knowledge edge: {}", e),
            }
        }
        "schedule_task" => {
            let description = args["description"].as_str().unwrap_or("");
            let due_in_seconds = args["due_in_seconds"].as_i64().unwrap_or(0);
            if description.is_empty() {
                return "Error: description is empty".to_string();
            }
            if due_in_seconds <= 0 {
                return "Error: due_in_seconds must be positive".to_string();
            }
            match super::scheduler::add_scheduled_task(app, description, due_in_seconds).await {
                Ok(id) => {
                    log::info!(
                        "[Tools] Scheduled task #{}: \"{}\" in {}s",
                        id,
                        description,
                        due_in_seconds
                    );
                    format!(
                        "OK: Task #{} scheduled, will trigger in {} seconds",
                        id, due_in_seconds
                    )
                }
                Err(e) => format!("Error scheduling task: {}", e),
            }
        }
        "search_knowledge" => {
            let keyword = args["keyword"].as_str().unwrap_or("");
            if keyword.is_empty() {
                return "Error: keyword is empty".to_string();
            }
            let limit = args["limit"].as_i64().unwrap_or(10).min(30).max(1) as usize;
            let db_state = app.state::<DbState>();
            let pool = &db_state.0;
            match crate::db::search_kg_subgraph(pool, keyword, limit).await {
              Ok((nodes, edges)) => {
                if nodes.is_empty() {
                  format!("No knowledge found matching '{}'", keyword)
                } else {
                  let mut res = String::new();
                  res.push_str("Nodes:\n");
                  for n in nodes {
                    res.push_str(&format!("  {} ({}): {}\n", n.id, n.r#type, n.label));
                  }
                  if !edges.is_empty() {
                    res.push_str("\nEdges:\n");
                    for e in edges {
                      res.push_str(&format!(
                        "  {} -[{}]-> {}\n",
                        e.source_id, e.relation, e.target_id
                      ));
                    }
                  }
                  res
                }
              }
              Err(e) => format!("Error searching knowledge: {}", e),
            }
        }
        "disconnect_session" => {
            log::info!("[Tools] Disconnect session requested by AI");
            let agent_state = app.state::<super::AgentState>();
            let cmd_tx = {
                let guard = agent_state.session.lock().ok();
                guard.and_then(|g| g.as_ref().map(|h| h.cmd_tx.clone()))
            };
            if let Some(tx) = cmd_tx {
                let _ = tx.try_send(super::session::SessionCommand::Disconnect);
                "OK: Disconnect initiated. Say your farewell now — connection will close in 3 seconds.".to_string()
            } else {
                "Error: no active session to disconnect".to_string()
            }
        }
        _ => format!("Unknown UI tool: {}", name),
    }
}

fn allow_high_risk_shell(app: Option<&AppHandle>) -> bool {
    let Some(app) = app else {
        return false;
    };

  let app_handle = app.clone();
  let (tx, rx) = std::sync::mpsc::channel::<bool>();

  tauri::async_runtime::spawn(async move {
    let db_state = app_handle.state::<DbState>();
    let pool = &db_state.0;
    let setting = sqlx::query_scalar::<_, String>(
      "SELECT value FROM app_settings WHERE key = 'allow_high_risk_shell'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "false".to_string());

    let allowed = setting == "true" || setting == "1";
    let _ = tx.send(allowed);
  });

  rx.recv_timeout(std::time::Duration::from_secs(2))
    .unwrap_or(false)
}

fn dispatch_tool_inner(app: Option<&AppHandle>, name: &str, args_json: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or_default();

    log::info!("[Tools] Dispatching: {}({})", name, args_json);

    let result: Result<String, String> = match name {
        "exec_shell" => {
            let command = args["command"].as_str().unwrap_or("").to_string();
            let allow_high_risk = allow_high_risk_shell(app);
            match system_api::validate_shell_command(&command, allow_high_risk) {
                Ok(()) => system_api::exec_shell_with_policy(command, allow_high_risk),
                Err(e) => Err(e),
            }
        }
        "type_text" => {
            let text = args["text"].as_str().unwrap_or("").to_string();
            system_api::type_text(text).map(|_| "OK".to_string())
        }
        "press_key" => {
            let key = args["key"].as_str().unwrap_or("").to_string();
            system_api::press_key(key).map(|_| "OK".to_string())
        }
        "move_mouse" => {
            let x = args["x"].as_i64().unwrap_or(0) as i32;
            let y = args["y"].as_i64().unwrap_or(0) as i32;
            system_api::move_mouse(x, y).map(|_| "OK".to_string())
        }
        "click_mouse" => {
            let button = args["button"].as_str().unwrap_or("left").to_string();
            system_api::click_mouse(button).map(|_| "OK".to_string())
        }
        "observe_screen" => system_api::capture_screen().map(|cap| {
            serde_json::to_string(&serde_json::json!({
              "width": cap.width,
              "height": cap.height,
              "image_base64": cap.image_base64,
            }))
            .unwrap_or_default()
        }),
        _ => Err(format!("Unknown function: {}", name)),
    };

    match result {
        Ok(output) => {
            log::info!("[Tools] {} → OK ({} bytes)", name, output.len());
            output
        }
        Err(e) => {
            log::error!("[Tools] {} → Error: {}", name, e);
            format!("Error: {}", e)
        }
    }
}

/// Dispatch tool call, directly calling system_api functions
#[allow(dead_code)]
pub fn dispatch_tool(name: &str, args_json: &str) -> String {
    dispatch_tool_inner(None, name, args_json)
}

pub fn dispatch_tool_with_app(app: &AppHandle, name: &str, args_json: &str) -> String {
    dispatch_tool_inner(Some(app), name, args_json)
}
