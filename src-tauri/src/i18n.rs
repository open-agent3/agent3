/// i18n — Lightweight internationalization module
///
/// Embeds JSON translation dictionaries at compile time via `include_str!`.
/// Global locale stored in a RwLock, accessible from any thread.
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

static EN: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../locales/en.json")).unwrap_or_default());

static ZH: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../locales/zh.json")).unwrap_or_default());

static LOCALE: RwLock<String> = RwLock::new(String::new());

/// Set the global locale ("en" or "zh")
pub fn set_locale(lang: &str) {
    let lang = match lang {
        "zh" => "zh",
        _ => "en",
    };
    if let Ok(mut l) = LOCALE.write() {
        *l = lang.to_string();
    }
}

/// Get the current global locale
pub fn get_locale() -> String {
    LOCALE
        .read()
        .map(|l| {
            if l.is_empty() {
                "en".to_string()
            } else {
                l.clone()
            }
        })
        .unwrap_or_else(|_| "en".to_string())
}

/// Get the language name for LLM prompts (e.g. "Chinese (Simplified)" or "English")
pub fn language_name() -> &'static str {
    if get_locale() == "zh" {
        "Chinese (Simplified)"
    } else {
        "English"
    }
}

/// Translate a key using the current locale.
/// Returns the key itself if not found (makes missing translations visible in logs).
pub fn t(key: &str) -> String {
    let dict = if get_locale() == "zh" { &*ZH } else { &*EN };
    dict.get(key).cloned().unwrap_or_else(|| key.to_string())
}
