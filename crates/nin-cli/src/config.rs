//! `%APPDATA%\nin\config.toml` (SPEC §9).
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub prompt: String,
    pub history_size: usize,
    pub color: bool,
    /// "auto" | "always" | "never"
    pub pause_on_exit: String,
    pub packages: Option<PathBuf>,
    pub plugins: Option<PathBuf>,
    pub registries: Vec<String>,
    pub max_parallel: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Config { prompt: "nin> ".into(), history_size: 500, color: true, pause_on_exit: "auto".into(), packages: None, plugins: None, registries: vec![], max_parallel: None }
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(a) = std::env::var("APPDATA") {
        return PathBuf::from(a).join("nin");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".config").join("nin");
    }
    PathBuf::from(".nin")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load() -> (Config, Option<String>) {
    let mut c = Config::default();
    let p = std::env::var("NIN_CONFIG").map(PathBuf::from).unwrap_or_else(|_| config_path());
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => return (c, None),
    };
    let v: toml::Value = match text.parse() {
        Ok(v) => v,
        Err(e) => return (c, Some(format!("{}: {}", p.display(), e))),
    };
    if let Some(s) = v.get("shell") {
        if let Some(x) = s.get("prompt").and_then(|x| x.as_str()) {
            c.prompt = x.to_string();
        }
        if let Some(x) = s.get("history_size").and_then(|x| x.as_integer()) {
            c.history_size = x.max(0) as usize;
        }
        if let Some(x) = s.get("color").and_then(|x| x.as_bool()) {
            c.color = x;
        }
    }
    if let Some(x) = v.get("run").and_then(|r| r.get("pause_on_exit")).and_then(|x| x.as_str()) {
        c.pause_on_exit = x.to_string();
    }
    if let Some(p) = v.get("paths") {
        c.packages = p.get("packages").and_then(|x| x.as_str()).map(PathBuf::from);
        c.plugins = p.get("plugins").and_then(|x| x.as_str()).map(PathBuf::from);
    }
    if let Some(a) = v.get("registry").and_then(|r| r.get("sources")).and_then(|x| x.as_array()) {
        c.registries = a.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(x) = v.get("workflow").and_then(|w| w.get("max_parallel")).and_then(|x| x.as_integer()) {
        c.max_parallel = Some(x.max(1) as usize);
    }
    (c, None)
}
