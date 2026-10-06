use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Instance {
    pub name: String,
    pub mc_version: String,
    pub loader: String, // "vanilla" | "fabric" | "quilt" | "forge" | "neoforge"
    pub loader_version: Option<String>,
    pub java_path: Option<String>,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub game_args: Vec<String>,
    pub ram_min: Option<String>,
    pub ram_max: Option<String>,
    pub created_at: String,
    pub last_played: Option<String>,
    #[serde(default)]
    pub playtime_seconds: u64,
    /// Icon URL (set for instances installed from a Modrinth modpack).
    pub icon: Option<String>,
    pub notes: Option<String>,
    /// Modrinth project id of the modpack this instance came from.
    #[serde(default)]
    pub modrinth_project: Option<String>,
    /// Exit code of the most recent game session.
    #[serde(default)]
    pub last_exit: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstalledMod {
    pub filename: String,
    pub enabled: bool,
    pub size_bytes: u64,
    pub display_name: String,
}

pub fn base_instances_dir() -> PathBuf {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(v).join("mirage/instances")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".local/share/mirage/instances")
    }
}

impl Instance {
    pub fn new(name: &str, mc_version: &str, loader: &str) -> Self {
        Self {
            name: name.to_string(),
            mc_version: mc_version.to_string(),
            loader: loader.to_lowercase(),
            loader_version: None,
            java_path: None,
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            ram_min: None,
            ram_max: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_played: None,
            playtime_seconds: 0,
            icon: None,
            notes: None,
            modrinth_project: None,
            last_exit: None,
        }
    }

    /// Log file launches append `<seconds> <exit code>` lines to. It is the
    /// one hand-off point for both foreground launches and the new-terminal
    /// shell script; `load` folds it into the metadata.
    pub fn sessions_path(&self) -> PathBuf {
        self.dir().join(".sessions")
    }

