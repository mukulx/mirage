use anyhow::{bail, Context, Result};
use crossterm::style::Stylize;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MinecraftAccount {
    pub access_token: String,
    pub username: String,
    pub uuid: String,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

impl MinecraftAccount {
    /// True if this Microsoft session should be considered expired.
    /// 5 min clock-skew buffer included.
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            None => true, // legacy account from before expiry tracking -> force refresh/re-login
            Some(exp) => chrono::Utc::now().timestamp() >= exp - 300,
        }
    }

    pub fn expiry_label(&self) -> String {
        match self.expires_at {
            None => "unknown (legacy, re-login advised)".to_string(),
            Some(exp) => {
                let now = chrono::Utc::now().timestamp();
                if exp <= now {
                    "EXPIRED".to_string()
                } else {
                    let left = exp - now;
                    if left > 3600 {
                        format!("valid for ~{}h", left / 3600)
                    } else if left > 60 {
                        format!("valid for ~{}m", left / 60)
                    } else {
                        format!("valid for ~{}s", left)
                    }
                }
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AccountStore {
    pub active: Option<String>,
    pub accounts: Vec<MinecraftAccount>,
}

fn data_dir() -> PathBuf {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(v).join("mirage")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".local/share/mirage")
    }
}

fn accounts_path() -> PathBuf {
    let d = data_dir();
    fs::create_dir_all(&d).ok();
    d.join("accounts.json")
}

pub fn load_store() -> AccountStore {
    let p = accounts_path();
    if let Ok(data) = fs::read_to_string(&p) {
        if let Some((store, dropped)) = parse_store(&data) {
            if dropped {
                let _ = save_store(&store);
            }
            return store;
        }
    }
    // Backward compatibility with legacy tokens.json
    let legacy = data_dir().join("tokens.json");
    if legacy.exists() {
        if let Ok(data) = fs::read_to_string(&legacy) {
            #[derive(Deserialize)]
            struct LegacyAccount {
                access_token: String,
                username: String,
                uuid: String,
            }
            if let Ok(leg) = serde_json::from_str::<LegacyAccount>(&data) {
                let acc = MinecraftAccount {
                    access_token: leg.access_token,
                    username: leg.username.clone(),
                    uuid: leg.uuid,
                    expires_at: None,
                    refresh_token: None,
                };
                let store = AccountStore {
                    active: Some(leg.username),
                    accounts: vec![acc],
                };
                let _ = save_store(&store);
                return store;
            }
        }
    }
    AccountStore::default()
}

/// Parse accounts.json, dropping offline accounts written by older versions
/// (the launcher is premium-only now). Returns true if any were dropped.
fn parse_store(data: &str) -> Option<(AccountStore, bool)> {
    let mut raw: serde_json::Value = serde_json::from_str(data).ok()?;
    let mut dropped = false;
    if let Some(list) = raw.get_mut("accounts").and_then(|a| a.as_array_mut()) {
        let before = list.len();
        list.retain(|a| a.get("account_type").and_then(|t| t.as_str()) != Some("Offline"));
        dropped = list.len() != before;
    }
    let mut store: AccountStore = serde_json::from_value(raw).ok()?;
    let active_exists = store
        .active
        .as_ref()
        .is_some_and(|n| store.accounts.iter().any(|a| &a.username == n));
    if !active_exists {
        store.active = store.accounts.first().map(|a| a.username.clone());
    }
    Some((store, dropped))
}

/// Tokens live here, so write owner-only (0600) and atomically: a crash
/// mid-write must never lose the rotated refresh token.
pub fn save_store(store: &AccountStore) -> Result<()> {
    let p = accounts_path();
    let tmp = p.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(store)?;
    {
        use std::io::Write;
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &p).with_context(|| format!("Failed to save {}", p.display()))?;
    Ok(())
}

pub fn load_account() -> Option<MinecraftAccount> {
    let store = load_store();
    if let Some(active_name) = &store.active {
        if let Some(acc) = store.accounts.iter().find(|a| &a.username == active_name) {
            return Some(acc.clone());
        }
    }
    store.accounts.first().cloned()
}

pub fn save_account(acc: &MinecraftAccount) -> Result<()> {
    let mut store = load_store();
    // Match by UUID: a Minecraft name change must update the entry, not duplicate it.
    if let Some(idx) = store.accounts.iter().position(|a| a.uuid == acc.uuid) {
        store.accounts[idx] = acc.clone();
    } else {
        store.accounts.push(acc.clone());
    }
    store.active = Some(acc.username.clone());
    save_store(&store)
}

pub fn set_active_account(username: &str) -> Result<()> {
    let mut store = load_store();
    if store.accounts.iter().any(|a| a.username == username) {
        store.active = Some(username.to_string());
        save_store(&store)?;
        Ok(())
    } else {
        bail!("Account '{}' not found", username);
    }
}

pub fn delete_account(username: Option<&str>) -> Result<()> {
    let mut store = load_store();
    if let Some(name) = username {
        store.accounts.retain(|a| a.username != name);
        if store.active.as_deref() == Some(name) {
            store.active = store.accounts.first().map(|a| a.username.clone());
        }
    } else {
        store.accounts.clear();
        store.active = None;
    }
    save_store(&store)?;
    let legacy = data_dir().join("tokens.json");
    if legacy.exists() {
        let _ = fs::remove_file(legacy);
    }
    Ok(())
}

pub fn copy_to_clipboard(text: &str) -> bool {
    // OSC 52 terminal clipboard escape sequence (Supported by modern terminals, Kitty, Ghostty, Alacritty, WezTerm, Foot, VS Code, iTerm, tmux, etc.)
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    print!("\x1b]52;c;{}\x07", b64);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    copy_system_clipboard(text)
}

/// System clipboard binaries only (Wayland / X11 / macOS / WSL). Writes
/// nothing to the terminal, so it is safe while the TUI owns the screen.
pub fn copy_system_clipboard(text: &str) -> bool {
    let mut copied = false;
    let clip_commands: [(&str, &[&str]); 6] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("xsel", &["-b"]),
        ("pbcopy", &[]),
        ("clip.exe", &[]),
    ];

    for (cmd, args) in clip_commands {
        if let Ok(mut child) = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if let Ok(status) = child.wait() {
                if status.success() {
                    copied = true;
                    break;
                }
            }
        }
    }

    copied
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("mirage-launcher/", env!("CARGO_PKG_VERSION"), " (https://github.com/mukulx/mirage)"))
        .build()?)
}

