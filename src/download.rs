use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::AsyncWriteExt;

/// Process-wide download counters. Every download feeds them, so a UI can
/// show real progress (bytes, files, speed, ETA) without any plumbing
/// through the launcher or modpack code.
pub struct Progress {
    pub bytes: AtomicU64,
    /// Sum of sizes announced so far (content-length of sized downloads).
    pub bytes_total: AtomicU64,
    pub files: AtomicU64,
    pub files_total: AtomicU64,
    /// Files whose size is part of `bytes_total`. When it equals
    /// `files_total`, the byte ratio is exact; otherwise use the file ratio.
    sized: AtomicU64,
    label: Mutex<String>,
}

pub struct ProgressSnapshot {
    pub bytes: u64,
    pub bytes_total: u64,
    pub files: u64,
    pub files_total: u64,
    pub label: String,
}

impl ProgressSnapshot {
    /// Completion in 0..=1, or `None` before anything is known.
    pub fn fraction(&self) -> Option<f64> {
        if self.files_total == 0 {
            return None;
        }
        let f = if self.bytes_total > 0 && self.files_total <= 2 {
            self.bytes as f64 / self.bytes_total as f64
        } else {
            self.files as f64 / self.files_total as f64
        };
        Some(f.clamp(0.0, 1.0))
    }
}

pub fn progress() -> &'static Progress {
    static P: Progress = Progress {
        bytes: AtomicU64::new(0),
        bytes_total: AtomicU64::new(0),
        files: AtomicU64::new(0),
        files_total: AtomicU64::new(0),
        sized: AtomicU64::new(0),
        label: Mutex::new(String::new()),
    };
    &P
}

impl Progress {
    pub fn reset(&self) {
        for a in [&self.bytes, &self.bytes_total, &self.files, &self.files_total, &self.sized] {
            a.store(0, Ordering::Relaxed);
        }
        self.set_label("");
    }

