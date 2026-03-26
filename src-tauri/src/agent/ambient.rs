/// ambient — Proactive perception stream: background screen observation + behavior pattern detection
///
/// Periodically captures screenshots silently, compares differences via perceptual hash,
/// and sends changes to the session for analysis when necessary.
/// Detects user stagnation patterns (same screen > 5 minutes), can trigger proactive prompts.
/// Disabled by default, controlled by app_settings.ambient_enabled.
use crate::db::DbState;
use crate::system_api;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

// ============================================================
// Type Definitions
// ============================================================

/// Lightweight HTTP LLM config for screen analysis
#[derive(Clone)]
struct HttpLlmConfig {
    base_url: String,
    api_key: String,
    model: String,
}

/// Ambient perception event → bridged to session inject channel
#[derive(Debug)]
pub enum AmbientEvent {
    /// Detected that user may need help (stagnation pattern)
    ProactivePrompt(String),
    /// Ambient context update (screen summary)
    #[allow(dead_code)]
    ContextUpdate(String),
}

/// AmbientWatcher handle
pub struct AmbientHandle {
    task: tokio::task::JoinHandle<()>,
    stop_flag: Arc<AtomicBool>,
}

impl AmbientHandle {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

// ============================================================
// Start Ambient Perception
// ============================================================

/// Start the background ambient perception loop
pub fn start(app: AppHandle, event_tx: mpsc::Sender<AmbientEvent>) -> AmbientHandle {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = stop_flag.clone();

    let task = tokio::spawn(async move {
        ambient_loop(app, event_tx, flag_clone).await;
    });

    AmbientHandle { task, stop_flag }
}

// ============================================================
// Core Loop
// ============================================================

async fn ambient_loop(
    app: AppHandle,
    event_tx: mpsc::Sender<AmbientEvent>,
    stop_flag: Arc<AtomicBool>,
) {
    // Read HTTP LLM config: prefer a 'background' role provider, fall back to active realtime
    let llm_config = {
        let db_state = app.state::<DbState>();
        let pool = &db_state.0;

        // Try background provider first
        let bg_row: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, base_url, api_key, model, provider_type FROM llm_providers \
             WHERE is_active = 1 AND role = 'background' LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        // Fall back to any active provider
        let row = match bg_row {
            Some(r) => Some(r),
            None => sqlx::query_as(
                "SELECT id, base_url, api_key, model, provider_type FROM llm_providers \
                 WHERE is_active = 1 LIMIT 1",
            )
            .fetch_optional(pool)
            .await
            .ok()
            .flatten(),
        };

        row.map(|(id, base_url, raw_api_key, model, provider_type)| {
            let api_key = crate::keystore::resolve_api_key(&id, &raw_api_key);
            HttpLlmConfig {
                base_url: super::subagents::derive_chat_url(&base_url, &provider_type),
                api_key,
                model: super::subagents::derive_chat_model(&model),
            }
        })
    };

    let llm_config = match llm_config {
        Some(c) if !c.api_key.is_empty() => c,
        _ => {
            log::warn!("[Ambient] No active LLM provider — stopping");
            return;
        }
    };

    // Read configuration
    let interval_secs = {
        let db_state = app.state::<DbState>();
        let pool = &db_state.0;
        let result = sqlx::query_scalar::<_, String>(
            "SELECT value FROM app_settings WHERE key = 'ambient_interval_secs'",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30u64);
        result
    };

    let proactive_enabled = {
        let db_state = app.state::<DbState>();
        let pool = &db_state.0;
        let result = sqlx::query_scalar::<_, String>(
            "SELECT value FROM app_settings WHERE key = 'proactive_enabled'",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
        result
    };

    log::info!(
        "[Ambient] Started (interval={}s, proactive={})",
        interval_secs,
        proactive_enabled
    );

    let mut last_hash: u64 = 0;
    let mut same_screen_count: u32 = 0; // Consecutive similar screenshot count
    let stagnation_threshold = (300 / interval_secs).max(3) as u32; // 5 minutes

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // Skip the first immediate tick

    loop {
        interval.tick().await;

        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // Low-resolution screenshot
        let capture = match tokio::task::spawn_blocking(capture_thumbnail).await {
            Ok(Ok(c)) => c,
            _ => continue,
        };

        // Compute perceptual hash
        let current_hash = dhash(&capture.pixels, capture.width, capture.height);
        let distance = hamming_distance(last_hash, current_hash);

        if distance < 5 {
            // Screen barely changed
            same_screen_count += 1;

            if proactive_enabled && same_screen_count >= stagnation_threshold {
                log::info!(
                    "[Ambient] Stagnation detected ({} cycles, ~{}s)",
                    same_screen_count,
                    same_screen_count as u64 * interval_secs
                );

                // Send to LLM for screen state analysis
                let analysis = analyze_screen_for_help(&llm_config, &capture).await;
                if let Some(prompt) = analysis {
                    let _ = event_tx.send(AmbientEvent::ProactivePrompt(prompt)).await;
                }

                // Reset counter to avoid repeated prompts
                same_screen_count = 0;
            }
        } else {
            // Screen changed, reset counter
            same_screen_count = 0;
            last_hash = current_hash;
        }
    }

    log::info!("[Ambient] Stopped");
}

// ============================================================
// Screenshot + Hash
// ============================================================

struct Thumbnail {
    pixels: Vec<u8>, // Grayscale pixels (width × height)
    width: u32,
    height: u32,
    jpeg_base64: String, // Original JPEG, used for LLM analysis
}

/// Low-resolution screenshot (scaled to max 320px width)
fn capture_thumbnail() -> Result<Thumbnail, String> {
    let cap = system_api::capture_screen()?;

    // Decode base64 → image → resize
    let jpeg_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &cap.image_base64,
    )
    .map_err(|e| format!("base64 decode: {}", e))?;

    let img = image::load_from_memory_with_format(&jpeg_bytes, image::ImageFormat::Jpeg)
        .map_err(|e| format!("image decode: {}", e))?;

    let max_w = 320u32;
    let scale = max_w as f32 / cap.width as f32;
    let new_h = (cap.height as f32 * scale) as u32;

    let thumb = img.resize_exact(max_w, new_h, image::imageops::FilterType::Nearest);
    let gray = thumb.to_luma8();

    Ok(Thumbnail {
        pixels: gray.into_raw(),
        width: max_w,
        height: new_h,
        jpeg_base64: cap.image_base64,
    })
}

/// Difference hash (dHash) — 8x8 = 64-bit perceptual fingerprint
fn dhash(gray_pixels: &[u8], width: u32, height: u32) -> u64 {
    // Resize to 9x8 (dHash needs 9 columns to compare and produce 8 bits)
    let img = image::GrayImage::from_raw(width, height, gray_pixels.to_vec())
        .unwrap_or_else(|| image::GrayImage::new(9, 8));

    let small = image::imageops::resize(&img, 9, 8, image::imageops::FilterType::Nearest);

    let mut hash: u64 = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let left = small.get_pixel(x, y).0[0];
            let right = small.get_pixel(x + 1, y).0[0];
            if left > right {
                hash |= 1 << (y * 8 + x);
            }
        }
    }
    hash
}

