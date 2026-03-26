/// keystore — Platform keyring abstraction for API key storage
///
/// Wraps the `keyring` crate to store and retrieve API keys from the OS secret store.
/// Falls back gracefully on headless systems where no keyring is available.

const SERVICE_NAME: &str = "agent3";

/// Sentinel value stored in DB when key is in the platform keyring.
pub const KEYRING_SENTINEL: &str = "__keyring__";

/// Store an API key in the platform keyring.
pub fn store_key(provider_id: &str, key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, provider_id)
        .map_err(|e| format!("keyring entry error: {e}"))?;
    entry
        .set_password(key)
        .map_err(|e| format!("keyring store error: {e}"))
}

/// Retrieve an API key from the platform keyring.
pub fn get_key(provider_id: &str) -> Result<String, String> {
    let entry = keyring::Entry::new(SERVICE_NAME, provider_id)
        .map_err(|e| format!("keyring entry error: {e}"))?;
    entry
        .get_password()
        .map_err(|e| format!("keyring get error: {e}"))
}

/// Delete an API key from the platform keyring (used when provider is removed).
pub fn delete_key(provider_id: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, provider_id)
        .map_err(|e| format!("keyring entry error: {e}"))?;
    entry
        .delete_credential()
        .map_err(|e| format!("keyring delete error: {e}"))
}

/// Resolve an API key: if the DB value is the keyring sentinel, fetch from keyring.
/// Otherwise return the plaintext value as-is (backward compat / headless fallback).
pub fn resolve_api_key(provider_id: &str, db_value: &str) -> String {
    if db_value == KEYRING_SENTINEL {
        match get_key(provider_id) {
            Ok(key) => key,
            Err(e) => {
                log::error!(
                    "[Keystore] Failed to retrieve key for '{}': {}",
                    provider_id,
                    e
                );
                String::new()
            }
        }
    } else {
        db_value.to_string()
    }
}

/// Migrate a single provider's API key from plaintext DB to keyring.
/// Returns true if migration succeeded and DB should be updated to sentinel.
pub fn migrate_to_keyring(provider_id: &str, plaintext_key: &str) -> bool {
    if plaintext_key.is_empty() || plaintext_key == KEYRING_SENTINEL {
        return false;
    }
    match store_key(provider_id, plaintext_key) {
        Ok(()) => {
            log::info!(
                "[Keystore] Migrated API key for '{}' to platform keyring",
                provider_id
            );
            true
        }
        Err(e) => {
            log::warn!(
                "[Keystore] Cannot migrate key for '{}' to keyring ({}), keeping plaintext",
                provider_id,
                e
            );
            false
        }
    }
}