/// A pending Microsoft device-code sign-in.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
}

/// How long a device code stays valid for us.
pub const LOGIN_TIMEOUT_SECS: u64 = 300;

/// Step 1: ask Microsoft for a code the user types in the browser.
pub async fn request_device_code(client: &reqwest::Client) -> Result<DeviceCode> {
    #[derive(Deserialize)]
    struct Raw {
        user_code: String,
        device_code: String,
        verification_uri: Option<String>,
        interval: Option<u64>,
    }
    let dc: Raw = client
        .post("https://login.live.com/oauth20_connect.srf")
        .form(&[
            ("client_id", "00000000402B5328"),
            ("scope", "XboxLive.signin offline_access"),
            ("response_type", "device_code"),
        ])
        .send()
        .await
        .context("Failed to contact Microsoft login service")?
        .json()
        .await
        .context("Failed to parse device code response")?;
    Ok(DeviceCode {
        user_code: dc.user_code,
        device_code: dc.device_code,
        verification_uri: dc.verification_uri.unwrap_or_else(|| "https://www.microsoft.com/link".into()),
        interval: dc.interval.unwrap_or(5),
    })
}

/// Step 2: wait until the user approves the code in the browser.
pub async fn poll_device_code(client: &reqwest::Client, dc: &DeviceCode) -> Result<MsaTokens> {
    let start_time = std::time::Instant::now();
    loop {
        if start_time.elapsed().as_secs() > LOGIN_TIMEOUT_SECS {
            bail!("Login timed out after 5 minutes.");
        }

        tokio::time::sleep(std::time::Duration::from_secs(dc.interval.max(2))).await;

        let resp = client
            .post("https://login.live.com/oauth20_token.srf")
            .form(&[
                ("client_id", "00000000402B5328"),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &dc.device_code),
            ])
            .send()
            .await?;

        let text = resp.text().await?;

        if let Some(tokens) = parse_msa_token_response(&text) {
            return Ok(tokens);
        }

        // Not a token yet -> must be a pending/error state
        let (is_pending, is_expired, err_msg) = msa_poll_error(&text);
        if is_pending {
            continue;
        }
        if is_expired {
            bail!("Login code expired. Run login again for a fresh code.");
        }
        if !err_msg.is_empty() {
            bail!("Auth error: {}", err_msg);
        }
    }
}

