/// wakeword — Wake word model management
///
/// Provides Tauri commands: record samples, train models, list/delete models.
/// Model files are stored in the app_data_dir/wakewords/ directory.
use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;

use rustpotter::Wakeword;
use serde::Serialize;
use tauri::{AppHandle, Manager};

// ============================================================
// Type definitions
// ============================================================

#[derive(Debug, Serialize, Clone)]
pub struct WakewordInfo {
    pub name: String,
    pub path: String,
}

// ============================================================
// Helper functions
// ============================================================

/// Get the wakeword model storage directory
fn wakewords_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.appdata_failed")))?
        .join("wakewords");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.mkdir_failed")))?;
    }
    Ok(dir)
}

/// PCM i16 samples → WAV bytes (hound encoding)
fn pcm_to_wav(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>, String> {
    let mut cursor = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(&mut cursor, spec)
        .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.wav_writer_failed")))?;
    for &s in samples {
        writer
            .write_sample(s)
            .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.wav_write_failed")))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.wav_finalize_failed")))?;
    Ok(cursor.into_inner())
}

// ============================================================
// Tauri commands
// ============================================================

/// Start recording wakeword samples (notify audio module to start buffering)
#[tauri::command]
pub fn wakeword_start_record(state: tauri::State<'_, super::AgentState>) -> Result<(), String> {
    let guard = state.audio.lock().map_err(|e| e.to_string())?;
    let handle = guard
        .as_ref()
        .ok_or_else(|| crate::i18n::t("audio.no_module"))?;
    handle.start_recording();
    log::info!("[Wakeword] Recording started");
    Ok(())
}

/// Stop recording and return recording duration (seconds)
#[tauri::command]
pub fn wakeword_stop_record(state: tauri::State<'_, super::AgentState>) -> Result<f32, String> {
    let guard = state.audio.lock().map_err(|e| e.to_string())?;
    let handle = guard
        .as_ref()
        .ok_or_else(|| crate::i18n::t("audio.no_module"))?;
    let samples = handle.stop_recording();
    let duration = samples.len() as f32 / handle.capture_rate as f32;
    log::info!(
        "[Wakeword] Recording stopped: {} samples ({:.1}s)",
        samples.len(),
        duration
    );

    // Temporarily store the recorded PCM in WakewordRecordState
    if let Ok(mut buf) = handle.record_buffer.lock() {
        // record_buffer was cleared by stop_recording; store samples back as temporary buffer
        // A separate mechanism would be better, but reusing record_buffer for simplicity
        *buf = samples;
    }
    Ok(duration)
}

/// Save the most recently recorded samples to a temporary file
#[tauri::command]
pub fn wakeword_save_sample(
    app: AppHandle,
    index: usize,
    state: tauri::State<'_, super::AgentState>,
) -> Result<(), String> {
    let guard = state.audio.lock().map_err(|e| e.to_string())?;
    let handle = guard
        .as_ref()
        .ok_or_else(|| crate::i18n::t("audio.no_module"))?;
    let samples = {
        let buf = handle.record_buffer.lock().map_err(|e| e.to_string())?;
        buf.clone()
    };
    if samples.is_empty() {
        return Err(crate::i18n::t("wakeword.no_recording"));
    }

    let wav_bytes = pcm_to_wav(&samples, handle.capture_rate)?;
    let dir = wakewords_dir(&app)?;
    let path = dir.join(format!("_sample_{index}.wav"));
    std::fs::write(&path, wav_bytes)
        .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.save_sample_failed")))?;
    log::info!(
        "[Wakeword] Sample {} saved ({} samples)",
        index,
        samples.len()
    );
    Ok(())
}

/// Train wakeword model (from saved sample files)
#[tauri::command]
pub fn wakeword_train(app: AppHandle, name: String) -> Result<String, String> {
    let dir = wakewords_dir(&app)?;

    // Collect temporarily stored WAV sample files
    let mut wav_samples: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..10 {
        let path = dir.join(format!("_sample_{i}.wav"));
        if path.exists() {
            let data = std::fs::read(&path).map_err(|e| {
                format!("{} {i}: {e}", crate::i18n::t("wakeword.read_sample_failed"))
            })?;
            wav_samples.insert(format!("sample_{i}"), data);
        }
    }

    if wav_samples.len() < 3 {
        return Err(format!(
            "{} {}",
            crate::i18n::t("wakeword.min_samples"),
            wav_samples.len()
        ));
    }

    log::info!(
        "[Wakeword] Training model '{}' with {} samples",
        name,
        wav_samples.len()
    );

    // Build using Wakeword DTW method (no neural network training required)
    let wakeword = Wakeword::new_from_sample_buffers(
        name.clone(),
        None, // threshold: use default
        None, // avg_threshold: use default
        wav_samples,
    )
    .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.train_failed")))?;

    // Save model
    let model_path = dir.join(format!("{name}.rpw"));
    let model_path_str = model_path.to_string_lossy().to_string();
    wakeword
        .save_to_file(&model_path_str)
        .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.save_model_failed")))?;

    // Clean up temporary sample files
    for i in 0..10 {
        let path = dir.join(format!("_sample_{i}.wav"));
        let _ = std::fs::remove_file(path);
    }

    log::info!("[Wakeword] Model saved: {}", model_path_str);
    Ok(model_path_str)
}

/// List all trained wakeword models
#[tauri::command]
pub fn wakeword_list(app: AppHandle) -> Result<Vec<WakewordInfo>, String> {
    let dir = wakewords_dir(&app)?;
    let mut models = Vec::new();

    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.read_dir_failed")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("rpw") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                models.push(WakewordInfo {
                    name: stem.to_string(),
                    path: path.to_string_lossy().to_string(),
                });
            }
        }
    }

    Ok(models)
}

/// Delete wakeword model
#[tauri::command]
pub fn wakeword_delete(app: AppHandle, name: String) -> Result<(), String> {
    let dir = wakewords_dir(&app)?;
    let path = dir.join(format!("{name}.rpw"));
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("{}: {e}", crate::i18n::t("wakeword.delete_failed")))?;
        log::info!("[Wakeword] Model deleted: {}", name);
    }
    Ok(())
}

/// Set the active wakeword model (write to app_settings)
#[tauri::command]
pub async fn wakeword_set_active(
    app: AppHandle,
    model_path: String,
    enabled: bool,
) -> Result<(), String> {
    let db_state = app.state::<crate::db::DbState>();
    let pool = &db_state.0;

    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES ('wake_word_model_path', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(&model_path)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    let enabled_str = if enabled { "true" } else { "false" };
    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES ('wake_word_enabled', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(enabled_str)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    log::info!(
        "[Wakeword] Active model set: {} (enabled={})",
        model_path,
        enabled
    );
    Ok(())
}