    pub fn record_session(&self, seconds: u64, exit_code: i32) -> Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.sessions_path())?;
        writeln!(f, "{seconds} {exit_code}")?;
        Ok(())
    }

    /// Add logged sessions to playtime and the last exit code, then clear
    /// the log. Malformed lines are skipped.
    fn fold_sessions(&mut self) {
        let path = self.sessions_path();
        let Ok(log) = std::fs::read_to_string(&path) else { return };
        for line in log.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(Ok(secs)), Some(Ok(code))) =
                (parts.next().map(str::parse::<u64>), parts.next().map(str::parse::<i32>))
            {
                self.playtime_seconds += secs;
                self.last_exit = Some(code);
            }
        }
        if self.save().is_ok() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub fn dir(&self) -> PathBuf {
        base_instances_dir().join(&self.name)
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.dir().join("instance.json")
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.dir().join("mods")
    }

    pub fn saves_dir(&self) -> PathBuf {
        self.dir().join("saves")
    }

    pub fn resourcepacks_dir(&self) -> PathBuf {
        self.dir().join("resourcepacks")
    }

    pub fn save(&self) -> Result<()> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create instance directory: {}", dir.display()))?;
        std::fs::create_dir_all(self.mods_dir())?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(self.metadata_path(), json)?;
        Ok(())
    }

    pub fn load(name: &str) -> Result<Self> {
        let dir = base_instances_dir().join(name);
        let meta_file = dir.join("instance.json");
        if meta_file.exists() {
            let data = std::fs::read_to_string(&meta_file)?;
            if let Ok(mut inst) = serde_json::from_str::<Instance>(&data) {
                inst.name = name.to_string(); // ensure sync
                inst.fold_sessions();
                return Ok(inst);
            }
        }

        // Fallback for folders without instance.json
        if dir.exists() {
            let default_inst = Instance::new(name, "latest", "vanilla");
            let _ = default_inst.save();
            return Ok(default_inst);
        }

        bail!("Instance '{}' does not exist", name);
    }

    pub fn list_all() -> Vec<Instance> {
        let base = base_instances_dir();
        std::fs::create_dir_all(&base).ok();

        let mut list = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Ok(inst) = Self::load(&name) {
                        list.push(inst);
                    }
                }
            }
        }

        list.sort_by_key(|i| i.name.to_lowercase());
        list
    }

    pub fn create(
        name: &str,
        mc_version: &str,
        loader: &str,
        loader_ver: Option<&str>,
    ) -> Result<Instance> {
        let clean_name = name.trim();
        if clean_name.is_empty() {
            bail!("Instance name cannot be empty");
        }
        if clean_name.contains('/') || clean_name.contains('\\') || clean_name.contains("..") {
            bail!("Invalid instance name: cannot contain path separators");
        }

        let dir = base_instances_dir().join(clean_name);
        if dir.exists() && dir.join("instance.json").exists() {
            bail!("Instance '{}' already exists", clean_name);
        }

        let mut inst = Instance::new(clean_name, mc_version, loader);
        inst.loader_version = loader_ver.map(|s| s.to_string());
        inst.save()?;
        Ok(inst)
    }

    pub fn delete(&self) -> Result<()> {
        let dir = self.dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("Failed to delete instance dir: {}", dir.display()))?;
        }
        Ok(())
    }

    pub fn duplicate(&self, new_name: &str) -> Result<Instance> {
        let dest = base_instances_dir().join(new_name);
        if dest.exists() {
            bail!("Destination instance '{}' already exists", new_name);
        }

        copy_dir_recursive(&self.dir(), &dest)?;

        let mut new_inst = self.clone();
        new_inst.name = new_name.to_string();
        new_inst.created_at = chrono::Utc::now().to_rfc3339();
        new_inst.last_played = None;
        new_inst.playtime_seconds = 0;
        new_inst.last_exit = None;
        let _ = std::fs::remove_file(new_inst.sessions_path());
        new_inst.save()?;

        Ok(new_inst)
    }

    pub fn effective_version_id(&self) -> String {
        match self.loader.to_lowercase().as_str() {
            "fabric" => format!("fabric-{}", self.mc_version),
            "quilt" => format!("quilt-{}", self.mc_version),
            _ => self.mc_version.clone(),
        }
    }

    pub fn installed_mods(&self) -> Vec<InstalledMod> {
        let mods_dir = self.mods_dir();
        if !mods_dir.exists() {
            return Vec::new();
        }

        let mut mods = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&mods_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let filename = entry.file_name().to_string_lossy().to_string();
                    let enabled = filename.ends_with(".jar");
                    let is_mod = enabled || filename.ends_with(".jar.disabled");

                    if is_mod {
                        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        let display_name = filename
                            .trim_end_matches(".disabled")
                            .trim_end_matches(".jar")
                            .to_string();

                        mods.push(InstalledMod {
                            filename,
                            enabled,
                            size_bytes,
                            display_name,
                        });
                    }
                }
            }
        }
        mods.sort_by_key(|m| m.display_name.to_lowercase());
        mods
    }

    pub fn toggle_mod(&self, filename: &str) -> Result<bool> {
        let mods_dir = self.mods_dir();
        let src = mods_dir.join(filename);
        if !src.exists() {
            bail!("Mod file '{}' not found", filename);
        }

        let is_currently_enabled = filename.ends_with(".jar");
        let dest = if is_currently_enabled {
            mods_dir.join(format!("{}.disabled", filename))
        } else if let Some(base) = filename.strip_suffix(".disabled") {
            mods_dir.join(base)
        } else {
            bail!("Unsupported mod filename format");
        };

        std::fs::rename(&src, &dest)?;
        Ok(!is_currently_enabled)
    }

    pub fn remove_mod(&self, filename: &str) -> Result<()> {
        let path = self.mods_dir().join(filename);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_effective_version() {
        let inst_vanilla = Instance::new("test_v", "1.21.1", "vanilla");
        assert_eq!(inst_vanilla.effective_version_id(), "1.21.1");

        let inst_fabric = Instance::new("test_f", "1.21.1", "fabric");
        assert_eq!(inst_fabric.effective_version_id(), "fabric-1.21.1");
    }

    #[test]
    fn test_installed_mod_naming() {
        let filename = "sodium-fabric-0.5.8.jar";
        let display = filename.trim_end_matches(".jar");
        assert_eq!(display, "sodium-fabric-0.5.8");
    }
}