/// Steps 3-6 (shared with refresh): Xbox, XSTS and Minecraft profile, then save.
pub async fn finish_login(client: &reqwest::Client, msa: MsaTokens) -> Result<MinecraftAccount> {
    let (mc_token, mc_expires_in, profile) =
        exchange_msa_for_minecraft(client, &msa.access_token).await?;

    let account = MinecraftAccount {
        access_token: mc_token,
        username: profile.name,
        uuid: profile.id,
        expires_at: Some(chrono::Utc::now().timestamp() + mc_expires_in),
        refresh_token: msa.refresh_token,
    };

    save_account(&account)?;
    Ok(account)
}

/// Plain-terminal sign-in for the CLI: prints the code and waits.
pub async fn login_microsoft() -> Result<MinecraftAccount> {
    let client = http_client()?;
    let dc = request_device_code(&client).await?;
    let copied = copy_to_clipboard(&dc.user_code);

    println!(
        "\n  {}  {}",
        "▸".with(crate::term::Theme::INFO).bold(),
        "Go to:".bold()
    );
    println!("     {}", dc.verification_uri.as_str().with(crate::term::Theme::BRIGHT));
    println!(
        "  {}  {}",
        "▸".with(crate::term::Theme::INFO).bold(),
        "Enter Code:".bold()
    );
    println!(
        "     {}   {}",
        dc.user_code.clone().with(crate::term::Theme::WARN).bold(),
        if copied { "✔ (Code copied to clipboard!)" } else { "✔ (Copied to terminal clipboard!)" }
            .with(crate::term::Theme::GREEN)
            .bold()
    );
    println!(
        "  {}  {}",
        "⏳".with(crate::term::Theme::WARN),
        "Waiting for browser authorization...".with(crate::term::Theme::MUTED)
    );

    open_in_browser(&dc.verification_uri);

    let msa = poll_device_code(&client, &dc).await?;
    let account = finish_login(&client, msa).await?;
    crate::term::Term::done(&format!("Logged in as {}", account.username.clone().bold()));
    Ok(account)
}

pub fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

pub async fn login() -> Result<MinecraftAccount> {
    login_microsoft().await
}

#[derive(Debug, Clone)]
pub struct MsaTokens {
    access_token: String,
    refresh_token: Option<String>,
}

/// Parse an MSA token endpoint body (JSON or form-urlencoded).
/// Returns Some on success, None if this body is not a token (e.g. pending).
fn parse_msa_token_response(text: &str) -> Option<MsaTokens> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(token) = json["access_token"].as_str() {
            let refresh = json["refresh_token"].as_str().map(|s| s.to_string());
            return Some(MsaTokens {
                access_token: token.to_string(),
                refresh_token: refresh,
            });
        }
        // fall through to error handling by caller if no access_token
    }
    let params: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(text.as_bytes())
            .into_owned()
            .collect();
    if let Some(token) = params.get("access_token") {
        return Some(MsaTokens {
            access_token: token.clone(),
            refresh_token: params.get("refresh_token").cloned(),
        });
    }
    None
}

