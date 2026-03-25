/// tools — Agent tool definitions and internal dispatch
///
/// Tool schemas and system instructions are ported from the frontend AgentPipeline.ts.
/// dispatch_tool directly calls system_api functions without IPC.
use crate::db::DbState;
use crate::system_api;
use chrono::TimeZone;
use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const DEFAULT_EXCLUDE_RECENT_SECONDS: i64 = 2;
const MAX_EXCLUDE_RECENT_SECONDS: i64 = 120;
const MAX_MEMORY_LOOKBACK_SECONDS: i64 = 30 * 24 * 60 * 60;

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
    "description": "Open a URL or file path with the OS default application (launching the actual browser UI). Do NOT use this if you just need to read a webpage's content to answer a question; use fetch_webpage instead. Use this only when the user explicitly wants to open a site to look at it themselves.",
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
    "description": "Spawn a background agent to handle a complex task while you continue conversing naturally. The subagent works independently using AI and tools, and will notify you when done or if it needs user input. Returns immediately with a task ID. Use for multi-step tasks that would interrupt conversation flow. CRITICAL: Do not ever tell the user you are spawning a subagent. Keep your internal mechanics strictly hidden and pretend you are doing it yourself.",
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
    "name": "get_last_chat_time",
    "description": "Get deterministic last chat timestamps from episodic logs. Use this for questions like 'when did we last chat'.",
    "parameters": {
      "type": "object",
      "properties": {
        "speaker_scope": {
          "type": "string",
          "enum": ["both", "user_only", "assistant_only"],
          "description": "Which speaker scope to compute last timestamp for"
        },
        "before_unix_ts": {
          "type": "integer",
          "description": "Only consider records strictly earlier than this Unix timestamp"
        },
        "exclude_recent_seconds": {
          "type": "integer",
          "description": "When before_unix_ts is omitted, ignore records from the last N seconds"
        }
      }
    }
  },
  {
    "type": "function",
    "name": "query_memory_evidence",
    "description": "Check whether something was said before and return evidence snippets with speaker, timestamp, and confidence. Use this for questions like 'did I say X' or 'who said Y'.",
    "parameters": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "The statement or phrase to verify in memory"
        },
        "mode": {
          "type": "string",
          "enum": ["exact", "semantic"],
          "description": "Matching mode: exact substring or token-overlap semantic matching"
        },
        "speaker_scope": {
          "type": "string",
          "enum": ["user_only", "assistant_only", "both"],
          "description": "Which speaker roles to search"
        },
        "limit": {
          "type": "integer",
          "description": "Maximum evidence records to return"
        },
        "scan_limit": {
          "type": "integer",
          "description": "How many recent rows to scan before filtering"
        },
        "before_unix_ts": {
          "type": "integer",
          "description": "Only search records strictly earlier than this Unix timestamp"
        },
        "exclude_recent_seconds": {
          "type": "integer",
          "description": "When before_unix_ts is omitted, ignore records from the last N seconds to reduce current-turn contamination"
        }
      },
      "required": ["query"]
    }
  },
  {
    "type": "function",
    "name": "disconnect_session",
    "description": "Gracefully close the voice connection. Call this when the user says goodbye, asks you to leave, or says something like '退下吧'. Say your farewell BEFORE calling this tool — once called, the connection will close shortly after.",
    "parameters": { "type": "object", "properties": {} }
  },
  {
    "type": "function",
    "name": "fetch_webpage",
    "description": "Fetch the text content of a webpage. Returns the sanitized Markdown content. Useful for reading documentation or checking specific links.",
    "parameters": {
      "type": "object",
      "properties": {
        "url": { "type": "string", "description": "The URL of the webpage to fetch" }
      },
      "required": ["url"]
    }
  },
  {
    "type": "function",
    "name": "search_web_duckduckgo",
    "description": "Search DuckDuckGo and return top search results (URLs and snippets). Useful for discovering information, looking up recent news, or finding documentation links.",
    "parameters": {
      "type": "object",
      "properties": {
        "query": { "type": "string", "description": "The search query (try to use English keywords for better coding/tech results, but any language works)" }
      },
      "required": ["query"]
    }
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
            | "get_last_chat_time"
            | "query_memory_evidence"
            | "search_knowledge"
    )
}

      #[derive(Serialize)]
      struct MemoryEvidenceHit {
        session_id: String,
        role: String,
        created_at: i64,
        created_local: String,
        content_preview: String,
        score: f32,
      }

      fn tokenize_lower(s: &str) -> Vec<String> {
        s.to_lowercase()
          .split(|c: char| !c.is_alphanumeric())
          .filter(|t| !t.is_empty())
          .map(|t| t.to_string())
          .collect()
      }

      fn semantic_overlap_score(query_tokens: &[String], text_tokens: &[String]) -> f32 {
        if query_tokens.is_empty() || text_tokens.is_empty() {
          return 0.0;
        }
        let mut overlap = 0usize;
        for q in query_tokens {
          if text_tokens.iter().any(|t| t == q) {
            overlap += 1;
          }
        }
        overlap as f32 / query_tokens.len() as f32
      }

      fn split_cjk_chars(s: &str) -> Vec<String> {
        s.chars()
          .filter(|c| {
            let is_han = ('\u{4E00}'..='\u{9FFF}').contains(c)
              || ('\u{3400}'..='\u{4DBF}').contains(c);
            let is_hira_kata = ('\u{3040}'..='\u{30FF}').contains(c);
            let is_hangul = ('\u{AC00}'..='\u{D7AF}').contains(c);
            is_han || is_hira_kata || is_hangul
          })
          .map(|c| c.to_string())
          .collect()
      }

      fn resolve_evidence_cutoff(before_unix_ts: Option<i64>, exclude_recent_seconds: i64, now_ts: i64) -> i64 {
        let fallback = now_ts - exclude_recent_seconds.clamp(0, MAX_EXCLUDE_RECENT_SECONDS);
        let min_allowed = now_ts - MAX_MEMORY_LOOKBACK_SECONDS;
        match before_unix_ts {
          Some(ts) if ts >= min_allowed && ts <= now_ts + 60 => ts,
          _ => fallback,
        }
      }

      fn format_local_ts(ts: i64) -> String {
        chrono::Local
          .timestamp_opt(ts, 0)
          .single()
          .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
          .unwrap_or_else(|| "unknown".to_string())
      }

