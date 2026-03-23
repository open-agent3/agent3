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
        return Err("High-risk shell command blocked by policy. Ask user to explicitly allow and set app setting 'allow_high_risk_shell' to 'true'.".to_string());
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
