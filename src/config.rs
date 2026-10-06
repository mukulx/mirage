use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_instance_name")]
    pub default_instance: String,
    pub default_java: Option<String>,
    #[serde(default = "default_ram_min")]
    pub ram_min: String,
    #[serde(default = "default_ram_max")]
    pub ram_max: String,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub game_args: Vec<String>,
    pub active_account: Option<String>,
    #[serde(default = "default_parallel_downloads")]
    pub parallel_downloads: usize,
    /// Open games in their own terminal window. Off by default: the TUI runs
    /// the game itself and streams its log into the instance page.
    #[serde(default)]
    pub launch_new_terminal: bool,
}

fn default_instance_name() -> String {
    "default".to_string()
}
fn default_ram_min() -> String {
    "2G".to_string()
}
fn default_ram_max() -> String {
    "4G".to_string()
}
fn default_parallel_downloads() -> usize {
    32
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_instance: default_instance_name(),
            default_java: None,
            ram_min: default_ram_min(),
            ram_max: default_ram_max(),
            jvm_args: vec![
                "-XX:+UnlockExperimentalVMOptions".into(),
                "-XX:+UseG1GC".into(),
                "-XX:G1NewSizePercent=20".into(),
                "-XX:G1ReservePercent=20".into(),
                "-XX:MaxGCPauseMillis=50".into(),
                "-XX:G1HeapRegionSize=32M".into(),
            ],
            game_args: Vec::new(),
            active_account: None,
            parallel_downloads: 32,
            launch_new_terminal: false,
        }
    }
}

pub fn config_path() -> PathBuf {
    let dir = if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(v).join("mirage")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".config/mirage")
    };
    std::fs::create_dir_all(&dir).ok();
    dir.join("config.json")
}

pub fn load() -> Config {
    let p = config_path();
    if let Ok(data) = std::fs::read_to_string(&p) {
        if let Ok(cfg) = serde_json::from_str(&data) {
            return cfg;
        }
    }
    let default = Config::default();
    let _ = save(&default);
    default
}

pub fn save(config: &Config) -> Result<()> {
    let data = serde_json::to_string_pretty(config)?;
    std::fs::write(config_path(), data)?;
    Ok(())
}

pub fn set_default_instance(name: &str) -> Result<()> {
    let mut config = load();
    config.default_instance = name.to_string();
    save(&config)
}

pub fn set_ram(min: &str, max: &str) -> Result<()> {
    let mut config = load();
    config.ram_min = min.to_string();
    config.ram_max = max.to_string();
    save(&config)
}

pub fn set_java(path: Option<String>) -> Result<()> {
    let mut config = load();
    config.default_java = path;
    save(&config)
}

pub fn set_new_terminal(on: bool) -> Result<()> {
    let mut config = load();
    config.launch_new_terminal = on;
    save(&config)
}

pub fn add_jvm_arg(arg: &str) -> Result<()> {
    let mut config = load();
    if !config.jvm_args.iter().any(|a| a == arg) {
        config.jvm_args.push(arg.to_string());
        save(&config)?;
    }
    Ok(())
}

pub fn add_game_arg(arg: &str) -> Result<()> {
    let mut config = load();
    if !config.game_args.iter().any(|a| a == arg) {
        config.game_args.push(arg.to_string());
        save(&config)?;
    }
    Ok(())
}

pub fn remove_jvm_arg(idx: usize) -> Result<()> {
    let mut config = load();
    if idx < config.jvm_args.len() {
        config.jvm_args.remove(idx);
        save(&config)
    } else {
        bail!(
            "Index {} out of range (0-{})",
            idx,
            config.jvm_args.len().saturating_sub(1)
        );
    }
}

pub fn remove_game_arg(idx: usize) -> Result<()> {
    let mut config = load();
    if idx < config.game_args.len() {
        config.game_args.remove(idx);
        save(&config)
    } else {
        bail!(
            "Index {} out of range (0-{})",
            idx,
            config.game_args.len().saturating_sub(1)
        );
    }
}

pub fn list() {
    let config = load();
    use crate::term::Term;
    Term::header("Configuration");
    Term::label("Default Instance", &config.default_instance);
    Term::label(
        "Java Path",
        config.default_java.as_deref().unwrap_or("Auto-detect"),
    );
    Term::label("RAM Allocation", &format!("{} - {}", config.ram_min, config.ram_max));
    Term::label(
        "Parallel Downloads",
        &config.parallel_downloads.to_string(),
    );
    Term::label(
        "New Terminal on Launch",
        if config.launch_new_terminal { "on" } else { "off (default)" },
    );

    println!("\n  JVM Arguments ({}):", config.jvm_args.len());
    for (i, arg) in config.jvm_args.iter().enumerate() {
        println!("    [{}] {}", i, arg);
    }

    println!("\n  Game Arguments ({}):", config.game_args.len());
    for (i, arg) in config.game_args.iter().enumerate() {
        println!("    [{}] {}", i, arg);
    }

    println!("\n  Config file: {}", config_path().display());
}
