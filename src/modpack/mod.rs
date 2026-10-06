use crate::download::Downloader;
use crate::instance::Instance;
use crate::term::Term;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(v).join("mirage")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".local/share/mirage")
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct FabricLoaderVersion {
    pub version: String,
    pub stable: bool,
}

#[derive(Deserialize, Clone, Debug)]
pub struct FabricGameVersion {
    pub version: String,
    pub stable: bool,
}

pub async fn fetch_fabric_game_versions() -> Result<Vec<String>> {
    let dl = Downloader::new();
    let res: Vec<FabricGameVersion> = dl
        .get_json("https://meta.fabricmc.net/v2/versions/game")
        .await?;
    Ok(res.into_iter().filter(|v| v.stable).map(|v| v.version).collect())
}

pub async fn install_fabric(mc_version: &str) -> Result<String> {
    let dl = Downloader::new();
    let version_id = format!("fabric-{}", mc_version);
    let profile_dir = data_dir().join("versions").join(&version_id);
    std::fs::create_dir_all(&profile_dir)?;

    let loader_meta: Vec<FabricLoaderVersion> = dl
        .get_json("https://meta.fabricmc.net/v2/versions/loader")
        .await?;
    let loader_ver = loader_meta
        .iter()
        .find(|v| v.stable)
        .map(|v| v.version.as_str())
        .unwrap_or("0.16.13");

    let installer_url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
        mc_version, loader_ver
    );
    let profile: serde_json::Value = dl.get_json(&installer_url).await?;
    let profile_path = profile_dir.join(format!("{}.json", version_id));
    std::fs::write(&profile_path, serde_json::to_string_pretty(&profile)?)?;

    Term::done(&format!("Fabric {} profile saved for MC {}", loader_ver, mc_version));
    Ok(version_id)
}

// ── Modrinth API Data Models ──

#[derive(Deserialize, Clone, Debug)]
pub struct ModHit {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub slug: String,
    pub author: Option<String>,
    pub icon_url: Option<String>,
    pub downloads: Option<u64>,
    pub categories: Option<Vec<String>>,
    pub versions: Option<Vec<String>>,
    #[serde(default)]
    pub follows: Option<u64>,
    #[serde(default)]
    pub date_modified: Option<String>,
}

#[derive(Deserialize)]
struct ModrinthSearchResult {
    hits: Vec<ModHit>,
}

/// Modrinth search orders, in the order a UI should cycle them.
pub const SORTS: [&str; 5] = ["relevance", "downloads", "follows", "newest", "updated"];

/// GET JSON through a small disk cache: a fresh copy returns without touching
/// the network, and a failed request falls back to a stale copy so browsing
/// still works offline.
async fn cached_json<T: serde::de::DeserializeOwned>(url: &str, ttl: std::time::Duration) -> Result<T> {
    use sha1::{Digest, Sha1};
    let key: String = Sha1::digest(url.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let path = crate::download::global_cache_dir().join("api").join(key);
    let fresh = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok()?.elapsed().ok())
        .is_some_and(|age| age < ttl);
    if fresh {
        if let Some(v) = tokio::fs::read(&path).await.ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            return Ok(v);
        }
    }
    let got = Downloader::new().download_bytes(url).await.and_then(|b| {
        let v = serde_json::from_slice(&b).with_context(|| format!("Failed to deserialize JSON from {url}"))?;
        Ok((b, v))
    });
    match got {
        Ok((bytes, v)) => {
            if let Some(dir) = path.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            let _ = tokio::fs::write(&path, bytes).await;
            Ok(v)
        }
        Err(e) => match tokio::fs::read(&path).await.ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(v) => Ok(v),
            None => Err(e),
        },
    }
}

/// Mod categories offered by the filter, `any` first.
pub const MOD_CATEGORIES: &[&str] = &[
    "any", "optimization", "utility", "library", "technology", "magic", "adventure", "worldgen",
    "decoration", "equipment", "mobs", "storage", "food", "management",
];
/// Modpack categories offered by the filter, `any` first.
pub const PACK_CATEGORIES: &[&str] = &[
    "any", "adventure", "technology", "magic", "optimization", "multiplayer", "quests",
    "lightweight", "challenging", "kitchen-sink", "combat",
];