    pub fn set_label(&self, l: &str) {
        if let Ok(mut g) = self.label.lock() {
            l.clone_into(&mut g);
        }
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        let files_total = self.files_total.load(Ordering::Relaxed);
        let sized = self.sized.load(Ordering::Relaxed);
        ProgressSnapshot {
            bytes: self.bytes.load(Ordering::Relaxed),
            bytes_total: if sized == files_total { self.bytes_total.load(Ordering::Relaxed) } else { 0 },
            files: self.files.load(Ordering::Relaxed),
            files_total,
            label: self.label.lock().map(|g| g.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Clone)]
pub struct Downloader {
    client: reqwest::Client,
    pub total_downloaded: Arc<AtomicU64>,
}

pub fn global_cache_dir() -> PathBuf {
    let base = if let Ok(data) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(data).join("mirage")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share").join("mirage")
    } else {
        PathBuf::from(".mirage")
    };
    base.join("cache")
}

pub fn link_or_copy(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dst.exists() {
        let _ = std::fs::remove_file(dst);
    }
    // Fast hard link first (0 bytes disk used, instant 0.1ms link)
    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    // Fallback to copy if cross-device link or unsupported
    std::fs::copy(src, dst)?;
    Ok(())
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloader {
    /// All downloaders share one HTTP client, so TLS sessions and pooled
    /// connections are reused across searches, installs and launches.
    pub fn new() -> Self {
        static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
        let client = CLIENT.get_or_init(|| reqwest::Client::builder()
            .user_agent(concat!("mirage-launcher/", env!("CARGO_PKG_VERSION"), " (https://github.com/mukulx/mirage)"))
            .tcp_nodelay(true)
            .brotli(true)
            .gzip(true)
            .deflate(true)
            .pool_idle_timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(64)
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new()));

        Self {
            client: client.clone(),
            total_downloaded: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn download(&self, url: &str, path: &Path) -> Result<()> {
        self.download_with_sha1(url, path, None).await
    }

    pub async fn download_with_progress(
        &self,
        url: &str,
        path: &Path,
        expected_sha1: Option<&str>,
        title: &str,
    ) -> Result<()> {
        let prog = progress();
        prog.files_total.fetch_add(1, Ordering::Relaxed);
        prog.set_label(title);
        let r = self.download_with_progress_inner(url, path, expected_sha1, title).await;
        prog.files.fetch_add(1, Ordering::Relaxed);
        r
    }

    async fn download_with_progress_inner(
        &self,
        url: &str,
        path: &Path,
        expected_sha1: Option<&str>,
        title: &str,
    ) -> Result<()> {
        if path.exists() {
            if let Some(expected) = expected_sha1 {
                if verify_file_sha1(path, expected) {
                    return Ok(());
                }
                let _ = std::fs::remove_file(path);
            } else if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 0 {
                    return Ok(());
                }
                let _ = std::fs::remove_file(path);
            }
        }

        // Check global cache
        if let Some(expected) = expected_sha1 {
            let cache_file = global_cache_dir().join("objects").join(&expected[..2]).join(expected);
            if cache_file.exists()
                && verify_file_sha1(&cache_file, expected)
                && link_or_copy(&cache_file, path).is_ok()
            {
                return Ok(());
            }
        }

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to GET {}", url))?;

        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }

        let quiet = crate::term::Term::is_quiet();
        let content_length = resp.content_length();
        if let Some(len) = content_length {
            progress().bytes_total.fetch_add(len, Ordering::Relaxed);
            progress().sized.fetch_add(1, Ordering::Relaxed);
        }
        let pb = if quiet {
            None
        } else {
            content_length.map(|len| {
                let p = ProgressBar::new(len);
                p.set_style(
                    ProgressStyle::with_template(
                        "  :: {msg:<28} [{bar:24.yellow/blue}] {bytes:>8}/{total_bytes:<8} ({percent:>3}%) {eta:>5}",
                    )
                    .unwrap()
                    .progress_chars(" C•"),
                );
                p.set_message(title.to_string());
                p
            })
        };

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), uuid::Uuid::new_v4().simple()));
        let file = tokio::fs::File::create(&tmp).await?;
        let mut writer = tokio::io::BufWriter::with_capacity(128 * 1024, file);

        let mut hasher = Sha1::new();
        let mut stream = resp;

        while let Some(chunk) = stream.chunk().await? {
            writer.write_all(&chunk).await?;
            if expected_sha1.is_some() {
                hasher.update(&chunk);
            }
            if let Some(p) = &pb {
                p.inc(chunk.len() as u64);
            }
            self.total_downloaded
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            progress().bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        writer.flush().await?;
        drop(writer);

        if let Some(p) = pb {
            p.finish_and_clear();
        }

        if let Some(expected) = expected_sha1 {
            let actual = format!("{:x}", hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                let _ = tokio::fs::remove_file(&tmp).await;
                bail!(
                    "SHA-1 mismatch for {}. Expected {}, got {}",
                    path.display(),
                    expected,
                    actual
                );
            }
        }

        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    pub async fn download_with_sha1(
        &self,
        url: &str,
        path: &Path,
        expected_sha1: Option<&str>,
    ) -> Result<()> {
        if path.exists() {
            if let Some(expected) = expected_sha1 {
                if verify_file_sha1(path, expected) {
                    return Ok(());
                }
                let _ = std::fs::remove_file(path);
            } else if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 0 {
                    return Ok(());
                }
                let _ = std::fs::remove_file(path);
            }
        }

        // Check global cache if sha1 is known
        if let Some(expected) = expected_sha1 {
            let cache_file = global_cache_dir().join("objects").join(&expected[..2]).join(expected);
            if cache_file.exists()
                && verify_file_sha1(&cache_file, expected)
                && link_or_copy(&cache_file, path).is_ok()
            {
                return Ok(());
            }
        }

        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let mut attempts = 0;
        let max_attempts = 3;

        loop {
            attempts += 1;
            match self.try_download_file(url, path, expected_sha1).await {
                Ok(_) => {
                    // Populate global cache if sha1 is known
                    if let Some(expected) = expected_sha1 {
                        let cache_file = global_cache_dir().join("objects").join(&expected[..2]).join(expected);
                        if let Some(p) = cache_file.parent() {
                            let _ = tokio::fs::create_dir_all(p).await;
                        }
                        let _ = link_or_copy(path, &cache_file);
                    }
                    return Ok(());
                }
                Err(_e) if attempts < max_attempts => {
                    tokio::time::sleep(std::time::Duration::from_millis(150 * attempts as u64)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn try_download_file(
        &self,
        url: &str,
        path: &Path,
        expected_sha1: Option<&str>,
    ) -> Result<()> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to GET {}", url))?;

        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), uuid::Uuid::new_v4().simple()));
        let file = tokio::fs::File::create(&tmp).await?;
        let mut writer = tokio::io::BufWriter::with_capacity(128 * 1024, file);

        let mut hasher = Sha1::new();
        let mut stream = resp;

        while let Some(chunk) = stream.chunk().await? {
            writer.write_all(&chunk).await?;
            if expected_sha1.is_some() {
                hasher.update(&chunk);
            }
            self.total_downloaded
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            progress().bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        writer.flush().await?;
        drop(writer);

        if let Some(expected) = expected_sha1 {
            let actual = format!("{:x}", hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                let _ = tokio::fs::remove_file(&tmp).await;
                bail!(
                    "SHA-1 mismatch for {}. Expected {}, got {}",
                    path.display(),
                    expected,
                    actual
                );
            }
        }

        tokio::fs::rename(&tmp, path)
            .await
            .with_context(|| format!("Failed to move temp file to {}", path.display()))?;

        Ok(())
    }

    /// True when `url` answers at all (any HTTP status): a cheap "are we
    /// online" probe that never downloads a body.
    pub async fn reachable(&self, url: &str) -> bool {
        self.client.head(url).timeout(std::time::Duration::from_secs(4)).send().await.is_ok()
    }

    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to GET {}", url))?;

        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }

        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let resp = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("Failed to POST {}", url))?;
        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }
        resp.json().await.with_context(|| format!("Failed to deserialize JSON from {}", url))
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let bytes = self.download_bytes(url).await?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("Failed to deserialize JSON from {}", url))
    }

    pub async fn download_many(
        &self,
        items: Vec<(String, PathBuf, Option<String>)>,
        title: Option<&str>,
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        // Fast check against local disk & global cache
        let mut pending = Vec::new();
        for (url, dest, sha) in items {
            if dest.exists() {
                if let Some(expected) = &sha {
                    if let Ok(meta) = std::fs::metadata(&dest) {
                        if meta.len() > 0 && verify_file_sha1(&dest, expected) {
                            continue;
                        }
                    }
                } else if let Ok(meta) = std::fs::metadata(&dest) {
                    if meta.len() > 0 {
                        continue;
                    }
                }
            }

            // Check global cache hardlink
            if let Some(expected) = &sha {
                let cache_file = global_cache_dir().join("objects").join(&expected[..2]).join(expected);
                if cache_file.exists()
                    && verify_file_sha1(&cache_file, expected)
                    && link_or_copy(&cache_file, &dest).is_ok()
                {
                    continue;
                }
            }

            pending.push((url, dest, sha));
        }

        if pending.is_empty() {
            return Ok(());
        }

        let total = pending.len() as u64;
        let initial_label = title.unwrap_or("retrieving packages");
        progress().files_total.fetch_add(total, Ordering::Relaxed);
        progress().set_label(initial_label);
        let quiet = crate::term::Term::is_quiet();

        // In TUI mode indicatif would corrupt the alternate screen: use a
        // hidden bar and let the TUI show its own animated progress.
        let pb = ProgressBar::hidden();
        if !quiet {
            pb.set_length(total);
            pb.set_style(
                ProgressStyle::with_template(
                    "  :: {prefix} ({pos:>3}/{len:<3}) [{bar:24.yellow/blue}] {percent:>3}% {eta:>5}",
                )
                .unwrap()
                .progress_chars(" C•"),
            );
            pb.set_prefix(initial_label.to_string());
        }

        let concurrency = 48;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let finished_count = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::with_capacity(pending.len());
        for (url, path, sha) in pending {
            let permit = semaphore.clone();
            let downloader = self.clone();
            let pb_clone = pb.clone();
            let count = finished_count.clone();

            tasks.push(tokio::spawn(async move {
                let _p = permit.acquire().await.unwrap();
                let res = downloader.download_with_sha1(&url, &path, sha.as_deref()).await;
                count.fetch_add(1, Ordering::Relaxed);
                progress().files.fetch_add(1, Ordering::Relaxed);
                pb_clone.inc(1);
                res
            }));
        }

        let mut first_error = None;
        for task in tasks {
            match task.await {
                Ok(Err(e)) if first_error.is_none() => first_error = Some(e),
                Err(e) if first_error.is_none() => {
                    first_error = Some(anyhow::anyhow!("Download task panicked: {}", e));
                }
                _ => {}
            }
        }

        pb.finish_and_clear();

        if let Some(e) = first_error {
            return Err(e);
        }

        if !crate::term::Term::is_quiet() {
            println!("  :: Integrity verified for {} packages (SHA-1).", total);
        }
        Ok(())
    }
}

/// Lower-case hex SHA-1 of a file, or `None` if it cannot be read.
pub fn file_sha1(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

fn verify_file_sha1(path: &Path, expected: &str) -> bool {
    file_sha1(path).is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
}
