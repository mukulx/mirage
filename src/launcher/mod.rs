use crate::auth::MinecraftAccount;
use crate::download::Downloader;
use crate::instance::Instance;
use crate::term::Term;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Deserialize, Clone, Debug)]
pub struct VersionManifest {
    pub latest: LatestVersion,
    pub versions: Vec<VersionEntry>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct LatestVersion {
    pub release: String,
    pub snapshot: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub release_type: String,
    pub url: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct VersionInfo {
    pub id: String,
    #[serde(rename = "mainClass")]
    pub main_class: String,
    #[serde(rename = "minecraftArguments")]
    pub minecraft_arguments: Option<String>,
    pub arguments: Option<Arguments>,
    #[serde(rename = "assetIndex")]
    pub asset_index: Option<AssetIndexRef>,
    pub downloads: Option<Downloads>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(rename = "inheritsFrom")]
    pub inherits_from: Option<String>,
    #[serde(rename = "javaVersion")]
    pub java_version: Option<JavaVersion>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct JavaVersion {
    #[serde(rename = "majorVersion")]
    pub major_version: u32,
}

#[derive(Clone, Deserialize, Debug)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<ArgValue>,
    #[serde(default)]
    pub jvm: Vec<ArgValue>,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(untagged)]
pub enum ArgValue {
    String(String),
    Compound(CompoundArg),
}

#[derive(Clone, Deserialize, Debug)]
pub struct CompoundArg {
    pub rules: Option<Vec<Rule>>,
    pub value: ArgInner,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(untagged)]
pub enum ArgInner {
    String(String),
    List(Vec<String>),
}

#[derive(Clone, Deserialize, Debug)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Clone, Deserialize, Debug)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: Option<String>,
    pub url: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Downloads {
    pub client: DownloadArtifact,
    pub server: Option<DownloadArtifact>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DownloadArtifact {
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub rules: Option<Vec<Rule>>,
    pub natives: Option<HashMap<String, String>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct LibraryDownloads {
    pub artifact: Option<LibraryArtifact>,
    pub classifiers: Option<HashMap<String, LibraryArtifact>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct LibraryArtifact {
    pub path: String,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Deserialize, Debug)]
struct AssetIndex {
    objects: HashMap<String, AssetObject>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct AssetObject {
    hash: String,
    size: u64,
}

pub fn dirs_data() -> PathBuf {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(v).join("mirage")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".local/share/mirage")
    }
}

/// Steps of a launch, in order, for UIs that show where a launch is.
pub const LAUNCH_STAGES: [&str; 4] = ["Signing in", "Game files", "Java", "Starting"];
static LAUNCH_STAGE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Mark the launch as having reached `LAUNCH_STAGES[i]`.
pub fn set_launch_stage(i: usize) {
    LAUNCH_STAGE.store(i, std::sync::atomic::Ordering::Relaxed);
    crate::download::progress().set_label(LAUNCH_STAGES.get(i).copied().unwrap_or(""));
}

pub fn launch_stage() -> usize {
    LAUNCH_STAGE.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct Launcher {
    downloader: Downloader,
    data_dir: PathBuf,
    pub instance: Instance,
    java_override: Option<String>,
}

impl Launcher {
    pub fn new(java_override: Option<String>, instance_name: Option<String>) -> Self {
        let name = instance_name.unwrap_or_else(|| {
            let cfg = crate::config::load();
            cfg.default_instance
        });

        let inst = Instance::load(&name).unwrap_or_else(|_| {
            let new_inst = Instance::new(&name, "latest", "vanilla");
            let _ = new_inst.save();
            new_inst
        });

        Self {
            downloader: Downloader::new(),
            data_dir: dirs_data(),
            instance: inst,
            java_override,
        }
    }

    pub fn instance_mods_dir(&self) -> PathBuf {
        self.instance.mods_dir()
    }

    fn natives_dir(&self, version: &str) -> PathBuf {
        self.data_dir.join("versions").join(version).join("natives")
    }

    fn libraries_dir(&self) -> PathBuf {
        self.data_dir.join("libraries")
    }

    fn assets_dir(&self) -> PathBuf {
        self.data_dir.join("assets")
    }

    pub async fn fetch_manifest(&self) -> Result<VersionManifest> {
        load_manifest(&self.downloader, &self.data_dir).await
    }

    pub async fn resolve_mc_version(&self, version: &str) -> Result<String> {
        if version == "latest" {
            let m = self.fetch_manifest().await?;
            Ok(m.latest.release)
        } else {
            Ok(version.to_string())
        }
    }

    pub async fn manifest_entry(&self, version: &str) -> Result<VersionEntry> {
        let manifest = self.fetch_manifest().await?;
        if version == "latest" {
            manifest
                .versions
                .into_iter()
                .find(|v| v.id == manifest.latest.release)
                .context("Latest release version not found in manifest")
        } else {
            // 1. Exact match
            if let Some(v) = manifest.versions.iter().find(|v| v.id == version) {
                return Ok(v.clone());
            }
            // 2. Case-insensitive match
            if let Some(v) = manifest.versions.iter().find(|v| v.id.eq_ignore_ascii_case(version)) {
                return Ok(v.clone());
            }
            // 3. Prefix match
            if let Some(v) = manifest.versions.iter().find(|v| v.id.starts_with(version)) {
                return Ok(v.clone());
            }
            // 4. Local version file
            let local_json = self
                .data_dir
                .join("versions")
                .join(version)
                .join(format!("{}.json", version));
            if local_json.exists() {
                return Ok(VersionEntry {
                    id: version.to_string(),
                    release_type: "custom".to_string(),
                    url: String::new(),
                });
            }

            let similar: Vec<&str> = manifest
                .versions
                .iter()
                .filter(|v| v.id.contains(version) || version.contains(&v.id))
                .take(5)
                .map(|v| v.id.as_str())
                .collect();

            let suggestions = if !similar.is_empty() {
                format!(". Did you mean: {}?", similar.join(", "))
            } else {
                format!(". Latest release is '{}'. Run 'mirage versions' to list valid versions.", manifest.latest.release)
            };

            anyhow::bail!("Minecraft version '{}' not found in Mojang manifest{}", version, suggestions);
        }
    }

    pub async fn get_version_info(&self, version_id: &str) -> Result<(VersionInfo, PathBuf)> {
        let local_json = self
            .data_dir
            .join("versions")
            .join(version_id)
            .join(format!("{}.json", version_id));

        let mut info: VersionInfo = if local_json.exists() {
            let data = std::fs::read_to_string(&local_json)?;
            serde_json::from_str(&data)?
        } else {
            // Check Mojang manifest
            let entry = self.manifest_entry(version_id).await?;
            let raw = self.downloader.download_bytes(&entry.url).await?;
            std::fs::create_dir_all(local_json.parent().unwrap())?;
            std::fs::write(&local_json, &raw)?;
            serde_json::from_slice(&raw)?
        };

        // If this version inherits from a parent (e.g. Fabric -> Vanilla)
        if let Some(parent_id) = &info.inherits_from {
            let (parent_info, parent_jar) = Box::pin(self.get_version_info(parent_id)).await?;

            // Merge libraries (child libraries first, then parent)
            let mut merged_libs = info.libraries;
            for plib in parent_info.libraries {
                if !merged_libs.iter().any(|l| l.name == plib.name) {
                    merged_libs.push(plib);
                }
            }
            info.libraries = merged_libs;

            if info.downloads.is_none() {
                info.downloads = parent_info.downloads;
            }
            if info.asset_index.is_none() {
                info.asset_index = parent_info.asset_index;
            }
            if info.java_version.is_none() {
                info.java_version = parent_info.java_version;
            }
            if info.minecraft_arguments.is_none() {
                info.minecraft_arguments = parent_info.minecraft_arguments;
            }

            if let Some(parent_args) = parent_info.arguments {
                match &mut info.arguments {
                    Some(child_args) => {
                        let mut jvm = std::mem::take(&mut child_args.jvm);
                        jvm.extend(parent_args.jvm);
                        let game = if child_args.game.is_empty() {
                            parent_args.game
                        } else {
                            std::mem::take(&mut child_args.game)
                        };
                        info.arguments = Some(Arguments { jvm, game });
                    }
                    None => {
                        info.arguments = Some(parent_args);
                    }
                }
            }

            // The client jar to execute is the parent's vanilla jar
            return Ok((info, parent_jar));
        }

        let jar_path = self
            .data_dir
            .join("versions")
            .join(version_id)
            .join(format!("{}.jar", version_id));

        Ok((info, jar_path))
    }

    /// `verify`: SHA-1 check files already on disk. Off for versions that
    /// were fully installed before, where only missing files are fetched.
    async fn download_libraries(
        &self,
        dl: &Downloader,
        version: &str,
        libraries: &[Library],
        verify: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut download_items: Vec<(String, PathBuf, Option<String>)> = Vec::new();
        let mut result_paths = Vec::new();

        for lib in libraries {
            if !self.rules_pass(&lib.rules) {
                continue;
            }

            if let Some(lib_dl) = &lib.downloads {
                if let Some(artifact) = &lib_dl.artifact {
                    let path = self.libraries_dir().join(&artifact.path);
                    result_paths.push(path.clone());
                    download_items.push((
                        artifact.url.clone(),
                        path,
                        Some(artifact.sha1.clone()),
                    ));
                }

                if let Some(natives) = &lib.natives {
                    let os_key = if cfg!(target_os = "linux") {
                        "linux"
                    } else if cfg!(target_os = "macos") {
                        "osx"
                    } else {
                        "windows"
                    };

                    if let Some(classifier) = natives.get(os_key) {
                        let native_key = classifier.replace(
                            "${arch}",
                            if cfg!(target_arch = "aarch64") {
                                "arm64"
                            } else {
                                "64"
                            },
                        );

                        if let Some(native_artifact) =
                            lib_dl.classifiers.as_ref().and_then(|c| c.get(&native_key))
                        {
                            let dest = self.natives_dir(version).join(&native_artifact.path);
                            result_paths.push(dest.clone());
                            download_items.push((
                                native_artifact.url.clone(),
                                dest,
                                Some(native_artifact.sha1.clone()),
                            ));
                        }
                    }
                }
            } else if !lib.name.is_empty() {
                let (local_path, path_str) = Self::maven_path(&lib.name);
                let full = self.libraries_dir().join(&local_path);
                result_paths.push(full.clone());

                if !full.exists() {
                    let repos = [
                        "https://maven.fabricmc.net/",
                        "https://libraries.minecraft.net/",
                        "https://repo1.maven.org/maven2/",
                    ];
                    for repo in &repos {
                        let url = format!("{}{}", repo, path_str);
                        if dl.download_with_sha1(&url, &full, None).await.is_ok() {
                            break;
                        }
                    }
                }
            }
        }

        if !verify {
            download_items.retain(|(_, p, _)| std::fs::metadata(p).map(|m| m.len() == 0).unwrap_or(true));
        }
        if !download_items.is_empty() {
            dl.download_many(download_items, Some("Downloading libraries"))
                .await?;
        }

        Ok(result_paths)
    }

    fn maven_path(name: &str) -> (PathBuf, String) {
        let parts: Vec<&str> = name.split(':').collect();
        let g = parts.first().copied().unwrap_or("");
        let a = parts.get(1).copied().unwrap_or("");
        let v = parts.get(2).copied().unwrap_or("");
        let classifier = parts.get(3).copied();

        let path = if let Some(c) = classifier {
            format!(
                "{}/{}/{}/{}-{}-{}.jar",
                g.replace('.', "/"),
                a,
                v,
                a,
                v,
                c
            )
        } else {
            format!("{}/{}/{}/{}-{}.jar", g.replace('.', "/"), a, v, a, v)
        };

        (PathBuf::from(&path), path)
    }

    async fn download_client_jar(
        &self,
        dl: &Downloader,
        jar_path: &Path,
        info: &VersionInfo,
    ) -> Result<()> {
        if jar_path.exists() {
            return Ok(());
        }

        if let Some(dl_info) = &info.downloads {
            dl.download_with_progress(
                &dl_info.client.url,
                jar_path,
                Some(&dl_info.client.sha1),
                "retrieving client jar",
            )
            .await?;
        }
        Ok(())
    }

    fn classpath(&self, jar_path: &Path, lib_paths: &[PathBuf]) -> String {
        #[cfg(windows)]
        const CP_SEP: &str = ";";
        #[cfg(not(windows))]
        const CP_SEP: &str = ":";

        let mut cp = jar_path.to_string_lossy().to_string();
        for lib in lib_paths {
            if lib.extension().map(|e| e == "jar").unwrap_or(false) && lib.exists() {
                cp.push_str(CP_SEP);
                cp.push_str(&lib.to_string_lossy());
            }
        }
        cp
    }

    async fn download_version(
        &self,
        version_req: &str,
    ) -> Result<(VersionInfo, PathBuf, Vec<PathBuf>)> {
        let (info, jar_path) = self.get_version_info(version_req).await?;
        let version_id = &info.id;
        let ready_marker = self
            .data_dir
            .join("versions")
            .join(version_id)
            .join(".ready");

        if ready_marker.exists() && jar_path.exists() {
            let libs = self.download_libraries(&self.downloader, version_id, &info.libraries, false).await?;
            return Ok((info, jar_path, libs));
        }

        let client_size_mb = info
            .downloads
            .as_ref()
            .map(|d| d.client.size as f64 / (1024.0 * 1024.0))
            .unwrap_or(30.0);
        Term::info(&format!(
            "Resolving Minecraft {version_id}: client {client_size_mb:.1} MiB, {} libraries",
            info.libraries.len()
        ));

        // Client jar, libraries and assets are independent: fetch all three
        // at once so a first launch is bounded by the slowest, not the sum.
        let client = async {
            self.download_client_jar(&self.downloader, &jar_path, &info)
                .await
                .with_context(|| format!("Failed to download client jar '{}'", jar_path.display()))
        };
        let libraries = async {
            self.download_libraries(&self.downloader, version_id, &info.libraries, true)
                .await
                .with_context(|| format!("Failed to download libraries for '{}'", version_id))
        };
        let assets = self.download_assets(&info);
        let ((), libs, ()) = tokio::try_join!(client, libraries, assets)?;

        if let Some(p) = ready_marker.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&ready_marker, b"1")?;
        Ok((info, jar_path, libs))
    }

    async fn download_assets(&self, info: &VersionInfo) -> Result<()> {
        let Some(asset_index) = &info.asset_index else { return Ok(()) };
        let idx_path = self
            .assets_dir()
            .join("indexes")
            .join(format!("{}.json", asset_index.id));

        let idx: AssetIndex = if idx_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&idx_path)?)?
        } else {
            let data = self.downloader.download_bytes(&asset_index.url).await?;
            std::fs::create_dir_all(idx_path.parent().unwrap())?;
            std::fs::write(&idx_path, &data)?;
            serde_json::from_slice(&data)?
        };

