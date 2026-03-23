mod agent;
mod db;
mod i18n;
mod system_api;
mod tray;

// Mutex removed - using sqlx::SqlitePool instead
use tauri::{menu::Menu, Emitter, Listener, Manager};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

#[tauri::command]
fn open_settings_window(app: tauri::AppHandle, focus_wakeword: Option<bool>) -> Result<(), String> {
    tray::open_config_window(&app)?;
    if focus_wakeword.unwrap_or(false) {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let _ = app_clone.emit("config-focus-wakeword", ());
        });
    }
    Ok(())
}

/// Forward frontend console logs to the terminal (dev mode visibility)
#[tauri::command]
fn log_from_frontend(level: String, message: String) {
    match level.as_str() {
        "error" => eprintln!("[frontend] ERROR {}", message),
        "warn" => eprintln!("[frontend] WARN {}", message),
        _ => println!("[frontend] {}", message),
    }
}

/// Get the current language setting
#[tauri::command]
fn get_language() -> String {
    i18n::get_locale()
}

#[tauri::command]
fn autostart_is_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn autostart_set_enabled(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let autostart = app.autolaunch();
    if enabled {
        autostart.enable().map_err(|e| e.to_string())
    } else {
        autostart.disable().map_err(|e| e.to_string())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("agent3.log".into()),
                    }),
                ])
                .max_file_size(2 * 1024 * 1024) // 2MB max per file
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne) // Keep current + 1 rotated file
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second instance launched - bring existing config window to front
            log::info!("[SingleInstance] Second instance blocked, focusing existing window");
            if let Some(win) = app.get_webview_window("config") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        let wake_shortcut =
                            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
                        if shortcut == &wake_shortcut {
                            log::info!("[GlobalShortcut] Ctrl+Alt+Space pressed - waking agent");
                            agent::wake_agent(app);
                        }
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--autostarted"]),
        ))
        .invoke_handler(tauri::generate_handler![
            log_from_frontend,
            get_language,
            open_settings_window,
            autostart_is_enabled,
            autostart_set_enabled,
            // --- System API ---
            system_api::exec_shell,
            system_api::type_text,
            system_api::press_key,
            system_api::move_mouse,
            system_api::click_mouse,
            // --- Database ---
            db::get_providers,
            db::save_provider,
            db::delete_provider,
            db::set_active_provider,
            db::get_active_provider,
            db::get_setting,
            db::set_setting,
            // --- Agent ---
            agent::agent_start,
            agent::agent_stop,
            agent::agent_restart,
            agent::agent_switch_voice,
            agent::get_board_content,
            agent::check_config_ready,
            // --- Wakeword ---
            agent::wakeword::wakeword_start_record,
            agent::wakeword::wakeword_stop_record,
            agent::wakeword::wakeword_save_sample,
            agent::wakeword::wakeword_train,
            agent::wakeword::wakeword_list,
            agent::wakeword::wakeword_delete,
            agent::wakeword::wakeword_set_active,
        ])
        .menu(|handle| Menu::new(handle))
        .setup(|app| {
            // Initialize SQLite database
            let app_handle = app.handle().clone();
            let pool = tauri::async_runtime::block_on(db::init_db(&app_handle)).map_err(|e| {
                tauri::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })?;
            app.manage(db::DbState(pool));

            // Detect and set locale
            {
                let db = app.state::<db::DbState>();
                let lang = tauri::async_runtime::block_on(async {
                    let saved: Option<String> =
                        sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'language'")
                            .fetch_optional(&db.0)
                            .await
                            .ok()
                            .flatten();

                    if let Some(lang) = saved {
                        lang
                    } else {
                        let detected = sys_locale::get_locale()
                            .map(|l| {
                                if l.starts_with("zh") {
                                    "zh".to_string()
                                } else {
                                    "en".to_string()
                                }
                            })
                            .unwrap_or_else(|| "en".to_string());
                        let _ = sqlx::query(
                            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?, ?)",
                        )
                        .bind("language")
                        .bind(&detected)
                        .execute(&db.0)
                        .await;
                        detected
                    }
                });
                i18n::set_locale(&lang);
                log::info!("[i18n] Locale set to: {}", i18n::get_locale());
            }

            // Initialize agent state
            app.manage(agent::AgentState::default());
            app.manage(agent::BoardState::default());

            // Listen for config changes - restart agent + notify frontend
            let app_handle2 = app.handle().clone();
            app.listen("config-changed", move |_| {
                let app_h = app_handle2.clone();
                tauri::async_runtime::spawn(async move {
                    // Re-read locale in case user changed language
                    {
                        let db = app_h.state::<db::DbState>();
                        let pool = &db.0;
                        let lang: Option<String> = sqlx::query_scalar(
                            "SELECT value FROM app_settings WHERE key = 'language'",
                        )
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten();

                        if let Some(lang) = lang {
                            i18n::set_locale(&lang);
                        }
                    }
                    let state = app_h.state::<agent::AgentState>();
                    agent::stop_existing(&state).await;
                    match agent::start_all(app_h.clone()).await {
                        Ok(()) => {
                            log::info!("[Agent] Restarted after config change");
                            let _ = app_h.emit("config-ready", ());
                        }
                        Err(e) => {
                            log::error!("[Agent] Restart after config change failed: {}", e);
                        }
                    }
                });
            });

            // Auto-start: only if config is ready, otherwise open Settings window
            let has_provider = {
                let db = app.state::<db::DbState>();
                tauri::async_runtime::block_on(async {
                    sqlx::query_scalar::<_, i64>(
                        "SELECT COUNT(*) FROM llm_providers WHERE is_active = 1 AND api_key != ''",
                    )
                    .fetch_one(&db.0)
                    .await
                    .map(|count| count > 0)
                    .unwrap_or(false)
                })
            };

            if has_provider {
                // Frontend AgentBridge.start() calls agent_start, do not start again here
                log::info!("[Agent] Provider configured - waiting for frontend to start");
            } else {
                log::info!("[Agent] No provider configured - opening Settings");
                // Open config window for first-time setup
                let _ = tauri::WebviewWindowBuilder::new(
                    app,
                    "config",
                    tauri::WebviewUrl::App("config.html".into()),
                )
                .title("Agent3 Settings")
                .inner_size(960.0, 720.0)
                .resizable(true)
                .decorations(true)
                .transparent(false)
                .center()
                .build();
            }

            let window = app.get_webview_window("main").unwrap();

            // Position main window to cover the whole screen, but click-through
            if let Ok(Some(monitor)) = window.current_monitor() {
                let screen = monitor.size();
                let scale = monitor.scale_factor();
                let width = screen.width as f64 / scale;
                // Subtracting 1 pixel from height prevents Windows from treating it as
                // a "fullscreen exclusive" app, which would otherwise hide the taskbar.
                let height = (screen.height as f64 / scale) - 1.0;

                let _ = window.set_size(tauri::LogicalSize::new(width, height));
                let _ = window.set_position(tauri::LogicalPosition::new(0.0, 0.0));
            }

            // Click-through
            window.set_ignore_cursor_events(true)?;

            // Show window after ready to avoid white flash
            let _ = window.show();

            // Sync autostart state with saved setting (default: enabled)
            {
                let autostart_mgr = app.autolaunch();
                let db_state = app.state::<db::DbState>();
                let should_enable = tauri::async_runtime::block_on(async {
                    sqlx::query_scalar::<_, String>(
                        "SELECT value FROM app_settings WHERE key = 'autostart_enabled'",
                    )
                    .fetch_optional(&db_state.0)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "true".to_string())
                        == "true"
                });
                let currently_enabled = autostart_mgr.is_enabled().unwrap_or(false);
                if should_enable && !currently_enabled {
                    let _ = autostart_mgr.enable();
                } else if !should_enable && currently_enabled {
                    let _ = autostart_mgr.disable();
                }
                log::info!(
                    "[Autostart] desired={}, current={}",
                    should_enable,
                    currently_enabled
                );
            }

            // System tray
            tray::setup_tray(app)?;

            // Register global shortcut: Ctrl+Alt+Space to wake agent
            {
                let shortcut =
                    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
                app.global_shortcut().register(shortcut)?;
                log::info!("[GlobalShortcut] Registered Ctrl+Alt+Space");
            }

            // macOS: hide Dock icon
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
