/// memory_injection — provider-agnostic memory injection policy + pack encoding
///
/// This module separates two concerns:
/// 1) Policy: what memory to inject for each connection scenario.
/// 2) Protocol encoding: how those memory items are converted into provider wire messages.
use crate::agent::memory::MemoryStore;
use crate::agent::realtime_ws;
use chrono::{Local, TimeZone};

/// Session connection scenarios that need context injection.
pub enum InjectionScenario {
    NewSession {
        greeting: String,
    },
    VoiceSwitch {
        greeting: String,
    },
    SilentReconnect {
        max_turns: usize,
    },
    ToolRetry {
        max_turns: usize,
        retry_hint: String,
    },
}

/// Provider-agnostic context pack produced by policy.
pub struct MemoryContextPack {
    pub speech_prompts: Vec<String>,
    pub timeline_items: Vec<(String, String)>,
    pub temporal_anchor: Option<String>,
}

/// Encoded message batch with degradation metadata.
pub struct EncodedInjection {
    pub messages: Vec<String>,
    pub timeline_items: usize,
    pub dropped_timeline_items: usize,
}

pub struct DefaultMemoryInjectionPolicy;

impl DefaultMemoryInjectionPolicy {
    pub async fn build_pack(
        &self,
        memory: &MemoryStore,
        scenario: InjectionScenario,
    ) -> MemoryContextPack {
        match scenario {
            InjectionScenario::NewSession { greeting }
            | InjectionScenario::VoiceSwitch { greeting } => MemoryContextPack {
                speech_prompts: vec![greeting],
                timeline_items: Vec::new(),
                temporal_anchor: None,
            },
            InjectionScenario::SilentReconnect { max_turns } => {
                let items = memory
                    .recent_context_with_meta(max_turns)
                    .await
                    .unwrap_or_default();
                let temporal_anchor = build_temporal_anchor(&items);
                let timeline_items = items.into_iter().map(|(r, c, _, _)| (r, c)).collect();
                MemoryContextPack {
                    speech_prompts: Vec::new(),
                    timeline_items,
                    temporal_anchor,
                }
            }
            InjectionScenario::ToolRetry {
                max_turns,
                retry_hint,
            } => {
                let items = memory
                    .recent_context_with_meta(max_turns)
                    .await
                    .unwrap_or_default();
                let temporal_anchor = build_temporal_anchor(&items);
                let timeline_items = items.into_iter().map(|(r, c, _, _)| (r, c)).collect();
                MemoryContextPack {
                    speech_prompts: vec![retry_hint],
                    timeline_items,
                    temporal_anchor,
                }
            }
        }
    }
}

pub fn encode_pack(
    protocol: &dyn realtime_ws::RealtimeProtocol,
    pack: &MemoryContextPack,
) -> EncodedInjection {
    let mut messages = Vec::new();
    for prompt in &pack.speech_prompts {
        messages.push(protocol.inject_system_directive(prompt));
    }

    let mut dropped_timeline_items = 0;
    for (role, text) in &pack.timeline_items {
        if let Some(msg) = protocol.conversation_inject_msg(role, text) {
            messages.push(msg);
        } else {
            dropped_timeline_items += 1;
        }
    }

    // Root fallback: providers without timeline injection should still receive
    // a compact continuity brief instead of silently losing all context.
    if dropped_timeline_items > 0 && !protocol.supports_timeline_injection() {
        let brief = build_continuity_brief(&pack.timeline_items, pack.temporal_anchor.as_deref());
        if !brief.is_empty() {
            messages.push(protocol.inject_system_directive(&brief));
        }
    }

    EncodedInjection {
        messages,
        timeline_items: pack.timeline_items.len(),
        dropped_timeline_items,
    }
}

fn build_continuity_brief(items: &[(String, String)], temporal_anchor: Option<&str>) -> String {
    if items.is_empty() {
        return String::new();
    }

    // Keep this short to avoid over-injecting on reconnect.
    let tail = items.iter().rev().take(6).collect::<Vec<_>>();
    let mut lines = Vec::new();
    for (role, text) in tail.into_iter().rev() {
        if role != "user" && role != "assistant" {
            continue;
        }
        let label = if role == "user" { "User" } else { "Assistant" };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let snippet = if trimmed.len() > 120 {
            format!("{}...", &trimmed[..trimmed.floor_char_boundary(120)])
        } else {
            trimmed.to_string()
        };
        lines.push(format!("- {}: {}", label, snippet));
    }

    if lines.is_empty() {
        return String::new();
    }

    let anchor_line = temporal_anchor
        .map(|s| format!("Time anchors: {}\n", s))
        .unwrap_or_default();

    format!(
        "[Continuity Brief] You reconnected and timeline injection is unavailable for this provider. Use this as immediate context and continue naturally:\n{}{}",
        anchor_line,
        lines.join("\n")
    )
}