fn msa_poll_error(text: &str) -> (bool, bool, String) {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        let error = json["error"].as_str().unwrap_or("").to_string();
        return (
            error == "authorization_pending",
            error == "expired_token",
            error,
        );
    }
    let params: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(text.as_bytes())
            .into_owned()
            .collect();
    let error = params.get("error").cloned().unwrap_or_default();
    (
        error == "authorization_pending",
        error == "expired_token",
        error,
    )
}

fn default_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("mirage-launcher/", env!("CARGO_PKG_VERSION"), " (https://github.com/mukulx/mirage)"))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")
}

/// Full XBL -> XSTS -> Minecraft exchange for a fresh MSA access token.
/// Returns (minecraft_access_token, expires_in_secs, profile).
async fn exchange_msa_for_minecraft(
    client: &reqwest::Client,
    msa_access_token: &str,
) -> Result<(String, i64, Profile)> {
    // Step 3: Xbox Live authentication
    let xbl_resp = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .json(&serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={}", msa_access_token)
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        }))
        .send()
        .await
        .context("Xbox Live authentication failed")?;

    if !xbl_resp.status().is_success() {
        let body = xbl_resp.text().await.unwrap_or_default();
        bail!("Xbox Live auth failed: {}", snippet(&body));
    }
    let xbl: XboxResponse = xbl_resp
        .json()
        .await
        .context("Failed to parse Xbox Live response")?;

    // Step 4: XSTS authorization (friendly errors for common XErr codes)
    let xsts_resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .json(&serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl.token]
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }))
        .send()
        .await
        .context("XSTS authorization failed")?;

    if !xsts_resp.status().is_success() {
        let body = xsts_resp.text().await.unwrap_or_default();
        bail!("Minecraft authorization failed: {}", xsts_friendly_error(&body));
    }
    let xsts: XboxResponse = xsts_resp
        .json()
        .await
        .context("Failed to parse XSTS response")?;

    let uhs = xsts
        .display_claims
        .xui
        .first()
        .map(|u| u.uhs.clone())
        .unwrap_or_default();
    if uhs.is_empty() || xsts.token.is_empty() {
        bail!("XSTS response missing user hash or token");
    }

    // Step 5: Minecraft services login
    let mc_resp = client
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .json(&serde_json::json!({
            "identityToken": format!("XBL3.0 x={};{}", uhs, xsts.token)
        }))
        .send()
        .await
        .context("Minecraft Services login failed")?;

    if !mc_resp.status().is_success() {
        let body = mc_resp.text().await.unwrap_or_default();
        bail!("Minecraft login failed: {}", snippet(&body));
    }
    let mc: McToken = mc_resp
        .json()
        .await
        .context("Failed to parse Minecraft token response")?;

    let expires_in = mc.expires_in.unwrap_or(86400);

    // Step 6: profile (also proves the account owns Minecraft)
    let profile = fetch_profile(client, &mc.access_token).await?;

    Ok((mc.access_token, expires_in, profile))
}

async fn fetch_profile(client: &reqwest::Client, mc_access_token: &str) -> Result<Profile> {
    let resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .header("Authorization", format!("Bearer {}", mc_access_token))
        .send()
        .await
        .context("Failed to fetch Minecraft profile")?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("INVALID_SESSION");
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("This Microsoft account does not own Minecraft (profile 404).");
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("Profile fetch failed (HTTP {}): {}", status, snippet(&body));
    }
    resp.json::<Profile>()
        .await
        .context("Failed to parse Minecraft profile (Does this account own Minecraft?)")
}

fn snippet(s: &str) -> String {
    const MAX: usize = 300;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}...", &s[..MAX])
    }
}

