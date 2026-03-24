/// tray — System tray module
///
/// Displays the app icon in the taskbar/system tray area with a right-click menu:
/// - Settings: Opens the config panel (standalone decorated window)
/// - Check for Updates: Checks for OTA updates
/// - Quit: Exits the application
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager,
};
use tauri_plugin_updater::UpdaterExt;

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
    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => {
            log::error!("[Updater] Builder error: {}", e);
            return;
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            log::info!("[Updater] Update available: v{}", update.version);
            match update.download_and_install(|_, _| {}, || {}).await {
                Ok(()) => {
                    log::info!("[Updater] Update installed, restarting...");
                    app.restart();
                }
                Err(e) => {
                    log::error!("[Updater] Download/install failed: {}", e);
                }
            }
        }
        Ok(None) => {
            log::info!("[Updater] Already up to date");
        }
        Err(e) => {
            log::error!("[Updater] Check failed: {}", e);
        }
    }
}
