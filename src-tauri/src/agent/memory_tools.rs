/// memory_tools — Memory evidence query tools (extracted from tools.rs)
///
/// Handles `get_last_chat_time` and `query_memory_evidence` tool dispatch.
/// Contains text similarity helpers for semantic memory search.
use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, Manager};

use crate::db::DbState;

const DEFAULT_EXCLUDE_RECENT_SECONDS: i64 = 2;
const MAX_EXCLUDE_RECENT_SECONDS: i64 = 120;
const MAX_MEMORY_LOOKBACK_SECONDS: i64 = 30 * 24 * 60 * 60;

// ============================================================
// Types
// ============================================================

#[derive(Serialize)]
struct MemoryEvidenceHit {
    session_id: String,
    role: String,
    created_at: i64,
    created_local: String,
    content_preview: String,
    score: f32,
}

// ============================================================
// Text similarity helpers
// ============================================================

pub(crate) fn tokenize_lower(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

pub(crate) fn semantic_overlap_score(query_tokens: &[String], text_tokens: &[String]) -> f32 {
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

pub(crate) fn split_cjk_chars(s: &str) -> Vec<String> {
    s.chars()
        .filter(|c| {
            let is_han =
                ('\u{4E00}'..='\u{9FFF}').contains(c) || ('\u{3400}'..='\u{4DBF}').contains(c);
            let is_hira_kata = ('\u{3040}'..='\u{30FF}').contains(c);
            let is_hangul = ('\u{AC00}'..='\u{D7AF}').contains(c);
            is_han || is_hira_kata || is_hangul
        })
        .map(|c| c.to_string())
        .collect()
}

pub(crate) fn resolve_evidence_cutoff(
    before_unix_ts: Option<i64>,
    exclude_recent_seconds: i64,
    now_ts: i64,
) -> i64 {
    let fallback = now_ts - exclude_recent_seconds.clamp(0, MAX_EXCLUDE_RECENT_SECONDS);
    let min_allowed = now_ts - MAX_MEMORY_LOOKBACK_SECONDS;
    match before_unix_ts {
        Some(ts) if ts >= min_allowed && ts <= now_ts + 60 => ts,
        _ => fallback,
    }
}

pub(crate) fn format_local_ts(ts: i64) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

use chrono::TimeZone;

// ============================================================
// Tool dispatch handlers
// ============================================================

/// Handle `get_last_chat_time` tool call
pub async fn handle_get_last_chat_time(app: &AppHandle, args: &serde_json::Value) -> String {
    let speaker_scope = args["speaker_scope"]
        .as_str()
        .unwrap_or("both")
        .to_string();
    if speaker_scope != "both" && speaker_scope != "user_only" && speaker_scope != "assistant_only"
    {
        return "Error: speaker_scope must be 'both', 'user_only', or 'assistant_only'".to_string();
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

    let map_row = |res: Result<Option<sqlx::sqlite::SqliteRow>, sqlx::Error>| match res {
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

/// Handle `query_memory_evidence` tool call
pub async fn handle_query_memory_evidence(
    app: &AppHandle,
    args: &serde_json::Value,
) -> String {
    let query = args["query"].as_str().unwrap_or("").trim().to_string();
    if query.is_empty() {
        return "Error: query is empty".to_string();
    }

    let mode = args["mode"].as_str().unwrap_or("exact").to_string();
    if mode != "exact" && mode != "semantic" {
        return "Error: mode must be 'exact' or 'semantic'".to_string();
    }

    let speaker_scope = args["speaker_scope"]
        .as_str()
        .unwrap_or("both")
        .to_string();
    if speaker_scope != "both" && speaker_scope != "user_only" && speaker_scope != "assistant_only"
    {
        return "Error: speaker_scope must be 'both', 'user_only', or 'assistant_only'".to_string();
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

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

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
