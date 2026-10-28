//! Tiny persistent key/value store (the webview build's localStorage), saved as JSON in %APPDATA%.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Settings {
    path: Option<PathBuf>,
    map: BTreeMap<String, String>,
}

impl Settings {
    pub fn load() -> Self {
        let path = std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("MDelight").join("native-settings.json"));
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Settings { path, map }
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.map.get(k).map(|s| s.as_str())
    }

    pub fn set(&mut self, k: &str, v: &str) {
        if self.map.get(k).map(|s| s.as_str()) == Some(v) {
            return;
        }
        self.map.insert(k.to_string(), v.to_string());
        self.save();
    }

    pub fn list(&self, k: &str) -> Vec<String> {
        self.get(k).and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default()
    }

    pub fn set_list(&mut self, k: &str, v: &[String]) {
        self.set(k, &serde_json::to_string(v).unwrap_or_default());
    }

    fn save(&self) {
        let Some(p) = &self.path else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.map) {
            let _ = std::fs::write(p, s);
        }
    }
}
