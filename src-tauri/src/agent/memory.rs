/// memory — Conversation persistence + context retrieval
///
/// Asynchronous struct held directly by the Session.
/// Responsibilities:
///   1. Persist transcripts/tool results to SQLite (single timeline)
///   2. Build system instructions (agent name + persona + long-term memories)
///   3. Retrieve recent conversation context for WS reconnection
///   4. Provide greeting/voice-switch prompts
use chrono::{Local, TimeZone, Timelike};

// ============================================================
// MemoryStore
// ============================================================

pub struct MemoryStore {
    pool: sqlx::SqlitePool,
    session_id: String,
}

impl MemoryStore {
    /// Create a new MemoryStore.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        let session_id = uuid::Uuid::new_v4().to_string();
        Self { pool, session_id }
    }

    /// Persist a message to the episodic_logs table.
    /// Returns a Result to allow proper error propagation.
    pub async fn persist(&self, role: &str, content: &str) -> Result<(), String> {
        let pool = &self.pool;

        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().timestamp();

        crate::db::add_episodic_log(pool, &id, &self.session_id, role, content, created_at)
            .await
            .map_err(|e| {
                log::error!("[Memory] Failed to persist message: {}", e);
                e.to_string()
            })
    }

    /// Persist a tool result as a formatted message.
    pub async fn persist_tool_result(&self, tool_name: &str, output: &str) -> Result<(), String> {
        self.persist("tool", &format!("[{}] {}", tool_name, output))
            .await
    }

    /// Build the full system instructions for the Realtime API session.
    pub async fn build_instructions(&self) -> Result<String, String> {
        let pool = &self.pool;

        // Fetch agent name
        let agent_name: String =
            sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'agent_name'")
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    log::error!("[Memory] Failed to fetch agent_name: {}", e);
                    e.to_string()
                })?
                .unwrap_or_default();

        // Fetch core profile
        let profile_entries = crate::db::get_core_profile(pool).await.map_err(|e| {
            log::error!("[Memory] Failed to fetch core_profile: {}", e);
            e.to_string()
        })?;

        let name_line = if agent_name.is_empty() {
            crate::i18n::t("prompt.suggest_name")
        } else {
            format!("Your name is \"{}\". ", agent_name)
        };

        let memory_section = if profile_entries.is_empty() {
            String::new()
        } else {
            let mut s = "\n\nKey facts you know about the user:\n".to_string();
            for entry in &profile_entries {
                s.push_str(&format!("- {}: {}\n", entry.key, entry.value));
            }
            s
        };

        let base = "You are a warm, friendly voice assistant — a system-level AI agent with full OS access.\n\
            Respond naturally in the user's language. Be concise and conversational.\n\
            Adapt tone and pace to the user's emotional state — gentle when frustrated, brief when rushed, playful when relaxed.\n\n\
            PRIVATE MODE: one user only. Use one-to-one wording and avoid any group/broadcast-style greetings.\n\n\
            You have tools to interact with the operating system: run shell commands, control keyboard/mouse, capture screenshots, \
            open files/URLs, store memories, display content on a board, and set reminders.\n\
            Use these tools when the user asks for system operations, information lookup, or anything beyond casual chat.\n\
            Always output a brief spoken acknowledgment before or alongside tool calls to avoid dead silence.\n\
            When a tool returns results, incorporate them naturally into your spoken response.\n\n\
            AGENTIC BEHAVIOR — For multi-step tasks:\n\
            1. PLAN: When the user asks for something that requires multiple steps, briefly say your plan out loud (e.g. \"OK, I'll first open Notepad, then type the text, then save it.\").\n\
            2. EXECUTE STEP BY STEP: You can call tools multiple rounds. After each tool result comes back, you get another chance to call more tools. Do NOT try to do everything in one shot — break complex tasks into small steps.\n\
            3. OBSERVE → ACT → VERIFY: For GUI operations, use observe_screen before acting to see the current state. After performing actions (clicking, typing), use observe_screen again to verify the result. This loop lets you self-correct.\n\
            4. SELF-CORRECT: If a tool returns an error or unexpected result, diagnose the issue and try an alternative approach. Do not immediately give up or report failure to the user.\n\
            5. COMPLETION: Only give your final spoken summary to the user after you have verified the task is actually done. Do not assume success — confirm it.\n\
            6. KEEP IT NATURAL: Between tool calls, you may speak brief status updates (\"OK, got it\", \"Let me check...\") so the user knows you're working. Avoid long silences.\n\n\
            MEMORY MANAGEMENT:\n\
            - SOTA Cognitive Architecture: You have a \"Core Profile\" for basic facts/preferences and a \"Knowledge Graph\" for complex relations.\n\
            - When the user shares strict preferences or vital static info, IMMEDIATELY use update_core_profile to store them (e.g. key: 'name', value: 'Alice').\n\
            - For relations and world knowledge, use add_knowledge_node and add_knowledge_edge. Establish links between entities.\n\
            - Use search_knowledge when you need to recall something specific the user mentioned before.\n\
                        - If the user asks when we last chatted, call get_last_chat_time first.\n\
                        - If the user asks whether something was said before (or asks exact prior wording), call query_memory_evidence first; do not guess from vague memory.\n- MOST IMPORTANTLY: You are now only provided with the most recent 3 turns of conversation on start. You MUST use search_knowledge or query_memory_evidence to recall older context. DO NOT GUESS.\n\
              - Be proactive with memory! Don't wait until the conversation ends.\n\n\
              CRITICAL: IF YOU ARE CONNECTED VIA A PLATFORM THAT PROHIBITS COMBINING AUDIO AND FUNCTION CALLS (e.g. Gemini Multimodal Live API), EXECUTING TOOLS AND SPEAKING IN THE SAME TURN MAY CAUSE A PROTOCOL CRASH. TO BE SAFE, WHEN YOU CALL A TOOL OR FUNCTION, YOU SHOULD DO IT SILENTLY WITHOUT GENERATING ANY SPOKEN TEXT/AUDIO IN THAT EXACT SAME RESPONSE ROUND.";
        let language_directive = format!(
            "LANGUAGE: Always speak in {}. This includes greetings, acknowledgments, and all spoken output.\n\n",
            crate::i18n::language_name()
        );

        Ok(format!(
            "{}{}{}{}{}",
            language_directive,
            name_line,
            base,
            memory_section,
            self.last_session_summary(10).await
        ))
    }

    /// Generate contextual greeting prompt based on trigger + time of day.
    pub fn contextual_greeting(&self, trigger: &str, task_desc: Option<&str>) -> String {
        let hour = chrono::Local::now().hour();
        let time_hint = match hour {
            6..=11 => "It's morning. Greet warmly with a morning greeting.",
            12..=17 => "It's afternoon. Be casual and friendly.",
            18..=23 => "It's evening. Use a relaxed, wind-down tone.",
            _ => "It's late night. Be brief and gentle.",
        };
        match trigger {
            "scheduled_task" => {
                format!(
                    "You reconnected because of a scheduled task: \"{}\". {} Remind the user naturally.",
                    task_desc.unwrap_or("unknown task"),
                    time_hint
                )
            }
            "voice_switch" => {
                format!(
                    "You just switched to a new voice. {} Say a short greeting so the user can hear how this voice sounds.",
                    time_hint
                )
            }
            "wakeword" => {
                format!(
                    "User called you. {} Greet naturally with a short sentence.",
                    time_hint
                )
            }
            _ => {
                format!(
                    "You just came online. {} Greet the user with a short, warm sentence.",
                    time_hint
                )
            }
        }
    }

    /// Retrieve recent conversation messages for context injection after WS reconnection.
    /// Returns (role, content) pairs in chronological order (oldest first).
    pub async fn recent_context(&self, max_turns: usize) -> Result<Vec<(String, String)>, String> {
        self.recent_context_with_meta(max_turns).await.map(|items| {
            items
                .into_iter()
                .map(|(role, content, _created_at, _session_id)| (role, content))
                .collect()
        })
    }

    /// Retrieve recent conversation messages with metadata.
    /// Returns (role, content, created_at, session_id) in chronological order (oldest first).
    pub async fn recent_context_with_meta(
        &self,
        max_turns: usize,
    ) -> Result<Vec<(String, String, i64, String)>, String> {
        let pool = &self.pool;

        crate::db::get_recent_episodes(pool, max_turns)
            .await
            .map(|episodes| {
                episodes
                    .into_iter()
                    .map(|log| (log.role, log.content, log.created_at, log.session_id))
                    .collect()
            })
            .map_err(|e| {
                log::error!("[Memory] Failed to get recent episodes: {}", e);
                e.to_string()
            })
    }

    /// Build a brief human-readable summary of recent conversation for system instructions.
    /// Used for NEW sessions so AI knows what happened before without confusing it
    /// with raw conversation items (which would be treated as current dialogue).
    pub async fn last_session_summary(&self, max_turns: usize) -> String {
        let messages = match self.recent_context_with_meta(max_turns).await {
            Ok(m) => m,
            Err(e) => {
                log::warn!(
                    "[Memory] Could not retrieve recent context for summary: {}",
                    e
                );
                return String::new();
            }
        };

        if messages.is_empty() {
            return String::new();
        }

        // Filter out tool messages and keep only user/assistant
        let relevant: Vec<&(String, String, i64, String)> = messages
            .iter()
            .filter(|(role, _, _, _)| role == "user" || role == "assistant")
            .collect();

        if relevant.is_empty() {
            return String::new();
        }

        let mut summary = String::from("\n\nRecent conversation history (from a PREVIOUS session — this is background context, NOT the current conversation):\n");
        for (role, content, created_at, _session_id) in &relevant {
            let label = if role == "user" { "User" } else { "You" };
            let ts = Local
                .timestamp_opt(*created_at, 0)
                .single()
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "unknown time".to_string());
            let truncated = if content.len() > 150 {
                format!("{}...", &content[..content.floor_char_boundary(150)])
            } else {
                content.clone()
            };
            summary.push_str(&format!("  [{}] {} said: {}\n", ts, label, truncated));
        }
        summary.push_str(
            "(This conversation has ENDED. Start fresh — do not continue it or respond to it.)",
        );
        summary
    }
}