/// One Modrinth search.
pub struct SearchQuery<'a> {
    /// "mod" or "modpack".
    pub kind: &'a str,
    /// Empty lists the most popular projects for `sort`.
    pub query: &'a str,
    pub loader: Option<&'a str>,
    pub mc_version: Option<&'a str>,
    /// A category slug; `None` or "any" means no filter.
    pub category: Option<&'a str>,
    pub sort: &'a str,
    pub limit: usize,
}

/// Run a search. When the loader + version filter finds nothing, it retries
/// with the loader filter alone.
pub async fn search(q: &SearchQuery<'_>) -> Result<Vec<ModHit>> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    let loader = q.loader.filter(|l| !l.is_empty() && *l != "vanilla").map(str::to_lowercase);
    let mc_version = q.mc_version.filter(|v| !v.is_empty() && *v != "latest");
    let category = q.category.filter(|c| !c.is_empty() && *c != "any");
    let build = |with_version: bool| {
        let mut facets = vec![format!(r#"["project_type:{}"]"#, q.kind)];
        if let Some(l) = &loader {
            facets.push(format!(r#"["categories:{l}"]"#));
        }
        if let Some(c) = category {
            facets.push(format!(r#"["categories:{c}"]"#));
        }
        if let (true, Some(v)) = (with_version, mc_version) {
            facets.push(format!(r#"["versions:{v}"]"#));
        }
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        format!(
            "https://api.modrinth.com/v2/search?query={}&facets={}&limit={}&index={}",
            enc(q.query),
            enc(&format!("[{}]", facets.join(","))),
            q.limit.clamp(5, 50),
            enc(q.sort),
        )
    };
    let res: ModrinthSearchResult = cached_json(&build(true), TTL).await?;
    if !res.hits.is_empty() || mc_version.is_none() {
        return Ok(res.hits);
    }
    Ok(cached_json::<ModrinthSearchResult>(&build(false), TTL).await?.hits)
}

/// Every version of a project, newest first (changelogs left out: they
/// dominate the payload and nothing here shows them).
pub async fn project_versions(project_id: &str) -> Result<Vec<ModrinthVersion>> {
    cached_json(
        &format!("https://api.modrinth.com/v2/project/{project_id}/version?include_changelog=false"),
        std::time::Duration::from_secs(15 * 60),
    )
    .await
}

pub async fn search_mods(
    query: &str,
    loader: Option<&str>,
    mc_version: Option<&str>,
    limit: usize,
) -> Result<Vec<ModHit>> {
    search(&SearchQuery { kind: "mod", query, loader, mc_version, category: None, sort: "relevance", limit }).await
}

pub async fn search_modpacks(query: &str, limit: usize) -> Result<Vec<ModHit>> {
    search(&SearchQuery { kind: "modpack", query, loader: None, mc_version: None, category: None, sort: "relevance", limit }).await
}

#[derive(Deserialize, Clone, Debug)]
pub struct ModrinthVersion {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<ModrinthFile>,
    pub dependencies: Vec<ModrinthDependency>,
    /// "release", "beta" or "alpha".
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub date_published: String,
    #[serde(default)]
    pub downloads: u64,
}

impl ModrinthVersion {
    /// True when this build targets the instance's game version and loader.
    pub fn fits(&self, mc_version: &str, loader: &str) -> bool {
        let mc_ok = mc_version == "latest" || self.game_versions.iter().any(|v| v == mc_version);
        let loader_ok = loader == "vanilla" || self.loaders.iter().any(|l| l.eq_ignore_ascii_case(loader));
        mc_ok && loader_ok
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct ModrinthFile {
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
    pub hashes: ModrinthHashes,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ModrinthHashes {
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ModrinthDependency {
    pub project_id: Option<String>,
    pub dependency_type: String, // "required", "optional", "incompatible"
}

pub async fn resolve_project_id(slug_or_id: &str) -> Result<String> {
    if slug_or_id.len() == 8 && slug_or_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(slug_or_id.to_string());
    }
    let dl = Downloader::new();
    let url = format!("https://api.modrinth.com/v2/project/{}", slug_or_id);
    let resp: serde_json::Value = dl.get_json(&url).await?;
    resp["id"]
        .as_str()
        .map(String::from)
        .context(format!("Project '{}' not found on Modrinth", slug_or_id))
}

pub async fn pick_best_version(
    project_id: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> Result<ModrinthVersion> {
    let versions = project_versions(project_id).await?;

    if versions.is_empty() {
        bail!("No versions found for project {}", project_id);
    }

    // 1. Exact match: MC version + Loader (e.g. 1.21.1 + fabric)
    let exact_matched: Vec<&ModrinthVersion> = versions
        .iter()
        .filter(|v| {
            let mc_ok = match mc_version {
                Some(mc) if mc != "latest" => v.game_versions.contains(&mc.to_string()),
                _ => true,
            };
            let loader_ok = match loader {
                Some(l) if l != "vanilla" => v.loaders.iter().any(|x| x.eq_ignore_ascii_case(l)),
                _ => true,
            };
            mc_ok && loader_ok
        })
        .collect();

    if let Some(best) = exact_matched.first() {
        return Ok((*best).clone());
    }

    // 2. Compatible major MC version + Loader
    if let Some(mc) = mc_version {
        let major = mc.split('.').take(2).collect::<Vec<&str>>().join(".");
        let major_matched: Vec<&ModrinthVersion> = versions
            .iter()
            .filter(|v| {
                let mc_ok = v.game_versions.iter().any(|gv| gv.starts_with(&major));
                let loader_ok = match loader {
                    Some(l) if l != "vanilla" => v.loaders.iter().any(|x| x.eq_ignore_ascii_case(l)),
                    _ => true,
                };
                mc_ok && loader_ok
            })
            .collect();

        if let Some(best) = major_matched.first() {
            return Ok((*best).clone());
        }
    }

    // 3. Match loader alone (e.g. any Fabric version)
    if let Some(l) = loader {
        if l != "vanilla" {
            if let Some(best) = versions.iter().find(|v| v.loaders.iter().any(|x| x.eq_ignore_ascii_case(l))) {
                return Ok(best.clone());
            }
        }
    }

    // 4. Fallback: latest release
    Ok(versions[0].clone())
}

/// Project fields returned by `/v2/projects`.
#[derive(Deserialize)]
struct Project {
    id: String,
    slug: String,
    title: String,
    description: String,
    icon_url: Option<String>,
    downloads: u64,
    followers: u64,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    updated: Option<String>,
}

/// A local jar matched to its Modrinth project by file hash.
#[derive(Clone, Debug)]
pub struct IdentifiedMod {
    pub project: ModHit,
    pub version_number: String,
    /// SHA-1 of the local jar.
    pub sha1: String,
    /// The Modrinth version the jar is a build of.
    pub version: ModrinthVersion,
}

/// Look up local mod jars on Modrinth by SHA-1. Returns a map keyed by file
/// name (without a trailing `.disabled`); jars Modrinth does not know are
/// simply absent.
pub async fn identify_mods(files: Vec<PathBuf>) -> Result<std::collections::HashMap<String, IdentifiedMod>> {
    use std::collections::HashMap;
    let hashed: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
        files
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().trim_end_matches(".disabled").to_string();
                Some((name, crate::download::file_sha1(p)?))
            })
            .collect()
    })
    .await?;
    if hashed.is_empty() {
        return Ok(HashMap::new());
    }

    let dl = Downloader::new();
    let hashes: Vec<&str> = hashed.iter().map(|(_, h)| h.as_str()).collect();
    let by_hash: HashMap<String, ModrinthVersion> = dl
        .post_json(
            "https://api.modrinth.com/v2/version_files",
            &serde_json::json!({ "hashes": hashes, "algorithm": "sha1" }),
        )
        .await?;

    let mut ids: Vec<&str> = by_hash.values().map(|v| v.project_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids_json = serde_json::to_string(&ids)?;
    let projects: Vec<Project> = dl
        .get_json(&format!(
            "https://api.modrinth.com/v2/projects?ids={}",
            url::form_urlencoded::byte_serialize(ids_json.as_bytes()).collect::<String>()
        ))
        .await?;
    let projects: HashMap<String, Project> = projects.into_iter().map(|p| (p.id.clone(), p)).collect();

    Ok(hashed
        .into_iter()
        .filter_map(|(name, hash)| {
            let ver = by_hash.get(&hash)?;
            let p = projects.get(&ver.project_id)?;
            let project = ModHit {
                project_id: p.id.clone(),
                title: p.title.clone(),
                description: p.description.clone(),
                slug: p.slug.clone(),
                author: None,
                icon_url: p.icon_url.clone(),
                downloads: Some(p.downloads),
                categories: Some(p.categories.clone()),
                versions: None,
                follows: Some(p.followers),
                date_modified: p.updated.clone(),
            };
            Some((name, IdentifiedMod { project, version_number: ver.version_number.clone(), sha1: hash, version: ver.clone() }))
        })
        .collect())
}

#[derive(Debug)]
pub struct InstalledModDetails {
    pub filename: String,
    pub version_number: String,
    pub title: String,
    /// Required dependencies that had to be downloaded as well.
    pub dependencies: usize,
}

pub async fn install_mod_to_instance(
    slug_or_id: &str,
    instance: &Instance,
) -> Result<InstalledModDetails> {
    let project_id = resolve_project_id(slug_or_id).await?;
    let ver = pick_best_version(
        &project_id,
        Some(&instance.mc_version),
        Some(&instance.loader),
    )
    .await?;
    install_mod_version(&ver, instance).await
}

fn primary_file(v: &ModrinthVersion) -> Option<&ModrinthFile> {
    v.files.iter().find(|f| f.primary).or_else(|| v.files.first())
}

/// Install one specific Modrinth version into `instance`, plus its required
/// dependencies (downloaded concurrently with the jar itself).
pub async fn install_mod_version(
    ver: &ModrinthVersion,
    instance: &Instance,
) -> Result<InstalledModDetails> {
    let file = primary_file(ver).context("No jar file found for this mod version")?;
    let mods_dir = instance.mods_dir();
    std::fs::create_dir_all(&mods_dir)?;

    let (dl, dest, label) = (Downloader::new(), mods_dir.join(&file.filename), format!("retrieving {}", file.filename));
    let jar = dl.download_with_progress(&file.url, &dest, file.hashes.sha1.as_deref(), &label);
    let (jar, dependencies) = tokio::join!(jar, install_required_dependencies(ver, instance));
    jar?;

    Ok(InstalledModDetails {
        filename: file.filename.clone(),
        version_number: ver.version_number.clone(),
        title: ver.name.clone(),
        dependencies,
    })
}

/// Required dependencies of `ver`, and theirs in turn, resolved level by
/// level. Projects already installed are skipped, so a newer jar never ends
/// up beside an older one (Fabric refuses duplicates). Best effort: a
/// dependency that cannot be fetched never blocks the mod the user asked
/// for. Returns how many jars were downloaded.
async fn install_required_dependencies(ver: &ModrinthVersion, instance: &Instance) -> usize {
    use std::collections::HashSet;
    let required = |v: &ModrinthVersion| -> Vec<String> {
        v.dependencies
            .iter()
            .filter(|d| d.dependency_type == "required")
            .filter_map(|d| d.project_id.clone())
            .collect()
    };
    let mut queue = required(ver);
    if queue.is_empty() {
        return 0;
    }
    let mods_dir = instance.mods_dir();
    let jars: Vec<PathBuf> = instance.installed_mods().into_iter().map(|m| mods_dir.join(m.filename)).collect();
    let mut seen: HashSet<String> = identify_mods(jars)
        .await
        .map(|known| known.values().map(|m| m.project.project_id.clone()).collect())
        .unwrap_or_default();
    seen.insert(ver.project_id.clone());

    let mut installed = 0;
    while !queue.is_empty() {
        queue.retain(|id| seen.insert(id.clone()));
        let mut level = tokio::task::JoinSet::new();
        for id in std::mem::take(&mut queue) {
            let (mc, loader, dir) = (instance.mc_version.clone(), instance.loader.clone(), mods_dir.clone());
            level.spawn(async move {
                let d_ver = pick_best_version(&id, Some(&mc), Some(&loader)).await?;
                let f = primary_file(&d_ver).context("dependency has no files")?;
                let dest = dir.join(&f.filename);
                let fresh = !dest.exists();
                if fresh {
                    Downloader::new()
                        .download_with_progress(
                            &f.url,
                            &dest,
                            f.hashes.sha1.as_deref(),
                            &format!("retrieving dependency {}", f.filename),
                        )
                        .await?;
                }
                anyhow::Ok((d_ver, fresh))
            });
        }
        while let Some(done) = level.join_next().await {
            if let Ok(Ok((d_ver, fresh))) = done {
                installed += usize::from(fresh);
                queue.extend(required(&d_ver));
            }
        }
    }
    installed
}

/// Newer builds of installed mods: a map from jar file name to the latest
/// Modrinth version for the instance's loader and game version. Mods that are
/// already current (or have nothing newer for this setup) are absent.
pub async fn check_updates(
    known: &std::collections::HashMap<String, IdentifiedMod>,
    loader: &str,
    mc_version: &str,
) -> Result<std::collections::HashMap<String, ModrinthVersion>> {
    if known.is_empty() || loader == "vanilla" {
        return Ok(Default::default());
    }
    let hashes: Vec<&str> = known.values().map(|m| m.sha1.as_str()).collect();
    let mut body = serde_json::json!({ "hashes": hashes, "algorithm": "sha1", "loaders": [loader] });
    if mc_version != "latest" {
        body["game_versions"] = serde_json::json!([mc_version]);
    }
    let latest: std::collections::HashMap<String, ModrinthVersion> = Downloader::new()
        .post_json("https://api.modrinth.com/v2/version_files/update", &body)
        .await?;
    Ok(known
        .iter()
        .filter_map(|(name, m)| {
            let v = latest.get(&m.sha1)?;
            // RFC 3339 timestamps order lexically; never "update" backwards.
            let newer = v.date_published > m.version.date_published;
            let same_jar = v.files.iter().any(|f| f.hashes.sha1.as_deref() == Some(m.sha1.as_str()));
            (newer && !same_jar).then(|| (name.clone(), v.clone()))
        })
        .collect())
}

/// Replace the installed jar `old_filename` with `new`, keeping it switched
/// off if it was. The old jar goes only after the new one has arrived.
pub async fn update_mod(
    new: &ModrinthVersion,
    instance: &Instance,
    old_filename: &str,
) -> Result<InstalledModDetails> {
    let was_disabled = old_filename.ends_with(".disabled");
    let details = install_mod_version(new, instance).await?;
    if old_filename != details.filename {
        instance.remove_mod(old_filename)?;
    }
    if was_disabled {
        instance.toggle_mod(&details.filename)?;
    }
    Ok(details)
}

// ── Modrinth .mrpack export ──

/// Where exports land: Downloads when it exists, else the home directory.
pub fn export_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

pub struct Exported {
    pub path: PathBuf,
    /// Mods referenced by Modrinth download link.
    pub linked: usize,
    /// Mods that travel inside the pack (not on Modrinth, or offline).
    pub bundled: usize,
}

/// Loader entry for `modrinth.index.json`, e.g. `("fabric-loader", "0.16.9")`.
async fn loader_dependency(inst: &Instance) -> Result<Option<(&'static str, String)>> {
    let (key, meta) = match inst.loader.as_str() {
        "vanilla" => return Ok(None),
        "fabric" => ("fabric-loader", "https://meta.fabricmc.net/v2/versions/loader"),
        "quilt" => ("quilt-loader", "https://meta.quiltmc.org/v3/versions/loader"),
        "forge" => ("forge", ""),
        "neoforge" => ("neoforge", ""),
        other => bail!("Cannot export a '{other}' instance"),
    };
    if let Some(v) = &inst.loader_version {
        return Ok(Some((key, v.clone())));
    }
    if meta.is_empty() {
        bail!("Unknown {} version for '{}'", inst.loader, inst.name);
    }
    // Instances made without a pinned loader run the newest stable one.
    let versions: Vec<serde_json::Value> = cached_json(meta, std::time::Duration::from_secs(3600)).await?;
    let newest = versions
        .iter()
        .find(|v| v["stable"].as_bool().unwrap_or(true))
        .and_then(|v| v["version"].as_str())
        .context("No loader versions published")?;
    Ok(Some((key, newest.to_string())))
}

fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
        match e.file_type() {
            Ok(t) if t.is_dir() => collect_files(&e.path(), &name, out),
            Ok(t) if t.is_file() => out.push((name, e.path())),
            _ => {}
        }
    }
}

fn write_mrpack(path: &Path, index: &serde_json::Value, bundled: &[(String, PathBuf)]) -> Result<()> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("modrinth.index.json", opts)?;
    zip.write_all(serde_json::to_string_pretty(index)?.as_bytes())?;
    for (name, src) in bundled {
        zip.start_file(name, opts)?;
        std::io::copy(&mut std::fs::File::open(src)?, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

/// Pack `inst` into `<dest>/<name>.mrpack`, which Modrinth, Prism and Mirage
/// can all install. Enabled mods found on Modrinth are linked, the rest and
/// the `config/` folder are bundled. Worlds and options are left out.
pub async fn export_mrpack(inst: &Instance, dest: &Path) -> Result<Exported> {
    let mods: Vec<_> = inst.installed_mods().into_iter().filter(|m| m.enabled).collect();
    let mods_dir = inst.mods_dir();
    // Offline, every jar simply travels inside the pack.
    let known = identify_mods(mods.iter().map(|m| mods_dir.join(&m.filename)).collect()).await.unwrap_or_default();

    let (mut files, mut bundled, mut linked) = (Vec::new(), Vec::new(), 0);
    for m in &mods {
        let link = known.get(&m.filename).and_then(|k| {
            let f = k.version.files.iter().find(|f| f.hashes.sha1.as_deref() == Some(k.sha1.as_str()))?;
            Some((k.sha1.clone(), f.hashes.sha512.clone()?, f.url.clone(), f.size))
        });
        match link {
            Some((sha1, sha512, url, size)) => {
                linked += 1;
                files.push(serde_json::json!({
                    "path": format!("mods/{}", m.filename),
                    "hashes": { "sha1": sha1, "sha512": sha512 },
                    "downloads": [url],
                    "fileSize": size,
                }));
            }
            None => bundled.push((format!("overrides/mods/{}", m.filename), mods_dir.join(&m.filename))),
        }
    }
    let bundled_mods = bundled.len();
    collect_files(&inst.dir().join("config"), "overrides/config", &mut bundled);

    let mut deps = serde_json::json!({ "minecraft": inst.mc_version });
    if let Some((key, version)) = loader_dependency(inst).await? {
        deps[key] = version.into();
    }
    let index = serde_json::json!({
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0.0",
        "name": inst.name,
        "summary": inst.notes,
        "files": files,
        "dependencies": deps,
    });

    std::fs::create_dir_all(dest)?;
    let path = dest.join(format!("{}.mrpack", inst.name));
    let out = path.clone();
    let written = tokio::task::spawn_blocking(move || write_mrpack(&out, &index, &bundled)).await?;
    if let Err(e) = written {
        let _ = std::fs::remove_file(&path);
        return Err(e.context("Could not write the modpack"));
    }
    Ok(Exported { path, linked, bundled: bundled_mods })
}

// ── Modrinth .mrpack Modpack Engine ──

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct MrpackIndex {
    game: String,
    #[serde(rename = "formatVersion")]
    format_version: u32,
    name: String,
    summary: Option<String>,
    dependencies: std::collections::HashMap<String, String>,
    files: Vec<MrpackFile>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct MrpackFile {
    path: String,
    hashes: ModrinthHashes,
    downloads: Vec<String>,
    #[serde(rename = "fileSize")]
    file_size: u64,
}

pub async fn install_modpack(
    slug_or_id: &str,
    custom_name: Option<&str>,
) -> Result<Instance> {
    let project_id = resolve_project_id(slug_or_id).await?;

    Term::header("Installing Modpack from Modrinth");
    Term::info(&format!("Fetching modpack details for '{}'...", slug_or_id));

    let ver = pick_best_version(&project_id, None, None).await?;
    install_modpack_version(&ver, custom_name).await
}

/// Install one specific release of a modpack as a new instance.
pub async fn install_modpack_version(
    ver: &ModrinthVersion,
    custom_name: Option<&str>,
) -> Result<Instance> {
    let mrpack_file = ver
        .files
        .iter()
        .find(|f| f.filename.ends_with(".mrpack"))
        .or_else(|| ver.files.first())
        .context("No .mrpack file found in modpack release")?;

    let temp_dir = data_dir().join("temp");
    std::fs::create_dir_all(&temp_dir)?;
    let mrpack_path = temp_dir.join(&mrpack_file.filename);

    Term::info(&format!("Downloading {}...", mrpack_file.filename));
    Downloader::new()
        .download_with_progress(
            &mrpack_file.url,
            &mrpack_path,
            mrpack_file.hashes.sha1.as_deref(),
            &format!("retrieving {}", mrpack_file.filename),
        )
        .await?;

    let mut inst = install_mrpack_archive(&mrpack_path, custom_name).await?;
    let _ = std::fs::remove_file(&mrpack_path);

    // Remember where the instance came from, so UIs can show the pack's
    // icon. Best effort: a failed lookup never fails the install.
    inst.modrinth_project = Some(ver.project_id.clone());
    if let Ok(p) = Downloader::new()
        .get_json::<serde_json::Value>(&format!("https://api.modrinth.com/v2/project/{}", ver.project_id))
        .await
    {
        inst.icon = p["icon_url"].as_str().map(String::from);
    }
    let _ = inst.save();
    Ok(inst)
}

struct ExtractedMrpack {
    mc_version: String,
    loader: String,
    loader_version: Option<String>,
    name: String,
    summary: Option<String>,
    download_items: Vec<(String, PathBuf, Option<String>)>,
}

fn extract_mrpack_sync(mrpack_path: &Path, inst_dir: &Path) -> Result<ExtractedMrpack> {
    let file = std::fs::File::open(mrpack_path)
        .with_context(|| format!("Failed to open .mrpack file: {}", mrpack_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .context("Failed to read .mrpack as zip archive")?;

    let index_data = {
        let mut index_file = archive
            .by_name("modrinth.index.json")
            .context("Invalid modpack: missing modrinth.index.json")?;
        let mut data = String::new();
        index_file.read_to_string(&mut data)?;
        data
    };

    let index: MrpackIndex = serde_json::from_str(&index_data)
        .context("Failed to parse modrinth.index.json")?;

    let mc_version = index
        .dependencies
        .get("minecraft")
        .cloned()
        .unwrap_or_else(|| "latest".to_string());

    let (loader, loader_version) = if let Some(fabric) = index.dependencies.get("fabric-loader") {
        ("fabric".to_string(), Some(fabric.clone()))
    } else if let Some(quilt) = index.dependencies.get("quilt-loader") {
        ("quilt".to_string(), Some(quilt.clone()))
    } else if let Some(forge) = index.dependencies.get("forge") {
        ("forge".to_string(), Some(forge.clone()))
    } else if let Some(neoforge) = index.dependencies.get("neoforge") {
        ("neoforge".to_string(), Some(neoforge.clone()))
    } else {
        ("vanilla".to_string(), None)
    };

    // Extract overrides/ and client-overrides/
    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        let name = f.name().to_string();

        let rel_path = name.strip_prefix("overrides/").or_else(|| name.strip_prefix("client-overrides/"));

        if let Some(rel) = rel_path {
            if !rel.is_empty() {
                let out_path = inst_dir.join(rel);
                if f.is_dir() {
                    std::fs::create_dir_all(&out_path)?;
                } else {
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut out_file = std::fs::File::create(&out_path)?;
                    std::io::copy(&mut f, &mut out_file)?;
                }
            }
        }
    }

    let mut download_items = Vec::new();
    for mr_file in &index.files {
        if let Some(url) = mr_file.downloads.first() {
            let dest = inst_dir.join(&mr_file.path);
            download_items.push((url.clone(), dest, mr_file.hashes.sha1.clone()));
        }
    }

    Ok(ExtractedMrpack {
        mc_version,
        loader,
        loader_version,
        name: index.name,
        summary: index.summary,
        download_items,
    })
}

pub async fn install_mrpack_archive(
    mrpack_path: &Path,
    custom_name: Option<&str>,
) -> Result<Instance> {
    let dummy_dir = PathBuf::from("/tmp");
    let initial_meta = extract_mrpack_sync(mrpack_path, &dummy_dir)?;

    let default_name = initial_meta
        .name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>();

    let instance_name = custom_name.unwrap_or(&default_name);
    let mut inst = Instance::create(
        instance_name,
        &initial_meta.mc_version,
        &initial_meta.loader,
        initial_meta.loader_version.as_deref(),
    )?;
    inst.notes = initial_meta.summary;
    inst.save()?;

    Term::header(&format!("Setting Up Modpack: {}", initial_meta.name));
    Term::label("Instance Name", instance_name);
    Term::label("Minecraft", &initial_meta.mc_version);
    Term::label("Loader", &format!("{} ({})", initial_meta.loader, initial_meta.loader_version.as_deref().unwrap_or("latest")));

    // Re-extract to instance directory
    let inst_dir = inst.dir();
    let extracted = extract_mrpack_sync(mrpack_path, &inst_dir)?;
    Term::label("Total Mods/Files", &extracted.download_items.len().to_string());

    // Download files in batch
    let dl = Downloader::new();
    if !extracted.download_items.is_empty() {
        Term::info(&format!(
            "Retrieving {} modpack packages...",
            extracted.download_items.len()
        ));
        dl.download_many(extracted.download_items, Some("retrieving modpack mods")).await?;
    }

    // Install loader profile if Fabric
    if extracted.loader == "fabric" {
        Term::info("Preparing Fabric profile...");
        let _ = install_fabric(&extracted.mc_version).await;
    }

    Term::done(&format!("Modpack '{}' ready in instance '{}'!", initial_meta.name, instance_name));
    Ok(inst)
}

// ── Interactive CLI Wrappers ──

pub async fn install_mod_interactive(query: &str, instance_name: Option<&str>) -> Result<()> {
    let inst_name = instance_name.unwrap_or("default");
    let inst = Instance::load(inst_name)
        .unwrap_or_else(|_| Instance::new(inst_name, "latest", "vanilla"));

    let hits = search_mods(query, Some(&inst.loader), Some(&inst.mc_version), 15).await?;
    if hits.is_empty() {
        bail!("No mods found for query '{}'", query);
    }

    let items: Vec<crate::menu::MenuItem> = hits
        .iter()
        .map(|h| {
            let d = h.downloads.unwrap_or(0);
            let author = h.author.as_deref().unwrap_or("unknown");
            let short_desc = if h.description.len() > 65 {
                format!("{}...", &h.description[..65])
            } else {
                h.description.clone()
            };
            crate::menu::MenuItem {
                label: format!("{} (by {})", h.title, author),
                desc: format!("{} | ⬇ {}", short_desc, d),
            }
        })
        .collect();

    match crate::menu::show_menu(&format!("Search: '{}' (Instance: {})", query, inst.name), &items) {
        crate::menu::MenuAction::Selected(idx) => {
            let hit = &hits[idx];
            let res = install_mod_to_instance(&hit.slug, &inst).await?;
            Term::success(&format!("Installed {} ({}) into instance '{}'!", hit.title, res.version_number, inst.name));
        }
        crate::menu::MenuAction::Quit => {
            println!("  Cancelled");
        }
    }

    Ok(())
}

pub async fn install_modpack_interactive(query: &str) -> Result<()> {
    let hits = search_modpacks(query, 15).await?;
    if hits.is_empty() {
        bail!("No modpacks found for query '{}'", query);
    }

    let items: Vec<crate::menu::MenuItem> = hits
        .iter()
        .map(|h| {
            let d = h.downloads.unwrap_or(0);
            let short_desc = if h.description.len() > 65 {
                format!("{}...", &h.description[..65])
            } else {
                h.description.clone()
            };
            crate::menu::MenuItem {
                label: h.title.clone(),
                desc: format!("{} | ⬇ {}", short_desc, d),
            }
        })
        .collect();

    match crate::menu::show_menu(&format!("Modpacks Search: '{}'", query), &items) {
        crate::menu::MenuAction::Selected(idx) => {
            let hit = &hits[idx];
            let _ = install_modpack(&hit.slug, None).await?;
        }
        crate::menu::MenuAction::Quit => {
            println!("  Cancelled");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_pack_installs_back_with_its_overrides() {
        let dir = std::env::temp_dir().join(format!("mirage-mrpack-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("local.jar");
        std::fs::write(&jar, b"jar").unwrap();

        let index = serde_json::json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1.0.0", "name": "demo", "summary": null,
            "files": [{
                "path": "mods/sodium.jar",
                "hashes": { "sha1": "a", "sha512": "b" },
                "downloads": ["https://cdn.modrinth.com/data/x/sodium.jar"],
                "fileSize": 3
            }],
            "dependencies": { "minecraft": "1.21.1", "fabric-loader": "0.16.9" },
        });
        let pack = dir.join("demo.mrpack");
        write_mrpack(&pack, &index, &[("overrides/mods/local.jar".into(), jar)]).unwrap();

        let out = dir.join("inst");
        let got = extract_mrpack_sync(&pack, &out).unwrap();
        assert_eq!((got.mc_version.as_str(), got.loader.as_str()), ("1.21.1", "fabric"));
        assert_eq!(got.loader_version.as_deref(), Some("0.16.9"));
        assert_eq!(got.download_items.len(), 1);
        assert_eq!(std::fs::read(out.join("mods/local.jar")).unwrap(), b"jar");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn category_lists_start_with_no_filter() {
        assert_eq!(MOD_CATEGORIES[0], "any");
        assert_eq!(PACK_CATEGORIES[0], "any");
    }
}
