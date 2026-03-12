use crate::models::AppData;
use std::fs;
use std::path::PathBuf;

const APP_DIR: &str = "proxy-manager";
const CONFIG_FILE: &str = "config.json";

/// Return the platform-specific config file path.
/// Linux:   ~/.config/proxy-manager/config.json
/// macOS:   ~/Library/Application Support/proxy-manager/config.json
/// Windows: %APPDATA%\proxy-manager\config.json
fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_DIR).join(CONFIG_FILE))
}

/// Load `AppData` from disk. Returns `AppData::default()` on any error.
/// Supports legacy profile-based config format via `parse_config()`.
pub fn load() -> AppData {
    let Some(path) = config_path() else {
        return AppData::default();
    };
    match fs::read_to_string(&path) {
        Ok(json) => crate::models::parse_config(&json).unwrap_or_else(|e| {
            eprintln!("Failed to parse config: {e}");
            AppData::default()
        }),
        Err(_) => AppData::default(),
    }
}

/// Persist `AppData` to disk. Returns an error string on failure.
pub fn save(data: &AppData) -> Result<(), String> {
    let path = config_path().ok_or("Could not determine config directory")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create config dir: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(data).map_err(|e| format!("Serialization error: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write config: {e}"))?;
    Ok(())
}