/// Dispatch UI tool (requires AppHandle to create windows / open external apps)
pub async fn dispatch_ui_tool(app: &AppHandle, name: &str, args_json: &str) -> String {
  let args: serde_json::Value = match serde_json::from_str(args_json) {
    Ok(v) => v,
    Err(e) => {
      let msg = format!("Invalid tool args JSON: {}", e);
      log::error!("[Tools] {}", msg);
      return format!("Error: {}", msg);
    }
  };
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
            let limit = args["limit"].as_i64().unwrap_or(10).clamp(1, 30) as usize;
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
            "get_last_chat_time" => {
              let speaker_scope = args["speaker_scope"].as_str().unwrap_or("both").to_string();
              if speaker_scope != "both"
                && speaker_scope != "user_only"
                && speaker_scope != "assistant_only"
              {
                return "Error: speaker_scope must be 'both', 'user_only', or 'assistant_only'"
                  .to_string();
              }

              let exclude_recent_seconds = args["exclude_recent_seconds"]
                .as_i64()
                .unwrap_or(DEFAULT_EXCLUDE_RECENT_SECONDS)
                .clamp(0, MAX_EXCLUDE_RECENT_SECONDS);
              let now_ts = chrono::Utc::now().timestamp();
              let cutoff = resolve_evidence_cutoff(
                args["before_unix_ts"].as_i64(),
                exclude_recent_seconds,
                now_ts,
              );

              let db_state = app.state::<DbState>();
              let pool = &db_state.0;

              let last_any = sqlx::query(
                "SELECT role, created_at, session_id, content
                 FROM episodic_logs
                 WHERE created_at < ?1 AND role IN ('user', 'assistant')
                 ORDER BY created_at DESC
                 LIMIT 1",
              )
              .bind(cutoff)
              .fetch_optional(pool)
              .await;

              let last_user = sqlx::query(
                "SELECT created_at, session_id, content
                 FROM episodic_logs
                 WHERE created_at < ?1 AND role = 'user'
                 ORDER BY created_at DESC
                 LIMIT 1",
              )
              .bind(cutoff)
              .fetch_optional(pool)
              .await;

              let last_assistant = sqlx::query(
                "SELECT created_at, session_id, content
                 FROM episodic_logs
                 WHERE created_at < ?1 AND role = 'assistant'
                 ORDER BY created_at DESC
                 LIMIT 1",
              )
              .bind(cutoff)
              .fetch_optional(pool)
              .await;

              let (last_any_role, last_any_ts, last_any_session, last_any_preview) = match last_any {
                Ok(Some(row)) => {
                  let role: String = row.get(0);
                  let ts: i64 = row.get(1);
                  let sid: String = row.get(2);
                  let content: String = row.get(3);
                  let preview = if content.len() > 120 {
                    format!("{}...", &content[..content.floor_char_boundary(120)])
                  } else {
                    content
                  };
                  (Some(role), Some(ts), Some(sid), Some(preview))
                }
                Ok(None) => (None, None, None, None),
                Err(e) => return format!("Error querying last_any chat time: {}", e),
              };

              let map_row = |res: Result<Option<sqlx::sqlite::SqliteRow>, sqlx::Error>| {
                match res {
                  Ok(Some(row)) => {
                    let ts: i64 = row.get(0);
                    let sid: String = row.get(1);
                    let content: String = row.get(2);
                    let preview = if content.len() > 120 {
                      format!("{}...", &content[..content.floor_char_boundary(120)])
                    } else {
                      content
                    };
                    (Some(ts), Some(sid), Some(preview), None::<String>)
                  }
                  Ok(None) => (None, None, None, None),
                  Err(e) => (None, None, None, Some(e.to_string())),
                }
              };

              let (last_user_ts, last_user_session, last_user_preview, user_err) = map_row(last_user);
              if let Some(e) = user_err {
                return format!("Error querying last_user chat time: {}", e);
              }
              let (last_assistant_ts, last_assistant_session, last_assistant_preview, assistant_err) =
                map_row(last_assistant);
              if let Some(e) = assistant_err {
                return format!("Error querying last_assistant chat time: {}", e);
              }

              let target = match speaker_scope.as_str() {
                "user_only" => last_user_ts,
                "assistant_only" => last_assistant_ts,
                _ => last_any_ts,
              };

              serde_json::json!({
                "matched": target.is_some(),
                "speaker_scope": speaker_scope,
                "cutoff_unix_ts": cutoff,
                "last_any": {
                  "role": last_any_role,
                  "created_at": last_any_ts,
                  "created_local": last_any_ts.map(format_local_ts),
                  "session_id": last_any_session,
                  "content_preview": last_any_preview,
                },
                "last_user": {
                  "created_at": last_user_ts,
                  "created_local": last_user_ts.map(format_local_ts),
                  "session_id": last_user_session,
                  "content_preview": last_user_preview,
                },
                "last_assistant": {
                  "created_at": last_assistant_ts,
                  "created_local": last_assistant_ts.map(format_local_ts),
                  "session_id": last_assistant_session,
                  "content_preview": last_assistant_preview,
                },
              })
              .to_string()
            }
            "query_memory_evidence" => {
              let query = args["query"].as_str().unwrap_or("").trim().to_string();
              if query.is_empty() {
                return "Error: query is empty".to_string();
              }

              let mode = args["mode"].as_str().unwrap_or("exact").to_string();
              if mode != "exact" && mode != "semantic" {
                return "Error: mode must be 'exact' or 'semantic'".to_string();
              }

              let speaker_scope = args["speaker_scope"].as_str().unwrap_or("both").to_string();
              if speaker_scope != "both"
                && speaker_scope != "user_only"
                && speaker_scope != "assistant_only"
              {
                return "Error: speaker_scope must be 'both', 'user_only', or 'assistant_only'"
                  .to_string();
              }

              let limit = args["limit"].as_i64().unwrap_or(5).clamp(1, 20) as usize;
              let scan_limit = args["scan_limit"].as_i64().unwrap_or(200).clamp(20, 500) as i64;
              let exclude_recent_seconds = args["exclude_recent_seconds"]
                .as_i64()
                .unwrap_or(DEFAULT_EXCLUDE_RECENT_SECONDS)
                .clamp(0, MAX_EXCLUDE_RECENT_SECONDS);
              let now_ts = chrono::Utc::now().timestamp();
              let cutoff = resolve_evidence_cutoff(
                args["before_unix_ts"].as_i64(),
                exclude_recent_seconds,
                now_ts,
              );

              let db_state = app.state::<DbState>();
              let pool = &db_state.0;

              let rows_result = if speaker_scope == "both" {
                sqlx::query(
                  "SELECT session_id, role, content, created_at
                   FROM episodic_logs
                   WHERE created_at < ?1 AND role IN ('user', 'assistant')
                   ORDER BY created_at DESC
                   LIMIT ?2",
                )
                .bind(cutoff)
                .bind(scan_limit)
                .fetch_all(pool)
                .await
              } else if speaker_scope == "user_only" {
                sqlx::query(
                  "SELECT session_id, role, content, created_at
                   FROM episodic_logs
                   WHERE created_at < ?1 AND role = 'user'
                   ORDER BY created_at DESC
                   LIMIT ?2",
                )
                .bind(cutoff)
                .bind(scan_limit)
                .fetch_all(pool)
                .await
              } else {
                sqlx::query(
                  "SELECT session_id, role, content, created_at
                   FROM episodic_logs
                   WHERE created_at < ?1 AND role = 'assistant'
                   ORDER BY created_at DESC
                   LIMIT ?2",
                )
                .bind(cutoff)
                .bind(scan_limit)
                .fetch_all(pool)
                .await
              };

              let rows = match rows_result {
                Ok(r) => r,
                Err(e) => return format!("Error querying episodic memory: {}", e),
              };

              let query_lower = query.to_lowercase();
              let query_tokens = tokenize_lower(&query);
              let query_cjk = split_cjk_chars(&query);
              let mut hits: Vec<MemoryEvidenceHit> = Vec::new();

              for row in rows {
                let session_id: String = row.get(0);
                let role: String = row.get(1);
                let content: String = row.get(2);
                let created_at: i64 = row.get(3);

                let score = if mode == "exact" {
                  if content.to_lowercase().contains(&query_lower) {
                    1.0
                  } else {
                    0.0
                  }
                } else {
                  let content_lower = content.to_lowercase();
                  if content_lower.contains(&query_lower) {
                    1.0
                  } else {
                  let content_tokens = tokenize_lower(&content);
                  let token_score = semantic_overlap_score(&query_tokens, &content_tokens);
                  if token_score > 0.0 {
                    token_score
                  } else {
                    // Fallback for CJK queries where alnum tokenization may produce weak signals.
                    let content_cjk = split_cjk_chars(&content);
                    semantic_overlap_score(&query_cjk, &content_cjk)
                  }
                  }
                };

                if score <= 0.0 {
                  continue;
                }

                let content_preview = if content.len() > 180 {
                  format!("{}...", &content[..content.floor_char_boundary(180)])
                } else {
                  content
                };

                let created_local = format_local_ts(created_at);

                hits.push(MemoryEvidenceHit {
                  session_id,
                  role,
                  created_at,
                  created_local,
                  content_preview,
                  score,
                });
              }

              hits.sort_by(|a, b| {
                b.score
                  .partial_cmp(&a.score)
                  .unwrap_or(std::cmp::Ordering::Equal)
                  .then_with(|| b.created_at.cmp(&a.created_at))
              });

              let total_hits = hits.len();
              let evidence: Vec<MemoryEvidenceHit> = hits.into_iter().take(limit).collect();
              let top_score = evidence.first().map(|h| h.score).unwrap_or(0.0);

              serde_json::json!({
                "matched": !evidence.is_empty(),
                "query": query,
                "mode": mode,
                "speaker_scope": speaker_scope,
                "cutoff_unix_ts": cutoff,
                "scanned_limit": scan_limit,
                "total_hits": total_hits,
                "confidence": top_score,
                "evidence": evidence,
              })
              .to_string()
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
        "fetch_webpage" => {
            let url = args.get("url").and_then(|u| u.as_str()).unwrap_or_default();
            crate::agent::web_tools::fetch_webpage(url)
        }
        "search_web_duckduckgo" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or_default();
            crate::agent::web_tools::search_web_duckduckgo(query)
        }
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

#[cfg(test)]
mod tests {
  use super::{format_local_ts, resolve_evidence_cutoff, semantic_overlap_score, split_cjk_chars, tokenize_lower};

  #[test]
  fn cutoff_prefers_explicit_before_timestamp() {
    let cutoff = resolve_evidence_cutoff(Some(1234), 30, 9999);
    assert_eq!(cutoff, 1234);
  }

  #[test]
  fn cutoff_uses_now_minus_exclude_recent_when_before_not_set() {
    let cutoff = resolve_evidence_cutoff(None, 5, 1000);
    assert_eq!(cutoff, 995);
  }

  #[test]
  fn cutoff_rejects_too_old_before_timestamp() {
    let now = 1_800_000_000;
    let too_old = now - (31 * 24 * 60 * 60);
    let cutoff = resolve_evidence_cutoff(Some(too_old), 7, now);
    assert_eq!(cutoff, now - 7);
  }

  #[test]
  fn cutoff_rejects_future_before_timestamp() {
    let now = 1_800_000_000;
    let too_future = now + 300;
    let cutoff = resolve_evidence_cutoff(Some(too_future), 9, now);
    assert_eq!(cutoff, now - 9);
  }

  #[test]
  fn semantic_overlap_scores_expected_fraction() {
    let q = tokenize_lower("learn rust memory");
    let t = tokenize_lower("we learn rust today");
    let s = semantic_overlap_score(&q, &t);
    assert!((s - (2.0 / 3.0)).abs() < 0.001);
  }

  #[test]
  fn cjk_char_split_supports_semantic_fallback() {
    let q = split_cjk_chars("上次聊天时间");
    let t = split_cjk_chars("我们上次聊天是什么时候");
    let s = semantic_overlap_score(&q, &t);
    assert!(s > 0.4);
  }

  #[test]
  fn format_local_ts_returns_readable_string() {
    let text = format_local_ts(1_700_000_000);
    assert!(text.len() >= 16);
    assert!(text.contains('-'));
    assert!(text.contains(':'));
  }
}
