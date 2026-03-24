/// tray — System tray module
///
/// Displays the app icon in the taskbar/system tray area with a right-click menu:
/// - Settings: Opens the config panel (standalone decorated window)
/// - Check for Updates: Checks for OTA updates
/// - Quit: Exits the application
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

#[derive(Clone, serde::Serialize)]
struct StatusPayload {
    state: String,
    message: Option<String>,
}

fn emit_status(app: &tauri::AppHandle, state: &str, message: Option<String>) {
    if let Err(e) = app.emit(
        "agent-status",
        StatusPayload {
            state: state.to_string(),
            message,
        },
    ) {
        log::warn!("[Updater] Emit status failed: {}", e);
    }
}

fn notify_update(app: &tauri::AppHandle, body: &str) {
    if let Err(e) = app
        .notification()
        .builder()
        .title("Agent3 Updater")
        .body(body)
        .show()
    {
        log::warn!("[Updater] Native notification failed: {}", e);
    }
}

pub fn open_config_window(app: &AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("config") {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    tauri::WebviewWindowBuilder::new(app, "config", tauri::WebviewUrl::App("config.html".into()))
        .title("Agent3 Settings")
        .inner_size(960.0, 720.0)
        .resizable(true)
        .decorations(true)
        .center()
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let settings_item = MenuItem::with_id(
        app,
        "settings",
        crate::i18n::t("tray.settings"),
        true,
        None::<&str>,
    )?;
    let update_item = MenuItem::with_id(
        app,
        "check-update",
        crate::i18n::t("tray.check_update"),
        true,
        None::<&str>,
    )?;
    let quit_item =
        MenuItem::with_id(app, "quit", crate::i18n::t("tray.quit"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings_item, &update_item, &quit_item])?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().expect("no default icon"))
        .menu(&menu)
        .tooltip("Agent3")
        .on_menu_event(|app, event| {
            match event.id.as_ref() {
                "settings" => {
                    if let Err(e) = open_config_window(app) {
                        log::warn!("[Tray] Failed to open settings window: {}", e);
                    }
                }
                "check-update" => {
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        check_for_updates(app_handle).await;
                    });
                }
                "quit" => {
                    app.exit(0);
                }
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}

/// Check for OTA updates and install if available
async fn check_for_updates(app: tauri::AppHandle) {
    log::info!("[Updater] Checking for updates...");
    notify_update(&app, "Checking for updates...");
    emit_status(
        &app,
        "update-checking",
        Some("Checking for updates...".to_string()),
    );

    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => {
            log::error!("[Updater] Builder error: {}", e);
            notify_update(&app, &format!("Update initialization failed: {}", e));
            emit_status(
                &app,
                "update-error",
                Some(format!("Update initialization failed: {}", e)),
            );
            return;
        }
    };

    match updater.check().await {
        Ok(Some(update)) => {
            log::info!("[Updater] Update available: v{}", update.version);
            notify_update(
                &app,
                &format!("Found new version v{}. Downloading now...", update.version),
            );
            emit_status(
                &app,
                "update-available",
                Some(format!("Found new version v{}", update.version)),
            );

            match update.download_and_install(|_, _| {}, || {}).await {
                Ok(()) => {
                    log::info!("[Updater] Update installed, restarting...");
                    notify_update(&app, "Update installed. Restarting now...");
                    emit_status(
                        &app,
                        "update-installed",
                        Some("Update installed. Restarting now...".to_string()),
                    );
                    app.restart();
                }
                Err(e) => {
                    log::error!("[Updater] Download/install failed: {}", e);
                    notify_update(&app, &format!("Update download/install failed: {}", e));
                    emit_status(
                        &app,
                        "update-error",
                        Some(format!("Update download/install failed: {}", e)),
                    );
                }
            }
        }
        Ok(None) => {
            log::info!("[Updater] Already up to date");
            notify_update(&app, "You are already on the latest version");
            emit_status(
                &app,
                "update-none",
                Some("You are already on the latest version".to_string()),
            );
        }
        Err(e) => {
            log::error!("[Updater] Check failed: {}", e);
            notify_update(&app, &format!("Update check failed: {}", e));
            emit_status(
                &app,
                "update-error",
                Some(format!("Update check failed: {}", e)),
            );
        }
    }
}