fn xsts_friendly_error(body: &str) -> String {
    // XSTS returns {"XErr": 2148000936, "Message": "..."} on failure
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let code = v
            .get("XErr")
            .and_then(|c| c.as_u64().or_else(|| c.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0);
        match code {
            2148000936 => return "No Xbox account linked to this Microsoft account.".into(),
            2148000933 => return "Child account needs family approval for online play (https://aka.ms/acctsettings).".into(),
            2148000928 => return "Xbox Live is banned in this region.".into(),
            2148000875 => return "Xbox Live sign-in blocked - check account security.".into(),
            2148000975 => return "Unverified account - verify email at https://aka.ms/verify.".into(),
            0 => {}
            _ => return format!("XSTS error {}: {}", code, snippet(body)),
        }
    }
    snippet(body)
}

/// Try to refresh a Microsoft account using its stored refresh_token.
/// On success the store is updated and the fresh account returned.
pub async fn refresh_microsoft_account(account: &MinecraftAccount) -> Result<MinecraftAccount> {
    let refresh = account.refresh_token.clone().unwrap_or_default();
    if refresh.is_empty() {
        bail!(
            "Session expired and no refresh token exists for '{}'. Please run `account login` again.",
            account.username
        );
    }

    let client = default_http_client()?;

    let resp = client
        .post("https://login.live.com/oauth20_token.srf")
        .form(&[
            ("client_id", "00000000402B5328"),
            ("scope", "XboxLive.signin offline_access"),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
        ])
        .send()
        .await
        .context("Failed to contact Microsoft (check your network; your login is kept)")?;

    let text = resp.text().await.unwrap_or_default();

    let msa = match parse_msa_token_response(&text) {
        Some(t) => t,
        None => {
            let (_, _, err) = msa_poll_error(&text);
            if err == "invalid_grant" || err.contains("invalid") {
                bail!(
                    "Microsoft refresh token rejected for '{}' (revoked/expired). Please run `account login` again.",
                    account.username
                );
            }
            if !err.is_empty() {
                bail!("Token refresh failed: {}", err);
            }
            bail!("Token refresh failed: {}", snippet(&text));
        }
    };

    let (mc_token, mc_expires_in, profile) =
        exchange_msa_for_minecraft(&client, &msa.access_token).await?;

    // Keep rotating refresh token; fall back to old one if endpoint didn't return a new one
    let new_refresh = msa.refresh_token.or_else(|| account.refresh_token.clone());

    let fresh = MinecraftAccount {
        access_token: mc_token,
        username: profile.name,
        uuid: profile.id,
        expires_at: Some(chrono::Utc::now().timestamp() + mc_expires_in),
        refresh_token: new_refresh,
    };
    save_account(&fresh)?;
    Ok(fresh)
}