        let objs = self.assets_dir().join("objects");
        let mut asset_items = Vec::new();
        for obj in idx.objects.values() {
            let sub = &obj.hash[..2];
            let dest = objs.join(sub).join(&obj.hash);
            if !dest.exists() {
                let url = format!("https://resources.download.minecraft.net/{}/{}", sub, obj.hash);
                asset_items.push((url, dest, Some(obj.hash.clone())));
            }
        }

        if !asset_items.is_empty() {
            Term::info(&format!("{} missing asset objects", asset_items.len()));
            self.downloader
                .download_many(asset_items, Some("retrieving assets"))
                .await
                .context("Failed to download asset files")?;
        }
        Ok(())
    }

    pub async fn prepare(&self, version: &str) -> Result<()> {
        Term::header(&format!("Preparing Minecraft {}", version));
        self.download_version(version).await?;
        Term::done(&format!("Ready: {}", version));
        Ok(())
    }

    /// Pick the Java to run. Explicit overrides win (flag, instance, config).
    /// Otherwise choose among installed JVMs the one matching the version's
    /// required major (exact, else the closest newer one), falling back to
    /// `java` on PATH. Reads each JVM's `release` file instead of spawning
    /// `java -version`, so detection costs microseconds.
    pub fn detect_java_binary(&self, required: Option<u32>) -> (String, Option<u32>) {
        let explicit = self
            .java_override
            .clone()
            .or_else(|| self.instance.java_path.clone())
            .or_else(|| crate::config::load().default_java);
        if let Some(j) = explicit {
            let major = java_major(&j);
            return (j, major);
        }

        let mut candidates: Vec<String> = Vec::new();
        if let Ok(jh) = std::env::var("JAVA_HOME") {
            candidates.push(PathBuf::from(jh).join("bin/java").to_string_lossy().into_owned());
        }
        for root in ["/usr/lib/jvm", "/usr/lib64/jvm", "/opt/java", "/Library/Java/JavaVirtualMachines"] {
            if let Ok(entries) = std::fs::read_dir(root) {
                for e in entries.flatten() {
                    for sub in ["bin/java", "Contents/Home/bin/java"] {
                        let p = e.path().join(sub);
                        if p.is_file() {
                            candidates.push(p.to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
        let path_java = which_bin("java");
        if let Some(p) = &path_java {
            candidates.push(p.clone());
        }
        let found: Vec<(String, u32)> =
            candidates.into_iter().filter_map(|c| java_major(&c).map(|m| (c, m))).collect();

        let pick = match required {
            Some(req) => found
                .iter()
                .find(|(_, m)| *m == req)
                .or_else(|| found.iter().filter(|(_, m)| *m > req).min_by_key(|(_, m)| *m)),
            None => found.iter().max_by_key(|(_, m)| *m),
        };
        match pick {
            Some((p, m)) => (p.clone(), Some(*m)),
            None => {
                let p = path_java.unwrap_or_else(|| "java".into());
                let m = java_major(&p);
                (p, m)
            }
        }
    }

    /// Resolve + download everything and build the full java argv
    /// (`[jvm..., mainClass, game..., extra...]`). Shared by foreground and
    /// new-terminal launches so both run the exact same command.
    async fn prepare_command(
        &mut self,
        account: &MinecraftAccount,
        version_req: &str,
        extra_args: &[String],
    ) -> Result<(String, String, Vec<String>, PathBuf)> {
        set_launch_stage(1);
        let (info, jar_path, lib_paths) = self.download_version(version_req).await?;
        set_launch_stage(2);
        let version_id = info.id.clone();

        Term::header(&format!("Launching Minecraft {} [Instance: {}]", version_id, self.instance.name));

        let required = info.java_version.as_ref().map(|j| j.major_version);
        let (java_bin, java_ver) = self.detect_java_binary(required);
        if let (Some(req), Some(have)) = (required, java_ver) {
            if have < req {
                anyhow::bail!(
                    "Minecraft {} needs Java {} but the newest found is Java {} ({}). Install Java {} or set one with `mirage config` / instance java_path.",
                    version_id, req, have, java_bin, req
                );
            }
            if have != req {
                Term::warn(&format!(
                    "Minecraft {version_id} expects Java {req}; using Java {have}. Install Java {req} if the game crashes on start."
                ));
            }
        }

        let game_args = self.resolve_game_args(&info, account, &version_id)?;
        let jvm_args =
            self.resolve_jvm_args(&info, &version_id, &jar_path, &lib_paths, java_ver.or(required))?;

        let cfg = crate::config::load();
        let mut final_jvm_args = jvm_args;
        final_jvm_args.extend(cfg.jvm_args.iter().cloned());
        final_jvm_args.extend(self.instance.jvm_args.iter().cloned());

        let mut final_game_args = game_args;
        final_game_args.extend(cfg.game_args.iter().cloned());
        final_game_args.extend(self.instance.game_args.iter().cloned());

        Term::label(
            "Java Binary",
            &match java_ver {
                Some(v) => format!("{java_bin} (Java {v})"),
                None => java_bin.clone(),
            },
        );
        Term::label("Player Account", &account.username);
        Term::label("Session", &account.expiry_label());
        if account.is_expired() {
            // Callers refresh expired sessions first (auth::account_for_launch); guard anyway.
            anyhow::bail!(
                "Microsoft session for '{}' is EXPIRED. Run `mirage account login` or `account refresh` first.",
                account.username
            );
        }
        Term::label("Instance Path", &self.instance.dir().display().to_string());

        std::fs::create_dir_all(self.instance.dir())
            .with_context(|| format!("Failed to create instance directory '{}'", self.instance.dir().display()))?;

        let mut argv = final_jvm_args;
        argv.push(info.main_class.clone());
        argv.extend(final_game_args);
        argv.extend(extra_args.iter().cloned());

        let dir = self.instance.dir();
        Ok((version_id, java_bin, argv, dir))
    }

    pub async fn spawn_minecraft(
        &mut self,
        account: &MinecraftAccount,
        version_req: &str,
        extra_args: &[String],
    ) -> Result<std::process::Child> {
        let prep_start = std::time::Instant::now();
        let (version_id, java_bin, argv, dir) = self.prepare_command(account, version_req, extra_args).await?;
        set_launch_stage(3);

        let mut cmd = Command::new(&java_bin);
        cmd.args(&argv);
        cmd.current_dir(&dir);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = cmd
            .spawn()
            .with_context(|| format!("Failed to start Java process with '{}' (working dir: '{}'). Is Java installed?", java_bin, dir.display()))?;

        // Update instance last played
        self.instance.last_played = Some(chrono::Utc::now().to_rfc3339());
        let _ = self.instance.save();

        let prep_elapsed = prep_start.elapsed();
        Term::success(&format!(
            "Minecraft {} process started! (PID: {}) [Prepared in {:.2}s]",
            version_id,
            child.id(),
            prep_elapsed.as_secs_f64()
        ));
        Ok(child)
    }

    /// Spawn the game detached in a new terminal window so the caller (e.g.
    /// the TUI) stays usable. Returns the terminal's PID. Falls back to an
    /// error if no supported emulator is installed — callers should then run
    /// the foreground `launch()` instead.
    pub async fn launch_in_new_terminal(
        &mut self,
        account: &MinecraftAccount,
        version: &str,
        extra_args: &[String],
    ) -> Result<u32> {
        let (version_id, java_bin, argv, dir) = self.prepare_command(account, version, extra_args).await?;
        let (em, em_args) = find_terminal_emulator().ok_or_else(|| {
            anyhow::anyhow!(
                "No terminal emulator found (looked for $TERMINAL, kitty, alacritty, gnome-terminal, konsole, foot, wezterm, xterm). \
                 Turn it off with `mirage config terminal off` to launch in this terminal."
            )
        })?;

        set_launch_stage(3);
        // The script logs `<seconds> <exit code>` to the instance's session
        // file, which is how playtime reaches Mirage from a detached window.
        let mut script = format!("start=$(date +%s); cd {} && ", sh_quote(&dir.display().to_string()));
        script.push_str(&sh_quote(&java_bin));
        for a in &argv {
            script.push(' ');
            script.push_str(&sh_quote(a));
        }
        script.push_str(&format!(
            "\ncode=$?; echo \"$(( $(date +%s) - start )) $code\" >> {}; echo; echo '[mirage] Minecraft {} exited with code '$code' — you can close this window.'; exec ${{SHELL:-bash}}",
            sh_quote(&self.instance.sessions_path().display().to_string()),
            version_id
        ));

        let mut cmd = Command::new(&em);
        cmd.args(&em_args);
        cmd.arg(&script);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        let child = cmd.spawn().with_context(|| format!("Failed to open new terminal with '{}'", em))?;
        let pid = child.id();

        self.instance.last_played = Some(chrono::Utc::now().to_rfc3339());
        let _ = self.instance.save();

        Term::success(&format!("Minecraft {} opened in a new {} window (PID {}) — this terminal stays usable.", version_id, em, pid));
        Ok(pid)
    }

    /// Foreground launch, or detached new-terminal launch per config.
    /// `force`: Some(true) = new terminal, Some(false) = same terminal,
    /// None = follow `launch_new_terminal` in config (default on).
    pub async fn launch_auto(
        &mut self,
        account: &MinecraftAccount,
        version: &str,
        extra_args: &[String],
        force: Option<bool>,
    ) -> Result<()> {
        let want_new = force.unwrap_or_else(|| crate::config::load().launch_new_terminal);
        if want_new {
            match self.launch_in_new_terminal(account, version, extra_args).await {
                Ok(_) => Ok(()),
                Err(e) if find_terminal_emulator().is_none() => {
                    Term::warn(&format!("{} Falling back to this terminal.", e));
                    self.launch(account, version, extra_args).await
                }
                Err(e) => Err(e),
            }
        } else {
            self.launch(account, version, extra_args).await
        }
    }

    pub async fn launch(
        &mut self,
        account: &MinecraftAccount,
        version: &str,
        extra_args: &[String],
    ) -> Result<()> {
        let session_start = std::time::Instant::now();
        let mut child = self.spawn_minecraft(account, version, extra_args).await?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let out = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                println!("{}", colorize_log(&line));
            }
        });
        let err = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("{}", colorize_log(&line));
            }
        });

        let status = child.wait()?;
        out.join().unwrap();
        err.join().unwrap();

        let total_secs = session_start.elapsed().as_secs();
        let _ = self.instance.record_session(total_secs, status.code().unwrap_or(-1));
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;

        let time_formatted = if hours > 0 {
            format!("{}h {}m {}s ({} total seconds)", hours, mins, secs, total_secs)
        } else if mins > 0 {
            format!("{}m {}s ({} minutes, {} seconds)", mins, secs, mins, secs)
        } else {
            format!("{} seconds", secs)
        };

        if !status.success() {
            Term::warn(&format!(
                "Minecraft exited with code {} (Session duration: {})",
                status.code().unwrap_or(-1),
                time_formatted
            ));
        } else {
            Term::done(&format!(
                "Minecraft session closed! Played for: {}",
                time_formatted
            ));
        }

        Ok(())
    }

    fn resolve_game_args(
        &self,
        info: &VersionInfo,
        account: &MinecraftAccount,
        version: &str,
    ) -> Result<Vec<String>> {
        let mut args = Vec::new();

        if let Some(arguments) = &info.arguments {
            for arg in &arguments.game {
                match arg {
                    ArgValue::String(s) => {
                        args.push(s.clone());
                    }
                    ArgValue::Compound(c) => {
                        if self.rules_pass(&c.rules) {
                            match &c.value {
                                ArgInner::String(s) => args.push(s.clone()),
                                ArgInner::List(l) => args.extend(l.iter().cloned()),
                            }
                        }
                    }
                }
            }
        } else if let Some(legacy) = &info.minecraft_arguments {
            args.extend(legacy.split_whitespace().map(String::from));
        }

        let tokens: HashMap<&str, String> = HashMap::from([
            ("${auth_player_name}", account.username.clone()),
            ("${auth_uuid}", account.uuid.clone()),
            ("${auth_access_token}", account.access_token.clone()),
            ("${auth_session}", account.access_token.clone()),
            ("${user_type}", "msa".into()),
            ("${version_name}", version.into()),
            ("${game_directory}", self.instance.dir().to_string_lossy().into()),
            ("${assets_root}", self.assets_dir().to_string_lossy().into()),
            (
                "${assets_index_name}",
                info.asset_index
                    .as_ref()
                    .map_or("default".into(), |a| a.id.clone()),
            ),
            ("${user_properties}", "{}".into()),
            ("${profile_name}", account.username.clone()),
        ]);

        for arg in &mut args {
            for (k, v) in &tokens {
                if arg.contains(k) {
                    *arg = arg.replace(k, v);
                }
            }
        }

        Ok(args)
    }

    fn resolve_jvm_args(
        &self,
        info: &VersionInfo,
        version: &str,
        jar: &Path,
        lib_paths: &[PathBuf],
        java_major: Option<u32>,
    ) -> Result<Vec<String>> {
        let natives = self.natives_dir(version);
        std::fs::create_dir_all(&natives)?;

        let mut args = Vec::new();

        // Memory allocation
        let cfg = crate::config::load();
        let ram_min = self
            .instance
            .ram_min
            .as_ref()
            .unwrap_or(&cfg.ram_min);
        let ram_max = self
            .instance
            .ram_max
            .as_ref()
            .unwrap_or(&cfg.ram_max);

        let (xms, xmx) = fit_heap(ram_min, ram_max, total_memory_mb());
        args.push(format!("-Xms{}", xms));
        args.push(format!("-Xmx{}", xmx));

        let cp_str = self.classpath(jar, lib_paths);
        let natives_str = natives.to_string_lossy().to_string();

        let mut has_cp = false;
        let mut has_natives = false;

        if let Some(arguments) = &info.arguments {
            for arg in &arguments.jvm {
                match arg {
                    ArgValue::String(s) => {
                        let replaced = s
                            .replace("${natives_directory}", &natives_str)
                            .replace("${launcher_name}", "mirage")
                            .replace("${launcher_version}", env!("CARGO_PKG_VERSION"))
                            .replace("${classpath}", &cp_str);
                        if replaced.contains("-Djava.library.path") {
                            has_natives = true;
                        }
                        if replaced == "-cp" || replaced == "-classpath" {
                            has_cp = true;
                        }
                        if !replaced.is_empty() {
                            args.push(replaced);
                        }
                    }
                    ArgValue::Compound(c) => {
                        if self.rules_pass(&c.rules) {
                            match &c.value {
                                ArgInner::String(s) => {
                                    let replaced = s
                                        .replace("${natives_directory}", &natives_str)
                                        .replace("${launcher_name}", "mirage")
                                        .replace("${launcher_version}", env!("CARGO_PKG_VERSION"))
                                        .replace("${classpath}", &cp_str);
                                    if replaced.contains("-Djava.library.path") {
                                        has_natives = true;
                                    }
                                    if replaced == "-cp" || replaced == "-classpath" {
                                        has_cp = true;
                                    }
                                    args.push(replaced);
                                }
                                ArgInner::List(l) => {
                                    for item in l {
                                        let replaced = item
                                            .replace("${natives_directory}", &natives_str)
                                            .replace("${launcher_name}", "mirage")
                                            .replace("${launcher_version}", env!("CARGO_PKG_VERSION"))
                                            .replace("${classpath}", &cp_str);
                                        if replaced.contains("-Djava.library.path") {
                                            has_natives = true;
                                        }
                                        if replaced == "-cp" || replaced == "-classpath" {
                                            has_cp = true;
                                        }
                                        args.push(replaced);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if !has_natives {
            args.push(format!("-Djava.library.path={}", natives_str));
        }

        if !has_cp {
            args.push("-cp".into());
            args.push(cp_str);
        }

        // Module flags only exist on modern Java; Java 8 refuses to start
        // with them, which broke every pre-1.17 version.
        let major = java_major.unwrap_or(8);
        if major >= 9 {
            args.push("--add-opens=java.base/java.lang=ALL-UNNAMED".into());
            args.push("--add-opens=java.base/java.lang.invoke=ALL-UNNAMED".into());
            args.push("--add-opens=java.base/java.util=ALL-UNNAMED".into());
        }
        if major >= 17 {
            args.push("--enable-native-access=ALL-UNNAMED".into());
        }

        Ok(args)
    }

    fn rule_matches(&self, rule: &Rule) -> bool {
        let os_name = std::env::consts::OS;
        let os_arch = std::env::consts::ARCH;

        if let Some(feats) = &rule.features {
            if !feats.is_empty() {
                return false;
            }
        }

        match &rule.os {
            None => true,
            Some(os) => {
                let name_match = match &os.name {
                    Some(n) => {
                        (n == "linux" && os_name == "linux")
                            || (n == "osx" && os_name == "macos")
                            || (n == "windows" && os_name == "windows")
                    }
                    None => true,
                };
                let arch_match = match &os.arch {
                    Some(a) => {
                        (a == "64" && os_arch.contains("64"))
                            || (a == "arm64" && os_arch == "aarch64")
                    }
                    None => true,
                };
                name_match && arch_match
            }
        }
    }

    fn rules_pass(&self, rules: &Option<Vec<Rule>>) -> bool {
        let Some(rules) = rules else { return true };
        if rules.is_empty() { return true; }

        let mut allow = false;
        for rule in rules {
            if self.rule_matches(rule) {
                allow = rule.action == "allow";
            }
        }
        allow
    }
}

pub fn colorize_log(line: &str) -> String {
    use crossterm::style::Stylize;
    if line.contains("ERROR") || line.contains("FATAL") {
        format!("{}", line.red().bold())
    } else if line.contains("WARN") || line.contains("WARNING") {
        format!("{}", line.with(crate::term::Theme::WARN))
    } else if line.contains("INFO") {
        format!("{}", line.with(crate::term::Theme::INFO))
    } else if line.contains("DEBUG") || line.contains("TRACE") {
        format!("{}", line.with(crate::term::Theme::MUTED))
    } else {
        line.to_string()
    }
}

/// Quote one shell word with single quotes (POSIX sh compatible).
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Java major version from the JDK's `release` file next to `bin/java`
/// (`JAVA_VERSION="21.0.2"` → 21, `"1.8.0_392"` → 8).
fn java_major(bin: &str) -> Option<u32> {
    let resolved = if bin.contains('/') { PathBuf::from(bin) } else { PathBuf::from(which_bin(bin)?) };
    let real = std::fs::canonicalize(resolved).ok()?;
    let home = real.parent()?.parent()?;
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let line = text.lines().find(|l| l.starts_with("JAVA_VERSION="))?;
    parse_java_major(line.trim_start_matches("JAVA_VERSION=").trim_matches('"'))
}

fn parse_java_major(v: &str) -> Option<u32> {
    let mut parts = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 { parts.next()?.parse().ok() } else { Some(first) }
}

/// Parse a JVM size ("4G", "512M", "2048m", "1g") into MiB.
pub fn parse_mem_mb(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().ok()?;
    match unit.to_ascii_lowercase().as_str() {
        "g" => Some(n * 1024),
        "m" => Some(n),
        "k" => Some(n / 1024),
        "" => Some(n / (1024 * 1024)),
        _ => None,
    }
}

fn total_memory_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = info.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// Keep the heap inside what the machine has: a 4G `-Xmx` on a 4 GB laptop
/// swaps the whole system to death. Caps `-Xmx` at 3/4 of RAM (leaving room
/// for the OS and the JVM's own overhead) and keeps `-Xms <= -Xmx`.
fn fit_heap(min: &str, max: &str, total_mb: Option<u64>) -> (String, String) {
    let (Some(mut lo), Some(mut hi)) = (parse_mem_mb(min), parse_mem_mb(max)) else {
        return (min.to_string(), max.to_string());
    };
    if let Some(total) = total_mb {
        hi = hi.min((total * 3 / 4).max(512));
    }
    lo = lo.min(hi);
    (format!("{lo}M"), format!("{hi}M"))
}

fn which_bin(name: &str) -> Option<String> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        if p.is_file() {
            return Some(name.to_string());
        }
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

/// Find a terminal emulator we know how to open with a login shell command.
/// Returns (binary, argv-prefix); the game shell script is appended as the
/// final argument. Honors `$TERMINAL` first.
pub fn find_terminal_emulator() -> Option<(String, Vec<String>)> {
    if let Ok(t) = std::env::var("TERMINAL") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            if let Some(bin) = which_bin(&t) {
                return Some((bin, vec!["-e".into(), "bash".into(), "-c".into()]));
            }
        }
    }
    // (binary, prefix args before `bash -c <script>`)
    const CANDIDATES: &[(&str, &[&str])] = &[
        ("kitty", &[]),
        ("alacritty", &["-e"]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("foot", &[]),
        ("wezterm", &["start", "--"]),
        ("xfce4-terminal", &["--", "bash", "-c"]),
        ("xterm", &["-e", "bash", "-c"]),
        ("x-terminal-emulator", &["-e", "bash", "-c"]),
    ];
    for (bin, prefix) in CANDIDATES {
        if let Some(path) = which_bin(bin) {
            let mut args: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
            // Normalize so every entry ends with `bash -c <script>`.
            if matches!(*bin, "kitty" | "foot" | "alacritty" | "konsole" | "gnome-terminal" | "wezterm") {
                args.push("bash".into());
                args.push("-c".into());
            }
            return Some((path, args));
        }
    }
    None
}

/// Mojang version manifest, cached on disk for 2 hours. When the network is
/// down a stale cache is still used, so offline starts never stall.
async fn load_manifest(dl: &Downloader, data_dir: &Path) -> Result<VersionManifest> {
    let path = data_dir.join("version_manifest.json");
    let cached = || -> Option<VersionManifest> {
        serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()
    };
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 7200);
    if fresh {
        if let Some(m) = cached() {
            return Ok(m);
        }
    }
    match dl
        .download_bytes("https://launchermeta.mojang.com/mc/game/version_manifest.json")
        .await
    {
        Ok(raw) => {
            let m: VersionManifest = serde_json::from_slice(&raw)?;
            std::fs::create_dir_all(data_dir)?;
            std::fs::write(&path, &raw)?;
            Ok(m)
        }
        Err(e) => cached().ok_or(e),
    }
}

pub async fn fetch_minecraft_versions(include_snapshots: bool) -> Result<Vec<(String, String)>> {
    let manifest = load_manifest(&Downloader::new(), &dirs_data()).await?;
    let versions: Vec<(String, String)> = manifest
        .versions
        .into_iter()
        .filter(|v| include_snapshots || v.release_type == "release")
        .map(|v| (v.id, v.release_type))
        .collect();
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_major_parses_old_and_new_schemes() {
        assert_eq!(parse_java_major("1.8.0_392"), Some(8));
        assert_eq!(parse_java_major("21.0.12.1"), Some(21));
        assert_eq!(parse_java_major("25"), Some(25));
        assert_eq!(parse_java_major("garbage"), None);
    }

    #[test]
    fn heap_fits_the_machine() {
        assert_eq!(parse_mem_mb("4G"), Some(4096));
        assert_eq!(parse_mem_mb("512m"), Some(512));
        // 4 GB box: 6G max capped to 3 GB, min follows.
        assert_eq!(fit_heap("4G", "6G", Some(4096)), ("3072M".into(), "3072M".into()));
        // Plenty of RAM: untouched.
        assert_eq!(fit_heap("2G", "4G", Some(32768)), ("2048M".into(), "4096M".into()));
        // Unknown RAM or odd sizes: passed through.
        assert_eq!(fit_heap("2G", "4G", None), ("2048M".into(), "4096M".into()));
        assert_eq!(fit_heap("x", "4G", Some(1024)), ("x".into(), "4G".into()));
    }
}
