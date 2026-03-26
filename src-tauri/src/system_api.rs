/// system_api — System capability introspection and control module
///
/// Exposes atomic OS-level capabilities to the frontend/LLM without imposing high-level semantics.
/// System info queries (processes, disks, files, env vars) are handled uniformly via exec_shell where the LLM constructs commands autonomously.
use std::io::Cursor;

use base64::Engine;
use serde::Serialize;

const SHELL_TIMEOUT_SECS: u64 = 45;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRiskLevel {
    Safe,
    High,
}

fn detect_shell_risk(command: &str) -> ShellRiskLevel {
    let normalized = command.to_lowercase();
    let risky_patterns = [
        "rm -rf",
        "del /f /s /q",
        "format ",
        "shutdown",
        "reboot",
        "curl ",
        "wget ",
        "invoke-expression",
        "iex ",
        " | sh",
        " | powershell",
        " | pwsh",
    ];
    if risky_patterns.iter().any(|p| normalized.contains(p)) {
        ShellRiskLevel::High
    } else {
        ShellRiskLevel::Safe
    }
}

pub fn validate_shell_command(command: &str, allow_high_risk: bool) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("Shell command is empty".to_string());
    }
    if detect_shell_risk(command) == ShellRiskLevel::High && !allow_high_risk {
        return Err("This command is classified as high-risk and was blocked. Tell the user the specific command you want to run and ask for their explicit permission. They can enable high-risk commands in Settings.".to_string());
    }
    Ok(())
}

// ============================================================
// Tauri Commands
// ============================================================

/// Execute a shell command and return the output
#[tauri::command]
pub fn exec_shell(command: String) -> Result<String, String> {
    exec_shell_with_policy(command, false)
}

pub fn exec_shell_with_policy(command: String, allow_high_risk: bool) -> Result<String, String> {
    validate_shell_command(&command, allow_high_risk)?;

    #[cfg(target_os = "windows")]
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &command])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;

    #[cfg(not(target_os = "windows"))]
    let output = std::process::Command::new("sh")
        .args(["-c", &command])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!("Exit {}: {}", output.status, stderr))
    }
}

pub fn shell_timeout_secs() -> u64 {
    SHELL_TIMEOUT_SECS
}

/// Simulate keyboard input (typing)
#[tauri::command]
pub fn type_text(text: String) -> Result<(), String> {
    use enigo::{Enigo, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    enigo.text(&text).map_err(|e| e.to_string())
}

/// Simulate key press
#[tauri::command]
pub fn press_key(key: String) -> Result<(), String> {
    use enigo::{Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    let k = match key.to_lowercase().as_str() {
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "escape" | "esc" => Key::Escape,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "mediaplaypause" | "playpause" => Key::MediaPlayPause,
        "medianexttrack" | "nexttrack" => Key::MediaNextTrack,
        "mediaprevtrack" | "prevtrack" => Key::MediaPrevTrack,
        "volumeup" => Key::VolumeUp,
        "volumedown" => Key::VolumeDown,
        "volumemute" | "mute" => Key::VolumeMute,
        other => {
            if other.len() == 1 {
                Key::Unicode(other.chars().next().unwrap())
            } else {
                return Err(format!("Unknown key: {}", key));
            }
        }
    };
    enigo
        .key(k, enigo::Direction::Click)
        .map_err(|e| e.to_string())
}

/// Simulate mouse movement
#[tauri::command]
pub fn move_mouse(x: i32, y: i32) -> Result<(), String> {
    use enigo::{Coordinate, Enigo, Mouse, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    enigo
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| e.to_string())
}

/// Simulate mouse click
#[tauri::command]
pub fn click_mouse(button: String) -> Result<(), String> {
    use enigo::{Button, Enigo, Mouse, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    let btn = match button.to_lowercase().as_str() {
        "left" => Button::Left,
        "right" => Button::Right,
        "middle" => Button::Middle,
        _ => return Err(format!("Unknown button: {}", button)),
    };
    enigo
        .button(btn, enigo::Direction::Click)
        .map_err(|e| e.to_string())
}

// ============================================================
// Screen capture (visual perception)
// ============================================================

#[derive(Serialize, Clone)]
pub struct ScreenCapture {
    /// Base64-encoded JPEG image (without data URI prefix)
    pub image_base64: String,
    /// Screen width (pixels)
    pub width: u32,
    /// Screen height (pixels)
    pub height: u32,
}

/// Capture the primary screen, compress to JPEG, and return base64 + resolution
pub fn capture_screen() -> Result<ScreenCapture, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("Failed to list monitors: {}", e))?;
    let monitor = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| xcap::Monitor::all().ok().and_then(|m| m.into_iter().next()))
        .ok_or("No monitor found")?;

    let img = monitor
        .capture_image()
        .map_err(|e| format!("Screen capture failed: {}", e))?;

    let width = img.width();
    let height = img.height();

    // Convert to DynamicImage then encode as JPEG (quality 75) to reduce transfer size
    let dyn_img = image::DynamicImage::ImageRgba8(img);
    let mut buf = Cursor::new(Vec::new());
    dyn_img
        .write_to(&mut buf, image::ImageFormat::Jpeg)
        .map_err(|e| format!("JPEG encode failed: {}", e))?;

    let image_base64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());

    Ok(ScreenCapture {
        image_base64,
        width,
        height,
    })
}

#[tauri::command]
pub fn read_clipboard() -> Result<String, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.get_text().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_clipboard(text: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_running_apps() -> Result<Vec<String>, String> {
    use sysinfo::System;
    let mut sys = System::new_all();
    sys.refresh_processes();

    let mut processes: Vec<String> = sys
        .processes()
        .values()
        .map(|p| p.name().to_string())
        .collect();

    processes.sort();
    processes.dedup();

    Ok(processes)
}

#[tauri::command]
pub fn search_installed_software(keyword: Option<String>) -> Result<Vec<String>, String> {
    use std::process::Command;
    let keyword_lower = keyword.unwrap_or_default().to_lowercase();

    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args(&[
                "-NoProfile",
                "-Command",
                r#"Get-StartApps | Select-Object -Property Name | ConvertTo-Json -Compress"#,
            ])
            .output()
            .map_err(|e| e.to_string())?;

        if !output.status.success() {
            return Err("Failed to query installed apps".to_string());
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        if let Ok(apps) = serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
            let mut results: Vec<String> = apps
                .into_iter()
                .filter_map(|val| {
                    val.get("Name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .filter(|name| name.to_lowercase().contains(&keyword_lower))
                .collect();
            results.sort();
            results.dedup();
            return Ok(results);
        }
        return Ok(vec![]);
    }

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("mdfind")
            .args(&["kMDItemContentType", "==", "com.apple.application-bundle"])
            .output()
            .map_err(|e| e.to_string())?;

        let text = String::from_utf8_lossy(&output.stdout);
        let mut results: Vec<String> = text
            .lines()
            .filter_map(|line| {
                let path = std::path::Path::new(line);
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .filter(|name| name.to_lowercase().contains(&keyword_lower))
            .collect();
        results.sort();
        results.dedup();
        return Ok(results);
    }

    #[cfg(target_os = "linux")]
    {
        let mut results = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/usr/share/applications") {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".desktop") {
                        let app_name = name.trim_end_matches(".desktop").replace("-", " ");
                        if app_name.to_lowercase().contains(&keyword_lower) {
                            results.push(app_name);
                        }
                    }
                }
            }
        }
        results.sort();
        results.dedup();
        return Ok(results);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Ok(vec![])
    }
}