/// Returns true if the stored Minecraft token is still accepted by Mojang.
/// Network errors return Err (unknown), 401/403 returns Ok(false).
pub async fn is_session_valid(account: &MinecraftAccount) -> Result<bool> {
    let client = default_http_client()?;
    match fetch_profile(&client, &account.access_token).await {
        Ok(_) => Ok(true),
        Err(e) => {
            if e.to_string().contains("INVALID_SESSION") {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Guarantee the account can join online servers:
/// - expired OR rejected by profile endpoint -> auto-refresh once
/// - refresh failure -> clear actionable error telling user to re-login
///
/// Never prints, so the TUI can call it without corrupting the screen. The
/// optional note says what happened (refreshed, or could not verify).
pub async fn revalidate_account(
    account: &MinecraftAccount,
) -> Result<(MinecraftAccount, Option<String>)> {
    if !account.is_expired() {
        match is_session_valid(account).await {
            Ok(true) => return Ok((account.clone(), None)),
            Ok(false) => {}
            // Transient network/profile error: keep the stored session so
            // single-player still works. Only refresh on definite rejection or expiry.
            Err(e) => {
                return Ok((account.clone(), Some(format!("Could not verify session: {}", e))))
            }
        }
    }

    match refresh_microsoft_account(account).await {
        Ok(fresh) => {
            let note = format!("Session refreshed for {}", fresh.username);
            Ok((fresh, Some(note)))
        }
        Err(e) => Err(e.context(format!("Could not refresh session for '{}'", account.username))),
    }
}

/// Launch fast path: trust an unexpired token instead of a network round
/// trip on every launch (startup and `account status` still verify with
/// Mojang). Expired tokens are refreshed as usual.
pub async fn account_for_launch(account: &MinecraftAccount) -> Result<MinecraftAccount> {
    if !account.is_expired() {
        return Ok(account.clone());
    }
    match revalidate_account(account).await {
        Ok((fresh, _)) => Ok(fresh),
        // No network: launch with the saved session so single-player works.
        Err(e) if is_network_error(&e) => Ok(account.clone()),
        Err(e) => Err(e),
    }
}

/// True when `e` never reached the server (offline, DNS, timeout), as
/// opposed to the server rejecting the request.
fn is_network_error(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<reqwest::Error>().is_some_and(|r| r.is_connect() || r.is_timeout()))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxResponse {
    token: String,
    display_claims: XboxDisplayClaims,
}

#[derive(Deserialize)]
struct XboxDisplayClaims {
    xui: Vec<XboxUser>,
}

#[derive(Deserialize)]
struct XboxUser {
    uhs: String,
}

#[derive(Deserialize)]
struct McToken {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Deserialize, Clone, Debug)]
struct Profile {
    id: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_microsoft_expiry_logic() {
        let now = chrono::Utc::now().timestamp();
        let fresh = MinecraftAccount {
            access_token: "x".into(),
            username: "u".into(),
            uuid: "u".into(),
            expires_at: Some(now + 3600),
            refresh_token: Some("r".into()),
        };
        assert!(!fresh.is_expired());

        let stale = MinecraftAccount {
            expires_at: Some(now - 10),
            ..fresh.clone()
        };
        assert!(stale.is_expired());

        let legacy = MinecraftAccount {
            expires_at: None,
            refresh_token: None,
            ..fresh
        };
        assert!(legacy.is_expired());
    }

    #[test]
    fn test_legacy_account_deserializes_with_default_refresh() {
        let json = r#"{"access_token":"tok","username":"Steve","uuid":"abc","expires_at":123}"#;
        let acc: MinecraftAccount = serde_json::from_str(json).unwrap();
        assert_eq!(acc.username, "Steve");
        assert!(acc.refresh_token.is_none());
        assert!(acc.is_expired());
    }

    #[test]
    fn test_parse_store_drops_offline_accounts() {
        let json = r#"{"active":"Steve","accounts":[
            {"account_type":"Offline","access_token":"0","username":"Steve","uuid":"s","expires_at":null},
            {"account_type":"Microsoft","access_token":"t","username":"Alex","uuid":"a","expires_at":1,"refresh_token":"r"}
        ]}"#;
        let (store, dropped) = parse_store(json).unwrap();
        assert!(dropped);
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].username, "Alex");
        assert_eq!(store.active.as_deref(), Some("Alex"));

        let (_, dropped) = parse_store(r#"{"active":null,"accounts":[]}"#).unwrap();
        assert!(!dropped);
    }

    #[test]
    fn test_parse_msa_token_json_and_form() {
        let j = r#"{"access_token":"a123","refresh_token":"r123","expires_in":86400}"#;
        let t = parse_msa_token_response(j).unwrap();
        assert_eq!(t.access_token, "a123");
        assert_eq!(t.refresh_token.as_deref(), Some("r123"));

        let f = "access_token=a456&refresh_token=r456&expires_in=86400";
        let t2 = parse_msa_token_response(f).unwrap();
        assert_eq!(t2.access_token, "a456");
        assert_eq!(t2.refresh_token.as_deref(), Some("r456"));

        assert!(parse_msa_token_response(r#"{"error":"authorization_pending"}"#).is_none());
    }
}