/// Hamming distance (number of differing bits)
fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

// ============================================================
// LLM Analysis
// ============================================================

/// Send low-resolution screenshot to LLM to determine if user needs help
async fn analyze_screen_for_help(config: &HttpLlmConfig, thumb: &Thumbnail) -> Option<String> {
    use reqwest::Client;
    use serde_json::json;

    let client = Client::new();
    let effective_url = if config.base_url.is_empty() {
        "https://api.openai.com/v1/chat/completions".to_string()
    } else {
        config.base_url.clone()
    };
    let url = effective_url;

    let body = json!({
        "model": config.model,
        "max_tokens": 100,
        "messages": [{
            "role": "system",
            "content": "You observe a user's screen. In 1-2 sentences, decide if they seem stuck or need help. If they appear to be working normally, reply 'OK'. If they might need help (e.g., error dialog visible, same screen for a long time), suggest a brief offer."
        }, {
            "role": "user",
            "content": [
                {"type": "text", "text": "The user has been on this screen for over 5 minutes. Observe and assess:"},
                {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", thumb.jpeg_base64), "detail": "low"}}
            ]
        }],
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json_resp: serde_json::Value = resp.json().await.ok()?;
    let text = json_resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    // If LLM replies "OK" or similar, no help needed
    if text.is_empty() || text.to_uppercase().starts_with("OK") {
        None
    } else {
        Some(text)
    }
}