fn build_temporal_anchor(items: &[(String, String, i64, String)]) -> Option<String> {
    let mut last_user: Option<i64> = None;
    let mut last_assistant: Option<i64> = None;

    for (role, _content, ts, _session_id) in items {
        if role == "user" {
            last_user = Some(*ts);
        } else if role == "assistant" {
            last_assistant = Some(*ts);
        }
    }

    let format_ts = |ts: i64| {
        Local
            .timestamp_opt(ts, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "unknown".to_string())
    };

    match (last_user, last_assistant) {
        (None, None) => None,
        (Some(u), None) => Some(format!("last user: {}", format_ts(u))),
        (None, Some(a)) => Some(format!("last assistant: {}", format_ts(a))),
        (Some(u), Some(a)) => Some(format!(
            "last user: {}; last assistant: {}",
            format_ts(u),
            format_ts(a)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    struct NoTimelineProtocol;

    impl realtime_ws::RealtimeProtocol for NoTimelineProtocol {
        fn build_connect_url(&self, _base_url: &str, _api_key: &str, _model: &str) -> String {
            String::new()
        }

        fn build_log_url(&self, _base_url: &str, _api_key: &str, _model: &str) -> String {
            String::new()
        }

        fn build_session_update(
            &self,
            _instructions: &str,
            _tools: &[Value],
            _model: &str,
            _voice: &str,
        ) -> Value {
            serde_json::json!({})
        }

        fn audio_append_msg(&self, _base64_audio: &str) -> String {
            String::new()
        }

        fn parse_events(&self, _raw: &str) -> Vec<realtime_ws::WsEvent> {
            Vec::new()
        }

        fn inject_system_directive(&self, text: &str) -> String {
            format!("SPEECH:{}", text)
        }

        fn function_call_output_msg(&self, _call_id: &str, _name: &str, _output: &str) -> String {
            String::new()
        }
    }

    struct TimelineProtocol;

    impl realtime_ws::RealtimeProtocol for TimelineProtocol {
        fn build_connect_url(&self, _base_url: &str, _api_key: &str, _model: &str) -> String {
            String::new()
        }

        fn build_log_url(&self, _base_url: &str, _api_key: &str, _model: &str) -> String {
            String::new()
        }

        fn build_session_update(
            &self,
            _instructions: &str,
            _tools: &[Value],
            _model: &str,
            _voice: &str,
        ) -> Value {
            serde_json::json!({})
        }

        fn audio_append_msg(&self, _base64_audio: &str) -> String {
            String::new()
        }

        fn parse_events(&self, _raw: &str) -> Vec<realtime_ws::WsEvent> {
            Vec::new()
        }

        fn inject_system_directive(&self, text: &str) -> String {
            format!("SPEECH:{}", text)
        }

        fn function_call_output_msg(&self, _call_id: &str, _name: &str, _output: &str) -> String {
            String::new()
        }

        fn conversation_inject_msg(&self, role: &str, text: &str) -> Option<String> {
            Some(format!("TIMELINE:{}:{}", role, text))
        }

        fn supports_timeline_injection(&self) -> bool {
            true
        }
    }

    #[test]
    fn encode_pack_falls_back_to_continuity_brief_without_timeline_support() {
        let protocol = NoTimelineProtocol;
        let pack = MemoryContextPack {
            speech_prompts: Vec::new(),
            timeline_items: vec![
                ("user".to_string(), "hello there".to_string()),
                ("assistant".to_string(), "hi".to_string()),
            ],
            temporal_anchor: Some("last user: 2026-03-24 10:00".to_string()),
        };

        let encoded = encode_pack(&protocol, &pack);
        assert_eq!(encoded.timeline_items, 2);
        assert_eq!(encoded.dropped_timeline_items, 2);
        assert_eq!(encoded.messages.len(), 1);
        assert!(encoded.messages[0].contains("Continuity Brief"));
        assert!(encoded.messages[0].contains("Time anchors"));
    }

    #[test]
    fn encode_pack_prefers_timeline_messages_when_supported() {
        let protocol = TimelineProtocol;
        let pack = MemoryContextPack {
            speech_prompts: Vec::new(),
            timeline_items: vec![("user".to_string(), "hello".to_string())],
            temporal_anchor: None,
        };

        let encoded = encode_pack(&protocol, &pack);
        assert_eq!(encoded.dropped_timeline_items, 0);
        assert_eq!(encoded.messages.len(), 1);
        assert!(encoded.messages[0].starts_with("TIMELINE:user:hello"));
    }

    #[test]
    fn temporal_anchor_uses_latest_user_and_assistant() {
        let anchor = build_temporal_anchor(&[
            (
                "user".to_string(),
                "u1".to_string(),
                1_700_000_000,
                "s1".to_string(),
            ),
            (
                "assistant".to_string(),
                "a1".to_string(),
                1_700_000_100,
                "s1".to_string(),
            ),
            (
                "user".to_string(),
                "u2".to_string(),
                1_700_000_200,
                "s1".to_string(),
            ),
        ]);

        let text = anchor.unwrap_or_default();
        assert!(text.contains("last user"));
        assert!(text.contains("last assistant"));
    }
}
