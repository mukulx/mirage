//! Mirage Launcher terminal UI.
//!
//! Six tabs over the launcher core: instances, mods, Modrinth search,
//! modpacks, the new-instance wizard and settings. Rendering never touches
//! the disk or the network — everything slow runs in a background task and
//! reports back through a channel — so the 40 ms tick stays smooth even
//! while a modpack is downloading.
//!
//! Configure via env: `MIRAGE_NO_ANIM=1`, `MIRAGE_NO_MOUSE=1`,
//! `MIRAGE_TICK_MS=40`, `MIRAGE_THEME=<default|nord|catppuccin|synthwave>`.

use crate::auth::{self, MinecraftAccount};
use crate::crash;
use crate::instance::{Instance, InstalledMod};
use crate::launcher::{self, Launcher};
use crate::download;
use crate::modpack::{self, IdentifiedMod, ModHit, ModrinthVersion};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
    },
    DefaultTerminal, Frame,
};
use ratatui_image::{picker::Picker as ImgPicker, protocol::Protocol, Image, Resize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::io;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

// ── palette ────────────────────────────────────────────────────────────────
// ── themes: one small registry, default only for now ─────────────────────────
// New themes plug in as one `Theme` value + one `all()` entry + one `load()`
// arm. Select with `MIRAGE_THEME=<name>` (unknown names fall back to default).
#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    /// Screen background.
    pub bg: Color,
    /// Panel (card) background, one step lighter than `bg`.
    pub surface: Color,
    pub fg: Color,
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub cyan: Color,
    pub warn: Color,
    pub error: Color,
    pub muted: Color,
    pub border: Color,
    pub border_hi: Color,
    pub bg_sel: Color,
}

impl Theme {
    /// Calm graphite with Modrinth green.
    pub const DEFAULT: Self = Self {
        name: "default",
        bg: Color::Rgb(16, 18, 22),
        surface: Color::Rgb(23, 26, 31),
        fg: Color::Rgb(214, 219, 226),
        primary: Color::Rgb(246, 248, 250),
        secondary: Color::Rgb(146, 154, 166),
        accent: Color::Rgb(27, 217, 106),
        cyan: Color::Rgb(90, 180, 255),
        warn: Color::Rgb(242, 184, 75),
        error: Color::Rgb(242, 95, 92),
        muted: Color::Rgb(92, 100, 112),
        border: Color::Rgb(38, 42, 50),
        border_hi: Color::Rgb(27, 217, 106),
        bg_sel: Color::Rgb(34, 39, 47),
    };

    pub const NORD: Self = Self {
        name: "nord",
        bg: Color::Rgb(36, 41, 51),
        surface: Color::Rgb(46, 52, 64),
        fg: Color::Rgb(216, 222, 233),
        primary: Color::Rgb(236, 239, 244),
        secondary: Color::Rgb(148, 161, 179),
        accent: Color::Rgb(163, 190, 140),
        cyan: Color::Rgb(136, 192, 208),
        warn: Color::Rgb(235, 203, 139),
        error: Color::Rgb(191, 97, 106),
        muted: Color::Rgb(106, 117, 140),
        border: Color::Rgb(59, 66, 82),
        border_hi: Color::Rgb(163, 190, 140),
        bg_sel: Color::Rgb(59, 66, 82),
    };

    pub const CATPPUCCIN: Self = Self {
        name: "catppuccin",
        bg: Color::Rgb(17, 17, 27),
        surface: Color::Rgb(30, 30, 46),
        fg: Color::Rgb(205, 214, 244),
        primary: Color::Rgb(245, 247, 255),
        secondary: Color::Rgb(147, 153, 178),
        accent: Color::Rgb(166, 227, 161),
        cyan: Color::Rgb(137, 180, 250),
        warn: Color::Rgb(249, 226, 175),
        error: Color::Rgb(243, 139, 168),
        muted: Color::Rgb(108, 112, 134),
        border: Color::Rgb(49, 50, 68),
        border_hi: Color::Rgb(203, 166, 247),
        bg_sel: Color::Rgb(49, 50, 68),
    };

    pub const SYNTHWAVE: Self = Self {
        name: "synthwave",
        bg: Color::Rgb(20, 12, 36),
        surface: Color::Rgb(30, 19, 52),
        fg: Color::Rgb(241, 233, 255),
        primary: Color::Rgb(255, 255, 255),
        secondary: Color::Rgb(180, 160, 210),
        accent: Color::Rgb(255, 56, 172),
        cyan: Color::Rgb(54, 249, 246),
        warn: Color::Rgb(254, 222, 93),
        error: Color::Rgb(254, 68, 80),
        muted: Color::Rgb(120, 100, 150),
        border: Color::Rgb(56, 38, 88),
        border_hi: Color::Rgb(255, 56, 172),
        bg_sel: Color::Rgb(52, 34, 84),
    };

    /// Every available theme name, in cycling order.
    pub fn all() -> [&'static str; 4] {
        ["default", "nord", "catppuccin", "synthwave"]
    }

    /// Look up by name; anything unknown falls back to default.
    pub fn load(name: &str) -> Self {
        match name.trim().to_lowercase().as_str() {
            "nord" => Self::NORD,
            "catppuccin" => Self::CATPPUCCIN,
            "synthwave" => Self::SYNTHWAVE,
            _ => Self::DEFAULT,
        }
    }

    /// Cycle to the next theme, persisting nothing — purely in-session.
    pub fn next(&self) -> (usize, Self) {
        let names = Self::all();
        let cur = names.iter().position(|n| *n == self.name).unwrap_or(0);
        let idx = (cur + 1) % names.len();
        (idx, Self::load(names[idx]))
    }

    pub fn from_env() -> Self {
        Self::load(&std::env::var("MIRAGE_THEME").unwrap_or_default())
    }
}

// ── animation ──────────────────────────────────────────────────────────────
// Every animated value is derived from one frame counter, so there is no
// per-widget timer state to leak and nothing to resynchronise.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Blend two true-colour values; anything else falls through to `b` so
/// terminals without RGB simply skip the transition.
fn lerp_rgb(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let t = t.clamp(0.0, 1.0);
            Color::Rgb(
                lerp(ar as f32, br as f32, t) as u8,
                lerp(ag as f32, bg as f32, t) as u8,
                lerp(ab as f32, bb as f32, t) as u8,
            )
        }
        _ => b,
    }
}

/// Colour at `t` (0..1) along the theme's accent → cyan ramp.
fn ramp(th: &Theme, t: f32) -> Color {
    lerp_rgb(th.accent, th.cyan, t)
}

/// Text painted with a flowing accent → cyan gradient. `shift` scrolls the
/// gradient, so feeding it the frame clock makes the colour travel.
fn gradient_spans(th: &Theme, text: &str, shift: f32, bold: bool) -> Vec<Span<'static>> {
    let n = text.chars().count().max(1) as f32;
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let t = ((i as f32 / n - shift) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
            let st = Style::default().fg(ramp(th, t));
            Span::styled(c.to_string(), if bold { st.bold() } else { st })
        })
        .collect()
}

/// `━━━━━━────  67%` meter with a gradient fill. `grow` (0..1) animates the
/// bar filling in; the percentage always shows the real value.
fn gauge_spans(th: &Theme, frac: f32, grow: f32, width: usize) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * grow.clamp(0.0, 1.0) * width as f32).round() as usize;
    let mut v: Vec<Span> = (0..width)
        .map(|i| {
            if i < filled {
                Span::styled("━", Style::default().fg(ramp(th, i as f32 / width.max(1) as f32)))
            } else {
                Span::styled("─", Style::default().fg(th.border))
            }
        })
        .collect();
    v.push(Span::styled(format!(" {:>3}%", (frac * 100.0).round() as u32), Style::default().fg(th.secondary)));
    v
}

/// Two-row block logo for the empty state.
const LOGO: [&str; 2] = [
    "█▀▄▀█ ▀█▀ █▀█ ▄▀█ █▀▀ █▀▀",
    "█ ▀ █ ▄█▄ █▀▄ █▀█ █▄█ ██▄",
];

// ── project icons ──────────────────────────────────────────────────────────
// Icons are decoded once, off the UI thread, into two fixed sizes and drawn
// with half blocks (two square pixels per cell). Works in any true-colour
// terminal — no graphics protocol needed.
const ICON_SMALL: u32 = 4; // 4 cols × 2 rows, list rows
const ICON_LARGE: u32 = 16; // 16 cols × 8 rows, detail panel

#[derive(Clone)]
struct Pixels {
    w: u32,
    h: u32,
    px: Vec<[u8; 4]>,
}

pub struct Icon {
    small: Pixels,
    large: Pixels,
    /// Real-pixel versions for terminals with a graphics protocol (kitty,
    /// sixel, iTerm2); `None` means draw the half-block pixels instead.
    gfx_small: Option<Protocol>,
    gfx_large: Option<Protocol>,
}

impl Icon {
    fn decode(bytes: &[u8], gfx: Option<&ImgPicker>) -> Option<Self> {
        let img = image::load_from_memory(bytes).ok()?;
        // Lanczos keeps edges crisp at these tiny sizes.
        Some(Self::from_image(img, gfx, image::imageops::FilterType::Lanczos3))
    }

    /// A player's head from a skin of any standard size (64x32, 64x64, or
    /// HD multiples like 128x128): face plus hat layer. Like the game, the
    /// face is drawn opaque and only the hat layer may be see-through.
    fn decode_head(bytes: &[u8], gfx: Option<&ImgPicker>) -> Option<Self> {
        use image::imageops::{crop_imm, overlay};
        let skin = image::load_from_memory(bytes).ok()?.to_rgba8();
        let s = skin.width() / 64;
        if s == 0 || skin.width() % 64 != 0 || skin.height() < 16 * s {
            return None;
        }
        let mut head = crop_imm(&skin, 8 * s, 8 * s, 8 * s, 8 * s).to_image();
        head.pixels_mut().for_each(|p| p.0[3] = 255);
        overlay(&mut head, &crop_imm(&skin, 40 * s, 8 * s, 8 * s, 8 * s).to_image(), 0, 0);
        Some(Self::from_head(head, gfx))
    }

    /// Icon from a square head image. Pixel art must never be smoothed: the
    /// real-pixel version is blown up by whole numbers (so whatever scaling
    /// the terminal protocol does stays crisp), and the half-block grids
    /// pick nearest pixels, or average only when shrinking an HD skin.
    fn from_head(head: image::RgbaImage, gfx: Option<&ImgPicker>) -> Self {
        use image::imageops::{resize, FilterType};
        let grid = |n: u32| {
            let f = if head.width() <= n { FilterType::Nearest } else { FilterType::Triangle };
            Pixels { w: n, h: n, px: resize(&head, n, n, f).pixels().map(|p| p.0).collect() }
        };
        let big = image::DynamicImage::ImageRgba8(resize(&head, 128, 128, FilterType::Nearest));
        let proto = |n: u32| {
            let size = ratatui::layout::Size::new(n as u16, n as u16 / 2);
            gfx?.new_protocol(big.clone(), size, Resize::Scale(Some(FilterType::Nearest))).ok()
        };
        Self { small: grid(ICON_SMALL), large: grid(ICON_LARGE), gfx_small: proto(ICON_SMALL), gfx_large: proto(ICON_LARGE) }
    }

    fn from_image(img: image::DynamicImage, gfx: Option<&ImgPicker>, filter: image::imageops::FilterType) -> Self {
        let scale = |n: u32| {
            let rgba = img.resize_exact(n, n, filter).to_rgba8();
            Pixels { w: n, h: n, px: rgba.pixels().map(|p| p.0).collect() }
        };
        let proto = |n: u32| {
            let size = ratatui::layout::Size::new(n as u16, n as u16 / 2);
            gfx?.new_protocol(img.clone(), size, Resize::Scale(Some(filter))).ok()
        };
        Self {
            small: scale(ICON_SMALL),
            large: scale(ICON_LARGE),
            gfx_small: proto(ICON_SMALL),
            gfx_large: proto(ICON_LARGE),
        }
    }

    /// The player's head, via Mojang's session server. The skin is cached so
    /// the head still shows offline.
    async fn fetch_head(uuid: &str, gfx: Option<ImgPicker>) -> Option<Self> {
        let path = download::global_cache_dir().join("heads").join(uuid);
        let fresh = async {
            use base64::Engine as _;
            let dl = download::Downloader::new();
            let id = uuid.replace('-', "");
            let profile: serde_json::Value =
                serde_json::from_slice(&dl.download_bytes(&format!("https://sessionserver.mojang.com/session/minecraft/profile/{id}")).await.ok()?).ok()?;
            let value = profile["properties"].as_array()?.iter().find(|p| p["name"] == "textures")?["value"].as_str()?;
            let tex: serde_json::Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(value).ok()?).ok()?;
            let url = tex["textures"]["SKIN"]["url"].as_str()?.replacen("http://", "https://", 1);
            dl.download_bytes(&url).await.ok()
        }
        .await;
        let bytes = match fresh {
            Some(b) => {
                if let Some(dir) = path.parent() {
                    let _ = tokio::fs::create_dir_all(dir).await;
                }
                let _ = tokio::fs::write(&path, &b).await;
                b
            }
            None => tokio::fs::read(&path).await.ok()?,
        };
        tokio::task::spawn_blocking(move || Self::decode_head(&bytes, gfx.as_ref())).await.ok().flatten()
    }

    /// Fetch a project icon, via a small on-disk cache so revisits are instant.
    async fn fetch(project_id: &str, url: &str, gfx: Option<ImgPicker>) -> Option<Self> {
        static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
        let path = download::global_cache_dir().join("icons").join(project_id);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(_) => {
                let _slot = SLOTS.acquire().await.ok()?;
                let b = download::Downloader::new().download_bytes(url).await.ok()?;
                if let Some(dir) = path.parent() {
                    let _ = tokio::fs::create_dir_all(dir).await;
                }
                let _ = tokio::fs::write(&path, &b).await;
                b
            }
        };
        tokio::task::spawn_blocking(move || Self::decode(&bytes, gfx.as_ref())).await.ok().flatten()
    }
}

/// One row of half-block cells per two pixel rows. Pixels are alpha-blended
/// onto `bg` (the panel colour underneath), so soft icon edges stay smooth
/// instead of snapping to a jagged on/off mask.
fn icon_lines(p: &Pixels, bg: Color) -> Vec<Line<'static>> {
    let Color::Rgb(br, bgc, bb) = bg else { return Vec::new() };
    let blend = |c: [u8; 4]| {
        let a = c[3] as f32 / 255.0;
        let mix = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
        Color::Rgb(mix(c[0], br), mix(c[1], bgc), mix(c[2], bb))
    };
    (0..p.h / 2)
        .map(|row| {
            Line::from(
                (0..p.w)
                    .map(|x| {
                        let top = p.px[(row * 2 * p.w + x) as usize];
                        let bot = p.px[((row * 2 + 1) * p.w + x) as usize];
                        Span::styled("▀", Style::default().fg(blend(top)).bg(blend(bot)))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Stand-in for a project with no icon (or one still loading): a black tile
/// with the project's initial. The large size gets a hairline frame. `aspect`
/// is a terminal cell's height over its width, so the tile is a true square
/// on screen whatever the font; it has fewer rows than the icon slot when
/// cells are taller than 2:1.
fn icon_placeholder(th: &Theme, title: &str, cols: u32, aspect: f32) -> Vec<Line<'static>> {
    let cols_n = cols as usize;
    let rows = ((cols as f32 / aspect).round() as usize).clamp(2, cols_n / 2);
    let cols = cols_n;
    let initial = title.chars().find(|c| c.is_alphanumeric()).unwrap_or('?').to_ascii_uppercase();
    let tile = lerp_rgb(th.bg, Color::Rgb(0, 0, 0), 0.55);
    let fill = Style::default().bg(tile);
    let frame = rows > 2;
    (0..rows)
        .map(|r| {
            let (l, m, rt) = match (frame, r) {
                (false, _) => (' ', ' ', ' '),
                (true, 0) => ('╭', '─', '╮'),
                (true, r) if r == rows - 1 => ('╰', '─', '╯'),
                _ => ('│', ' ', '│'),
            };
            let inner = cols - if frame { 2 } else { 0 };
            let letter_row = if frame { rows / 2 } else { 0 };
            let body = if r == letter_row {
                let left = (inner - 1) / 2;
                Span::styled(
                    format!("{}{initial}{}", " ".repeat(left), " ".repeat(inner - left - 1)),
                    fill.fg(th.fg).bold(),
                )
            } else {
                Span::styled(m.to_string().repeat(inner), fill.fg(th.border))
            };
            if frame {
                let edge = fill.fg(th.border);
                Line::from(vec![Span::styled(l.to_string(), edge), body, Span::styled(rt.to_string(), edge)])
            } else {
                Line::from(body)
            }
        })
        .collect()
}

const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const LOADERS: [&str; 3] = ["fabric", "vanilla", "quilt"];
const FALLBACK_VERSIONS: [&str; 10] = [
    "1.21.4", "1.21.1", "1.20.6", "1.20.4", "1.20.1", "1.19.4", "1.18.2", "1.16.5", "1.12.2",
    "1.8.9",
];

// ── tiny runtime config ────────────────────────────────────────────────────
#[derive(Clone, Copy)]
struct Cfg {
    anim: bool,
    mouse: bool,
    tick_ms: u64,
}
impl Cfg {
    fn load() -> Self {
        let off = |k: &str| std::env::var(k).map(|v| v == "1").unwrap_or(false);
        Self {
            anim: !off("MIRAGE_NO_ANIM"),
            mouse: !off("MIRAGE_NO_MOUSE"),
            tick_ms: std::env::var("MIRAGE_TICK_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(40),
        }
    }
}

// ── model ──────────────────────────────────────────────────────────────────
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum TabIndex {
    Instances = 0,
    Mods = 1,
    SearchMods = 2,
    Modpacks = 3,
    NewInstance = 4,
    Settings = 5,
}
impl TabIndex {
    /// Tab labels, in index order.
    pub const ALL: [&'static str; 6] =
        ["Instances", "Installed", "Find Mods", "Modpacks", "New", "Settings"];

    pub fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Mods,
            2 => Self::SearchMods,
            3 => Self::Modpacks,
            4 => Self::NewInstance,
            _ => Self::Instances,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    fn next(self) -> Self {
        match self {
            Self::Instances => Self::Mods,
            Self::Mods => Self::SearchMods,
            Self::SearchMods => Self::Modpacks,
            Self::Modpacks => Self::NewInstance,
            Self::NewInstance => Self::Settings,
            Self::Settings => Self::Instances,
        }
    }
    fn prev(self) -> Self {
        match self {
            Self::Instances => Self::Settings,
            Self::Mods => Self::Instances,
            Self::SearchMods => Self::Mods,
            Self::Modpacks => Self::SearchMods,
            Self::NewInstance => Self::Modpacks,
            Self::Settings => Self::NewInstance,
        }
    }
    fn keys(self) -> &'static str {
        match self {
            Self::Instances => "Enter play • E edit • M mods • C copy • P export • D delete",
            Self::Mods => "Space on/off • U update • D remove • A add • Esc back",
            Self::SearchMods => "Enter install • V version • / search • O sort • F category • C compat",
            Self::Modpacks => "Enter install • V version • / search • O sort • F category",
            Self::NewInstance => "Enter edit • Tab next field • ←→ loader (while editing) • Esc stop",
            Self::Settings => "M sign in • T launch window • Y theme",
        }
    }
}

#[derive(Debug)]
pub enum TuiAction {
    Quit,
}

/// Progress of the Microsoft sign-in, as reported by its background task.
pub enum LoginMsg {
    Code { code: String, uri: String, copied: bool },
    Verifying,
    Done(MinecraftAccount),
    Failed(String),
}

/// Where the sign-in screen is.
enum LoginStage {
    /// First-run greeting; nothing started yet.
    Welcome,
    Requesting,
    /// Waiting for the user to approve the code in the browser.
    Code { code: String, uri: String, copied: bool, since: Instant },
    Verifying,
    Failed(String),
}

impl LoginStage {
    /// True while a background task is running (the screen animates).
    fn busy(&self) -> bool {
        matches!(self, Self::Requesting | Self::Code { .. } | Self::Verifying)
    }
}

#[derive(Debug, PartialEq)]
pub enum InputMode {
    Normal,
    TypingSearch,
    TypingModpackSearch,
    TypingNewInstance,
    TypingCloneName,
    EditingInstance,
}

pub enum DownloadMsg {
    Done(String),
    Error(String),
    /// Result of a background Microsoft session check/refresh.
    Session(Result<(MinecraftAccount, Option<String>), String>),
    /// Minecraft version list for the wizard.
    Versions(Vec<(String, String)>),
    /// Modrinth search results, tagged with the search they answer.
    ModResults(u64, String, Result<Vec<ModHit>, String>),
    PackResults(u64, String, Result<Vec<ModHit>, String>),
    /// A detached launch finished preparing (Ok = status text).
    Launched(Result<String, String>),
    /// An embedded game process started, one log line, or its exit code.
    GameStarted(String, Arc<Mutex<std::process::Child>>),
    GameLog(String, String),
    GameExited(String, i32),
    /// A decoded project icon, keyed by Modrinth project id.
    Icon(String, Icon),
    Login(LoginMsg),
    /// Every version of one Modrinth project.
    ProjectVersions(String, Result<Vec<ModrinthVersion>, String>),
    /// Installed jars matched to Modrinth projects (keyed by file name).
    ModInfo(HashMap<String, IdentifiedMod>),
    /// The lookup failed; forget these jars so a later visit retries.
    ModInfoFailed(Vec<String>),
    /// Newer builds for an instance's jars: (instance, jars checked, updates).
    ModUpdates(String, Vec<String>, HashMap<String, ModrinthVersion>),
    /// Mods updated in place: (instance, jar keys now current, message, is_error).
    ModsUpdated(String, Vec<String>, String, bool),
    /// Whether Modrinth is reachable.
    Online(bool),
}

/// A game started from the TUI: its process and everything it printed.
pub struct Game {
    /// Shared with the waiter task, which polls it for exit; the UI locks
    /// it only to kill.
    child: Arc<Mutex<std::process::Child>>,
    log: VecDeque<String>,
    started: Instant,
    /// Exit code once the process has ended.
    exit: Option<i32>,
    /// A polite stop was sent; the next stop forces it.
    stopping: bool,
}

impl Game {
    fn running(&self) -> bool {
        self.exit.is_none()
    }
    /// First call asks the game to quit (SIGTERM, so it can save the
    /// world); a second call, or `force`, kills it outright.
    fn stop(&mut self, force: bool) {
        let Ok(mut c) = self.child.lock() else { return };
        if force || self.stopping || !cfg!(unix) {
            let _ = c.kill();
        } else {
            let _ = std::process::Command::new("kill").arg(c.id().to_string()).status();
        }
        self.stopping = true;
    }
}

/// Oldest lines are dropped past this, so a chatty modpack cannot grow the
/// log without bound.
const LOG_CAP: usize = 50_000;

/// Messages handled before the next redraw; the rest wait one tick.
const MSGS_PER_FRAME: usize = 1000;

/// The per-instance settings form: RAM, Java and extra JVM flags.
struct EditInstance {
    name: String,
    field: usize,
    vals: [String; 4],
}

const EDIT_FIELDS: [(&str, &str); 4] = [
    ("Min RAM", "default 2G — e.g. 2G or 1536M"),
    ("Max RAM", "default 4G — e.g. 6G"),
    ("Java path", "empty = detect automatically"),
    ("JVM arguments", "extra flags, space separated"),
];

/// Version chooser opened with `V` on a search result.
pub struct Picker {
    project_id: String,
    title: String,
    is_mod: bool,
    /// Show every version, not only the ones that fit the instance.
    show_all: bool,
    state: ListState,
}

pub struct App {
    cfg: Cfg,
    pub theme: Theme,
    pub current_tab: TabIndex,
    pub input_mode: InputMode,
    pub exit: bool,
    pub action: Option<TuiAction>,

    pub instances: Vec<Instance>,
    pub instance_list_state: ListState,
    pub instance_mods_counts: Vec<usize>,

    pub installed_mods: Vec<InstalledMod>,
    pub mods_list_state: ListState,

    pub mod_search_query: String,
    pub mod_search_results: Vec<ModHit>,
    pub mod_search_state: ListState,
    pub searching_mods: bool,
    /// Bumped per search; results from an older search are dropped.
    mod_search_seq: u64,
    pack_search_seq: u64,
    /// A launch is being prepared in the background.
    /// Instance whose launch is being prepared.
    launching: Option<String>,
    /// Games started from the TUI, by instance name (kept after exit so the
    /// log stays readable).
    games: HashMap<String, Game>,
    /// Instance whose page is open on the Instances tab.
    page: Option<String>,
    /// Log lines scrolled up from the bottom; 0 follows new output.
    log_scroll: usize,
    /// First Q while a game runs arms this; a second Q within 4 s quits.
    quit_armed: Option<Instant>,
    /// Summary of the current instance's last session (ok, text).
    last_session: Option<(bool, String)>,
    /// When the current instance was last re-read from disk, so playtime
    /// from a game running in another window shows up without a restart.
    inst_refreshed: Instant,

    pub modpack_search_query: String,
    pub modpack_search_results: Vec<ModHit>,
    pub modpack_search_state: ListState,
    pub searching_modpacks: bool,

    pub new_inst_name: String,
    pub new_inst_version_input: String,
    pub all_online_versions: Vec<(String, String)>,
    pub matching_versions: Vec<String>,
    pub matching_version_idx: usize,
    pub new_inst_loader_idx: usize,
    pub new_inst_ram: String,
    pub new_inst_field: usize,

    pub clone_source: Option<String>,
    pub clone_input: String,
    pub delete_confirm_target: Option<String>,

    pub status_msg: String,
    pub status_is_error: bool,
    pub status_time: Option<Instant>,

    pub bg_download_active: Option<String>,
    pub bg_task_start: Option<Instant>,
    pub bg_task_count: usize,
    pub download_tx: UnboundedSender<DownloadMsg>,
    login: Option<LoginStage>,
    login_task: Option<tokio::task::JoinHandle<()>>,
    pub download_rx: UnboundedReceiver<DownloadMsg>,

    pub term_width: u16,
    pub body_y: u16,
    /// Detached launches: game opens in a new window, TUI stays alive.
    /// Mirrors `launch_new_terminal` in config (default on).
    pub launch_new_term: bool,
    pub active_account: Option<MinecraftAccount>,

    // ── animation ─────────────────────────────────────────────────────────
    /// 40 ms ticks since start; drives the spinner and cursor blink.
    frame: u64,
    /// Wall-clock anchors, so transitions run at the same speed whether the
    /// loop is idle, busy, or processing a burst of key repeats.
    started: Instant,
    last_tick: Instant,
    /// Last row the cursor was on, used to restart the row flourish.
    last_sel: Option<usize>,
    /// Animated tab indicator, expressed in fractional tab-index space.
    tab_pos: f32,
    tab_pos_from: f32,
    tab_anim: f32,
    /// Flourish restarted whenever the cursor moves to a different row.
    sel_anim: f32,
    /// Decay applied to a freshly-set status message.
    status_flash: f32,
    /// Panel glow while a tab has just been entered.
    entrance: f32,
    /// Config snapshot for the settings tab, so rendering never hits disk.
    config: crate::config::Config,

    pub help_open: bool,
    /// Latest pointer position, used for hover feedback on the tab chips.
    pub mouse_x: u16,
    pub mouse_y: u16,
    /// Geometries captured during render so mouse hits are pixel-exact
    /// instead of guessed from proportions. `list_hits` maps a screen row to
    /// the row index of the list drawn there.
    tab_hits: Vec<(u16, u16, u16, TabIndex)>,
    list_hits: Vec<(u16, usize)>,

    // ── Modrinth extras ─────────────────────────────────────────────────────
    /// Icons by project id; `None` = requested (loading) or unavailable.
    icons: HashMap<String, Option<Icon>>,
    /// A terminal cell's height over its width (about 2; fonts differ).
    cell_aspect: f32,
    /// Project versions by id; `None` = loading.
    project_versions: HashMap<String, Option<Result<Vec<ModrinthVersion>, String>>>,
    /// Installed jars by file name (sans `.disabled`); `None` = unknown.
    mod_info: HashMap<String, Option<IdentifiedMod>>,
    picker: Option<Picker>,
    /// Terminal graphics protocol for crisp icons; `None` = half blocks.
    pub gfx: Option<ImgPicker>,
    /// A Modrinth lookup of installed jars is in flight.
    identifying: bool,
    /// Index into `modpack::SORTS` for each search tab.
    mod_sort: usize,
    pack_sort: usize,
    /// Category filter per search tab (index into `modpack::*_CATEGORIES`).
    mod_cat: usize,
    pack_cat: usize,
    /// Mod search limited to the selected instance's loader and version.
    mod_compat: bool,
    /// Newer builds of installed mods, keyed `"<instance>/<jar key>"`.
    mod_updates: HashMap<String, ModrinthVersion>,
    /// Modrinth is reachable (probed every 30 s); drives the offline badge.
    online: bool,
    last_probe: Instant,
    /// The settings form while `InputMode::EditingInstance`.
    edit: Option<EditInstance>,
    /// When the list cursor last moved; version lists load once it rests.
    sel_changed_at: Instant,
    /// Smoothed download speed for the task band (bytes/s).
    speed: f64,
    speed_sample: (Instant, u64),
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

// ── small selection helpers (kill 4× duplicated up/down blocks) ────────────
fn sel_up(s: &mut ListState) {
    let i = s.selected().unwrap_or(0);
    s.select(Some(i.saturating_sub(1)));
}
fn sel_down(s: &mut ListState, n: usize) {
    if n == 0 {
        s.select(None);
        return;
    }
    let i = s.selected().unwrap_or(usize::MAX);
    s.select(Some(i.saturating_add(1).min(n - 1)));
    if s.selected().is_none() {
        s.select(Some(0));
    }
}
fn sel_clamp(s: &mut ListState, n: usize) {
    if n == 0 {
        s.select(None);
        return;
    }
    let i = s.selected().unwrap_or(0).min(n - 1);
    s.select(Some(i));
}
/// Move a list cursor to an absolute row, clamped into `0..n`.
fn sel_updown(s: &mut ListState, want: isize, n: usize) {
    if n == 0 {
        s.select(None);
        return;
    }
    s.select(Some(want.clamp(0, n as isize - 1) as usize));
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        let mut a = Self {
            cfg: Cfg::load(),
            theme: Theme::from_env(),
            current_tab: TabIndex::Instances,
            input_mode: InputMode::Normal,
            exit: false,
            action: None,
            instances: Vec::new(),
            instance_list_state: ListState::default(),
            instance_mods_counts: Vec::new(),
            installed_mods: Vec::new(),
            mods_list_state: ListState::default(),
            mod_search_query: String::new(),
            mod_search_results: Vec::new(),
            mod_search_state: ListState::default(),
            searching_mods: false,
            mod_search_seq: 0,
            pack_search_seq: 0,
            launching: None,
            games: HashMap::new(),
            page: None,
            log_scroll: 0,
            quit_armed: None,
            last_session: None,
            inst_refreshed: Instant::now(),
            modpack_search_query: String::new(),
            modpack_search_results: Vec::new(),
            modpack_search_state: ListState::default(),
            searching_modpacks: false,
            new_inst_name: String::new(),
            new_inst_version_input: "1.21.1".into(),
            all_online_versions: Vec::new(),
            matching_versions: FALLBACK_VERSIONS.iter().map(|s| s.to_string()).collect(),
            matching_version_idx: 1,
            new_inst_loader_idx: 0,
            new_inst_ram: "4G".into(),
            new_inst_field: 0,
            clone_source: None,
            clone_input: String::new(),
            delete_confirm_target: None,
            status_msg: "Ready".into(),
            status_is_error: false,
            status_time: Some(Instant::now()),
            bg_download_active: None,
            bg_task_start: None,
            bg_task_count: 0,
            download_tx: tx,
            login: None,
            login_task: None,
            download_rx: rx,
            term_width: 80,
            body_y: 6,
            launch_new_term: crate::config::load().launch_new_terminal,
            active_account: auth::load_account(),
            frame: 0,
            started: Instant::now(),
            last_tick: Instant::now(),
            last_sel: None,
            tab_pos: 0.0,
            tab_pos_from: 0.0,
            tab_anim: 1.0,
            sel_anim: 1.0,
            status_flash: 1.0,
            entrance: 0.0,
            config: crate::config::load(),
            help_open: false,
            mouse_x: 0,
            mouse_y: 0,
            tab_hits: Vec::new(),
            list_hits: Vec::new(),
            icons: HashMap::new(),
            cell_aspect: 2.0,
            project_versions: HashMap::new(),
            mod_info: HashMap::new(),
            picker: None,
            gfx: None,
            identifying: false,
            mod_sort: 0,
            pack_sort: 0,
            mod_cat: 0,
            pack_cat: 0,
            mod_compat: true,
            mod_updates: HashMap::new(),
            online: true,
            last_probe: Instant::now(),
            edit: None,
            sel_changed_at: Instant::now(),
            speed: 0.0,
            speed_sample: (Instant::now(), 0),
        };
        a.reload_instances();
        a
    }

    pub fn set_status(&mut self, msg: &str, is_error: bool) {
        if self.status_msg != msg {
            self.status_flash = if self.cfg.anim { 0.0 } else { 1.0 };
        }
        self.status_msg = msg.to_string();
        self.status_is_error = is_error;
        self.status_time = Some(Instant::now());
    }

    /// One tick of every animated value, driven from the event loop.
    fn advance_animations(&mut self) {
        let now = Instant::now();
        if self.selected() != self.last_sel {
            self.sel_changed_at = now;
        }
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        self.frame = (now.duration_since(self.started).as_millis() / 40) as u64;

        if !self.cfg.anim {
            self.tab_anim = 1.0;
            self.sel_anim = 1.0;
            self.status_flash = 1.0;
            self.entrance = 0.0;
            return;
        }
        // 0.16 per 40 ms tick ≈ a 250 ms transition.
        let step = 0.16_f32 * dt / 0.040;
        self.tab_anim = (self.tab_anim + step).min(1.0);
        self.sel_anim = (self.sel_anim + step * 2.2).min(1.0);
        self.status_flash = (self.status_flash + step * 1.7).min(1.0);
        self.entrance = (self.entrance - step).max(0.0);

        // Restart the row flourish whenever the cursor lands somewhere new.
        let sel = self.selected();
        if sel != self.last_sel {
            self.last_sel = sel;
            self.sel_anim = 0.0;
        }
    }

    /// Selected row of whichever list the current tab is showing.
    fn selected(&self) -> Option<usize> {
        match self.current_tab {
            TabIndex::Instances => self.instance_list_state.selected(),
            TabIndex::Mods => self.mods_list_state.selected(),
            TabIndex::SearchMods => self.mod_search_state.selected(),
            TabIndex::Modpacks => self.modpack_search_state.selected(),
            TabIndex::NewInstance | TabIndex::Settings => None,
        }
    }

    fn list_len(&self) -> usize {
        match self.current_tab {
            TabIndex::Instances => self.instances.len(),
            TabIndex::Mods => self.installed_mods.len(),
            TabIndex::SearchMods => self.mod_search_results.len(),
            TabIndex::Modpacks => self.modpack_search_results.len(),
            TabIndex::NewInstance => self.matching_versions.len(),
            TabIndex::Settings => 0,
        }
    }

    /// Move the cursor of the current list, clamped to its bounds.
    fn sel_step(&mut self, delta: isize) {
        let n = self.list_len();
        match self.current_tab {
            TabIndex::Instances => {
                let cur = self.instance_list_state.selected().unwrap_or(0) as isize;
                sel_updown(&mut self.instance_list_state, cur + delta, n);
                self.reload_mods();
            }
            TabIndex::Mods => {
                let cur = self.mods_list_state.selected().unwrap_or(0) as isize;
                sel_updown(&mut self.mods_list_state, cur + delta, n);
            }
            TabIndex::SearchMods => {
                let cur = self.mod_search_state.selected().unwrap_or(0) as isize;
                sel_updown(&mut self.mod_search_state, cur + delta, n);
            }
            TabIndex::Modpacks => {
                let cur = self.modpack_search_state.selected().unwrap_or(0) as isize;
                sel_updown(&mut self.modpack_search_state, cur + delta, n);
            }
            TabIndex::NewInstance => {
                if n > 0 {
                    let cur = self.matching_version_idx as isize;
                    let next = (cur + delta).clamp(0, n as isize - 1) as usize;
                    self.matching_version_idx = next;
                    if let Some(v) = self.matching_versions.get(next) {
                        self.new_inst_version_input = v.clone();
                    }
                }
            }
            TabIndex::Settings => {}
        }
    }

    /// Jump the cursor to an absolute row (mouse clicks land here).
    fn select_row(&mut self, idx: usize) {
        if idx >= self.list_len() {
            return;
        }
        match self.current_tab {
            TabIndex::Instances => {
                self.instance_list_state.select(Some(idx));
                self.reload_mods();
            }
            TabIndex::Mods => self.mods_list_state.select(Some(idx)),
            TabIndex::SearchMods => self.mod_search_state.select(Some(idx)),
            TabIndex::Modpacks => self.modpack_search_state.select(Some(idx)),
            TabIndex::NewInstance => self.matching_version_idx = idx,
            TabIndex::Settings => {}
        }
    }

    pub fn reload_instances(&mut self) {
        let keep = self.current_instance().map(|i| i.name.clone());
        self.instances = Instance::list_all();
        // Most recently played first (RFC 3339 sorts lexically); never-played last.
        self.instances.sort_by(|a, b| b.last_played.cmp(&a.last_played));
        // Cache once per reload: render must never touch disk.
        self.instance_mods_counts = self.instances.iter().map(|i| i.installed_mods().len()).collect();
        if let Some(i) = keep.and_then(|n| self.instances.iter().position(|x| x.name == n)) {
            self.instance_list_state.select(Some(i));
        }
        sel_clamp(&mut self.instance_list_state, self.instances.len());
        if self.instance_list_state.selected().is_none() && !self.instances.is_empty() {
            self.instance_list_state.select(Some(0));
        }
        let icons = self
            .instances
            .iter()
            .filter_map(|i| Some((i.modrinth_project.clone()?, i.icon.clone()?)))
            .collect();
        self.request_icons(icons);
        self.reload_mods();
    }

    pub fn current_instance(&self) -> Option<&Instance> {
        self.instances.get(self.instance_list_state.selected().unwrap_or(0))
    }

    pub fn reload_mods(&mut self) {
        self.installed_mods = self.current_instance().map(|i| i.installed_mods()).unwrap_or_default();
        sel_clamp(&mut self.mods_list_state, self.installed_mods.len());
        // Keep counts in sync for the details pane without rescanning all.
        if let Some(idx) = self.instance_list_state.selected() {
            if let Some(c) = self.instance_mods_counts.get_mut(idx) {
                *c = self.installed_mods.len();
            }
        }
        if matches!(self.current_tab, TabIndex::Mods | TabIndex::Instances) {
            self.identify_mods();
        }
        self.last_session = self.session_of_current();
    }

    /// Last session of the selected instance, with a crash hint drawn from
    /// the game's own output when it is still in memory.
    fn session_of_current(&self) -> Option<(bool, String)> {
        let inst = self.current_instance()?;
        session_summary(inst, self.games.get(&inst.name).map(|g| &g.log))
    }

    /// Re-read the selected instance every few seconds on the Instances tab,
    /// so sessions logged by a game in another window (playtime, crashes)
    /// appear on their own.
    fn refresh_current_instance(&mut self) {
        if self.current_tab != TabIndex::Instances || self.inst_refreshed.elapsed().as_secs() < 3 {
            return;
        }
        self.inst_refreshed = Instant::now();
        let Some(idx) = self.instance_list_state.selected() else { return };
        let Some(name) = self.instances.get(idx).map(|i| i.name.clone()) else { return };
        if let Ok(fresh) = Instance::load(&name) {
            let changed = self.instances[idx].playtime_seconds != fresh.playtime_seconds
                || self.instances[idx].last_exit != fresh.last_exit
                || self.instances[idx].last_played != fresh.last_played;
            if changed {
                self.instances[idx] = fresh;
                self.last_session = self.session_of_current();
            }
        }
    }

    /// Match installed jars we have not seen yet to Modrinth projects, so the
    /// Mods tab can show real names, icons, download counts and updates.
    fn identify_mods(&mut self) {
        if !self.online {
            return;
        }
        let Some(inst) = self.current_instance() else { return };
        let (name, dir, loader, mc) = (inst.name.clone(), inst.mods_dir(), inst.loader.clone(), inst.mc_version.clone());
        let files: Vec<_> = self
            .installed_mods
            .iter()
            .filter(|m| !self.mod_info.contains_key(mod_key(&m.filename)))
            .map(|m| dir.join(&m.filename))
            .collect();
        if files.is_empty() {
            return;
        }
        for m in &self.installed_mods {
            self.mod_info.entry(mod_key(&m.filename).to_string()).or_insert(None);
        }
        self.identifying = true;
        let keys: Vec<String> = files
            .iter()
            .filter_map(|p| Some(mod_key(p.file_name()?.to_str()?).to_string()))
            .collect();
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            match modpack::identify_mods(files).await {
                Ok(found) => {
                    let updates = modpack::check_updates(&found, &loader, &mc).await.unwrap_or_default();
                    let _ = tx.send(DownloadMsg::ModInfo(found));
                    let _ = tx.send(DownloadMsg::ModUpdates(name, keys, updates));
                }
                Err(_) => {
                    let _ = tx.send(DownloadMsg::ModInfoFailed(keys));
                }
            }
        });
    }

    /// Newer build of an installed jar, if the last check found one.
    fn update_for(&self, instance: &str, filename: &str) -> Option<&ModrinthVersion> {
        self.mod_updates.get(&format!("{instance}/{}", mod_key(filename)))
    }

    /// Update the selected mod to its newest build for this instance.
    fn update_selected_mod(&mut self) {
        let sel = self.mods_list_state.selected().unwrap_or(0);
        let (Some(inst), Some(m)) = (self.current_instance().cloned(), self.installed_mods.get(sel).cloned()) else { return };
        let Some(ver) = self.update_for(&inst.name, &m.filename).cloned() else {
            self.set_status(&format!("{} is up to date", m.display_name), false);
            return;
        };
        let title = self.mod_info.get(mod_key(&m.filename)).and_then(|o| o.as_ref()).map_or(m.display_name.clone(), |x| x.project.title.clone());
        self.start_bg_task(format!("Updating {title} to {}", ver.version_number));
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let (done, msg, err) = match modpack::update_mod(&ver, &inst, &m.filename).await {
                Ok(d) => (vec![mod_key(&m.filename).to_string()], format!("✔ {title} → {}", d.version_number), false),
                Err(e) => (Vec::new(), format!("Update {title}: {e:#}"), true),
            };
            let _ = tx.send(DownloadMsg::ModsUpdated(inst.name, done, msg, err));
        });
    }

    /// Update every mod of the selected instance that has a newer build.
    fn update_all_mods(&mut self) {
        let Some(inst) = self.current_instance().cloned() else { return };
        let todo: Vec<(String, String, ModrinthVersion)> = self
            .installed_mods
            .iter()
            .filter_map(|m| {
                let v = self.update_for(&inst.name, &m.filename)?;
                Some((m.filename.clone(), m.display_name.clone(), v.clone()))
            })
            .collect();
        if todo.is_empty() {
            self.set_status("All mods are up to date", false);
            return;
        }
        self.start_bg_task(format!("Updating {} mods", todo.len()));
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let (mut done, mut failed) = (Vec::new(), Vec::new());
            for (file, name, ver) in &todo {
                match modpack::update_mod(ver, &inst, file).await {
                    Ok(_) => done.push(mod_key(file).to_string()),
                    Err(_) => failed.push(name.clone()),
                }
            }
            let (msg, err) = if failed.is_empty() {
                (format!("✔ Updated {} mods", done.len()), false)
            } else {
                (format!("Updated {}, failed: {}", done.len(), failed.join(", ")), true)
            };
            let _ = tx.send(DownloadMsg::ModsUpdated(inst.name, done, msg, err));
        });
    }

    /// Pack the selected instance into a `.mrpack` in the Downloads folder.
    fn export_current(&mut self) {
        let Some(inst) = self.current_instance().cloned() else { return };
        self.start_bg_task(format!("Exporting {}", inst.name));
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(match modpack::export_mrpack(&inst, &modpack::export_dir()).await {
                Ok(e) => DownloadMsg::Done(format!(
                    "✔ Exported '{}' → {} ({} linked, {} bundled)",
                    inst.name,
                    short_path(&e.path),
                    e.linked,
                    e.bundled
                )),
                Err(e) => DownloadMsg::Error(format!("Export '{}': {e:#}", inst.name)),
            });
        });
    }

    // ── instance settings (RAM, Java, JVM flags) ───────────────────────────
    fn open_edit(&mut self) {
        let Some(i) = self.current_instance() else { return };
        self.edit = Some(EditInstance {
            name: i.name.clone(),
            field: 0,
            vals: [
                i.ram_min.clone().unwrap_or_default(),
                i.ram_max.clone().unwrap_or_default(),
                i.java_path.clone().unwrap_or_default(),
                i.jvm_args.join(" "),
            ],
        });
        self.input_mode = InputMode::EditingInstance;
    }

    fn edit_key(&mut self, key: KeyEvent) {
        let Some(ed) = &mut self.edit else {
            self.input_mode = InputMode::Normal;
            return;
        };
        let outcome = match key.code {
            KeyCode::Tab | KeyCode::Down => { ed.field = (ed.field + 1) % 4; Edit::More }
            KeyCode::BackTab | KeyCode::Up => { ed.field = (ed.field + 3) % 4; Edit::More }
            _ => edit_line(key, &mut ed.vals[ed.field]),
        };
        match outcome {
            Edit::Done => self.save_edit(),
            Edit::Cancel => {
                self.edit = None;
                self.input_mode = InputMode::Normal;
            }
            Edit::More => {}
        }
    }

    /// Validate the form and write it to the instance. A bad value keeps the
    /// form open on the offending field.
    fn save_edit(&mut self) {
        let Some(ed) = &mut self.edit else { return };
        let [min, max, java, args] = ed.vals.clone().map(|v| v.trim().to_string());
        let mem = |v: &str| if v.is_empty() { Some(None) } else { launcher::parse_mem_mb(v).filter(|m| *m > 0).map(Some) };
        let problem = match (mem(&min), mem(&max)) {
            (None, _) => Some((0, "Min RAM: use a size like 2G or 1536M")),
            (_, None) => Some((1, "Max RAM: use a size like 6G or 4096M")),
            (Some(Some(lo)), Some(Some(hi))) if lo > hi => Some((0, "Min RAM cannot be above Max RAM")),
            _ if !java.is_empty() && !Path::new(&java).is_file() => Some((2, "Java path: no such file")),
            _ => None,
        };
        if let Some((field, msg)) = problem {
            ed.field = field;
            self.set_status(msg, true);
            return;
        }
        let name = ed.name.clone();
        let some = |v: String| (!v.is_empty()).then_some(v);
        let saved = Instance::load(&name).and_then(|mut i| {
            i.ram_min = some(min);
            i.ram_max = some(max);
            i.java_path = some(java);
            i.jvm_args = args.split_whitespace().map(String::from).collect();
            i.save()
        });
        match saved {
            Ok(()) => {
                self.edit = None;
                self.input_mode = InputMode::Normal;
                self.reload_instances();
                self.set_status(&format!("Saved settings for '{name}'"), false);
            }
            Err(e) => self.set_status(&format!("Save: {e}"), true),
        }
    }

    /// Check Modrinth is reachable; the answer arrives as `Online`.
    fn probe_online(&mut self) {
        self.last_probe = Instant::now();
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let up = download::Downloader::new().reachable("https://api.modrinth.com/").await;
            let _ = tx.send(DownloadMsg::Online(up));
        });
    }

    /// Start fetching icons we do not have yet. Failures just leave the
    /// placeholder tile in place.
    fn request_icons(&mut self, want: Vec<(String, String)>) {
        for (id, url) in want {
            if self.icons.contains_key(&id) {
                continue;
            }
            self.icons.insert(id.clone(), None);
            let tx = self.download_tx.clone();
            let gfx = self.gfx.clone();
            tokio::spawn(async move {
                if let Some(icon) = Icon::fetch(&id, &url, gfx).await {
                    let _ = tx.send(DownloadMsg::Icon(id, icon));
                }
            });
        }
    }

    /// Show the sign-in screen (first-run greeting).
    fn open_login(&mut self) {
        if self.login.is_none() {
            self.login = Some(LoginStage::Welcome);
        }
    }

    /// Run the Microsoft device-code sign-in in the background; progress comes
    /// back as `LoginMsg`s so the TUI never leaves the alternate screen.
    fn start_login(&mut self) {
        if let Some(t) = self.login_task.take() {
            t.abort();
        }
        self.login = Some(LoginStage::Requesting);
        let tx = self.download_tx.clone();
        self.login_task = Some(tokio::spawn(async move {
            let send = |m| {
                let _ = tx.send(DownloadMsg::Login(m));
            };
            let run = async {
                let client = auth::http_client()?;
                let dc = auth::request_device_code(&client).await?;
                let (code, uri) = (dc.user_code.clone(), dc.verification_uri.clone());
                let copied = auth::copy_system_clipboard(&code);
                auth::open_in_browser(&uri);
                send(LoginMsg::Code { code, uri, copied });
                let msa = auth::poll_device_code(&client, &dc).await?;
                send(LoginMsg::Verifying);
                auth::finish_login(&client, msa).await
            };
            match run.await {
                Ok(acc) => send(LoginMsg::Done(acc)),
                Err(e) => send(LoginMsg::Failed(format!("{e:#}"))),
            }
        }));
    }

    fn login_msg(&mut self, m: LoginMsg) {
        match m {
            LoginMsg::Code { code, uri, copied } => {
                self.login = Some(LoginStage::Code { code, uri, copied, since: Instant::now() });
            }
            LoginMsg::Verifying => self.login = Some(LoginStage::Verifying),
            LoginMsg::Failed(e) => self.login = Some(LoginStage::Failed(e)),
            LoginMsg::Done(acc) => {
                self.login = None;
                self.login_task = None;
                let next = if self.instances.is_empty() { " — press N to create your first instance" } else { "" };
                self.set_status(&format!("Signed in as {}{next}", acc.username), false);
                self.active_account = Some(acc);
                self.request_head();
            }
        }
    }

    fn login_key(&mut self, key: KeyEvent) {
        let Some(stage) = &self.login else { return };
        match (stage, key.code) {
            (LoginStage::Welcome, KeyCode::Enter) | (LoginStage::Failed(_), KeyCode::Enter | KeyCode::Char('r')) => self.start_login(),
            (LoginStage::Code { code, .. }, KeyCode::Char('c')) => {
                let msg = if auth::copy_system_clipboard(code) { "Code copied" } else { "No clipboard tool found — type the code by hand" };
                let msg = msg.to_string();
                self.set_status(&msg, false);
            }
            (LoginStage::Code { uri, .. }, KeyCode::Char('o')) => auth::open_in_browser(uri),
            (_, KeyCode::Esc) => {
                if let Some(t) = self.login_task.take() {
                    t.abort();
                }
                self.login = None;
            }
            _ => {}
        }
    }

    /// First-run checklist: sign in, create an instance, play.
    fn setup_steps(&self) -> Vec<Line<'static>> {
        let signed = self.active_account.is_some();
        let made = !self.instances.is_empty();
        let played = self.instances.iter().any(|i| i.last_played.is_some());
        let step = |done: bool, active: bool, n: u8, text: &str| {
            let (mark, tone) = if done {
                ("✔", self.theme.accent)
            } else if active {
                ("▸", self.theme.primary)
            } else {
                ("○", self.theme.muted)
            };
            Line::from(vec![
                Span::styled(format!("  {mark} "), Style::default().fg(tone).bold()),
                Span::styled(format!("{n}  {text}"), Style::default().fg(if done { self.theme.muted } else { self.theme.fg })),
            ])
        };
        vec![
            step(signed, !signed, 1, "Sign in with Microsoft"),
            step(made, signed && !made, 2, "Create an instance or install a modpack"),
            step(played, signed && made && !played, 3, "Press Enter on it to play"),
        ]
    }

    /// Fetch the signed-in player's head (shown on the Settings page).
    fn request_head(&mut self) {
        let Some(uuid) = self.active_account.as_ref().map(|a| a.uuid.clone()) else { return };
        let id = format!("head:{uuid}");
        if self.icons.contains_key(&id) {
            return;
        }
        self.icons.insert(id.clone(), None);
        let (tx, gfx) = (self.download_tx.clone(), self.gfx.clone());
        tokio::spawn(async move {
            if let Some(icon) = Icon::fetch_head(&uuid, gfx).await {
                let _ = tx.send(DownloadMsg::Icon(id, icon));
            }
        });
    }

    /// Search hit under the cursor on the active search tab.
    fn selected_hit(&self) -> Option<(&ModHit, bool)> {
        match self.current_tab {
            TabIndex::SearchMods => self.mod_search_results.get(self.mod_search_state.selected()?).map(|h| (h, true)),
            TabIndex::Modpacks => self.modpack_search_results.get(self.modpack_search_state.selected()?).map(|h| (h, false)),
            _ => None,
        }
    }

    /// Load the version list of the highlighted project once the cursor has
    /// rested briefly, so scrolling through results does not fire a request
    /// per row.
    fn ensure_versions(&mut self) {
        if self.sel_changed_at.elapsed().as_millis() < 150 {
            return;
        }
        let Some(id) = self.selected_hit().map(|(h, _)| h.project_id.clone()) else { return };
        if self.project_versions.contains_key(&id) {
            return;
        }
        self.project_versions.insert(id.clone(), None);
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let r = modpack::project_versions(&id).await.map_err(|e| e.to_string());
            let _ = tx.send(DownloadMsg::ProjectVersions(id, r));
        });
    }

    /// Versions of `project_id` to offer: those that fit the current instance
    /// (mods only), or all of them when `show_all` is set or none fit.
    /// Returns the list and whether it was filtered.
    fn version_choices(&self, project_id: &str, is_mod: bool, show_all: bool) -> (Vec<&ModrinthVersion>, bool) {
        let Some(Some(Ok(all))) = self.project_versions.get(project_id) else { return (Vec::new(), false) };
        let inst = self.current_instance();
        if !is_mod || show_all || inst.is_none() {
            return (all.iter().collect(), false);
        }
        let inst = inst.unwrap();
        let fit: Vec<_> = all.iter().filter(|v| v.fits(&inst.mc_version, &inst.loader)).collect();
        if fit.is_empty() { (all.iter().collect(), false) } else { (fit, true) }
    }

    /// The version Enter installs: newest stable build that fits, else the
    /// newest that fits at all.
    fn best_version(&self, project_id: &str, is_mod: bool) -> Option<ModrinthVersion> {
        let (choices, filtered) = self.version_choices(project_id, is_mod, false);
        if is_mod && !filtered {
            return None; // nothing fits: let the server-side fallback decide
        }
        choices
            .iter()
            .find(|v| v.version_type == "release")
            .or(choices.first())
            .map(|v| (*v).clone())
    }

    /// Smoothed transfer rate for the task band.
    fn sample_speed(&mut self) {
        let (at, bytes) = self.speed_sample;
        let dt = at.elapsed().as_secs_f64();
        if dt < 0.25 {
            return;
        }
        let now = download::progress().bytes.load(std::sync::atomic::Ordering::Relaxed);
        let rate = now.saturating_sub(bytes) as f64 / dt;
        self.speed = if self.speed == 0.0 { rate } else { self.speed * 0.7 + rate * 0.3 };
        self.speed_sample = (Instant::now(), now);
    }

    /// Version list comes from a 2 h disk cache (or the network) off the UI
    /// thread; the wizard shows built-in versions until it lands.
    fn fetch_versions_in_background(&self) {
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            if let Ok(v) = launcher::fetch_minecraft_versions(true).await {
                let _ = tx.send(DownloadMsg::Versions(v));
            }
        });
    }

    pub fn update_version_matches(&mut self) {
        let q = self.new_inst_version_input.trim().to_lowercase();
        self.matching_versions = if q.is_empty() {
            self.all_online_versions.iter().filter(|(_, t)| t == "release").take(15).map(|(v, _)| v.clone()).collect()
        } else {
            self.all_online_versions.iter().filter(|(v, _)| v.to_lowercase().contains(&q)).take(15).map(|(v, _)| v.clone()).collect()
        };
        if self.matching_versions.is_empty() && !q.is_empty() {
            self.matching_versions.push(self.new_inst_version_input.clone());
        }
        self.matching_version_idx = 0;
    }

    fn goto(&mut self, t: TabIndex) {
        if self.current_tab != t {
            // Start the slide from wherever the indicator currently sits, so
            // hammering 1-6 still looks continuous.
            self.tab_pos_from = lerp(self.tab_pos_from, self.tab_pos, ease_out(self.tab_anim));
            self.tab_anim = if self.cfg.anim { 0.0 } else { 1.0 };
            self.entrance = if self.cfg.anim { 1.0 } else { 0.0 };
        }
        self.tab_pos = t.index() as f32;
        self.current_tab = t;
        self.help_open = false;
        self.sel_anim = if self.cfg.anim { 0.0 } else { 1.0 };
        self.last_sel = None;
        // Never arrive trapped in the wizard: typing starts explicitly via
        // i/Enter/Space so global keys (1-6, Tab, Q, Esc) keep working.
        if self.input_mode == InputMode::TypingNewInstance {
            self.input_mode = InputMode::Normal;
        }
        if t == TabIndex::Mods {
            self.reload_mods();
        }
        if t == TabIndex::SearchMods && self.mod_search_results.is_empty() && !self.searching_mods {
            let q = self.mod_search_query.trim().to_string();
            self.execute_mod_search(&q);
        }
        if t == TabIndex::Modpacks && self.modpack_search_results.is_empty() && !self.searching_modpacks {
            let q = self.modpack_search_query.trim().to_string();
            self.execute_modpack_search(&q);
        }
    }

    pub fn start_bg_task(&mut self, title: String) {
        if self.bg_download_active.is_none() {
            download::progress().reset();
            self.speed = 0.0;
            self.speed_sample = (Instant::now(), 0);
            self.bg_download_active = Some(title);
            self.bg_task_start = Some(Instant::now());
        }
        self.bg_task_count += 1;
    }

    fn bg_done(&mut self, msg: String, err: bool) {
        self.bg_task_count = self.bg_task_count.saturating_sub(1);
        if self.bg_task_count == 0 {
            self.bg_download_active = None;
            self.bg_task_start = None;
            download::progress().reset();
        }
        self.set_status(&msg, err);
        if !err {
            self.reload_mods();
            self.reload_instances();
        }
    }

    pub fn poll_background_downloads(&mut self) {
        if self.last_probe.elapsed().as_secs() >= 30 {
            self.probe_online();
        }
        // Bounded per frame: a chatty game must not starve drawing and keys.
        for _ in 0..MSGS_PER_FRAME {
            let Ok(m) = self.download_rx.try_recv() else { break };
            match m {
                DownloadMsg::Done(x) => self.bg_done(x, false),
                DownloadMsg::Error(x) => self.bg_done(x, true),
                DownloadMsg::Session(r) => self.session_checked(r),
                DownloadMsg::Versions(v) => {
                    self.all_online_versions = v;
                    self.update_version_matches();
                }
                DownloadMsg::ModResults(seq, q, r) if seq == self.mod_search_seq => {
                    self.searching_mods = false;
                    self.apply_search(true, &q, r);
                }
                DownloadMsg::PackResults(seq, q, r) if seq == self.pack_search_seq => {
                    self.searching_modpacks = false;
                    self.apply_search(false, &q, r);
                }
                DownloadMsg::ModResults(..) | DownloadMsg::PackResults(..) => {} // superseded
                DownloadMsg::Launched(r) => {
                    self.launching = None;
                    match r {
                        Ok(msg) => self.bg_done(msg, false),
                        Err(e) => self.bg_done(e, true),
                    }
                }
                DownloadMsg::GameStarted(name, child) => {
                    self.launching = None;
                    self.bg_done(format!("'{name}' is running — its log is on the instance page"), false);
                    self.games.insert(name, Game { child, log: VecDeque::new(), started: Instant::now(), exit: None, stopping: false });
                    self.log_scroll = 0;
                }
                DownloadMsg::GameLog(name, line) => {
                    if let Some(g) = self.games.get_mut(&name) {
                        g.log.push_back(line);
                        if g.log.len() > LOG_CAP {
                            g.log.pop_front();
                        }
                        // Keep a scrolled-up view anchored on the same text.
                        if self.log_scroll > 0 && self.page.as_deref() == Some(name.as_str()) {
                            self.log_scroll += 1;
                        }
                    }
                }
                DownloadMsg::GameExited(name, code) => {
                    if let Some(g) = self.games.get_mut(&name) {
                        g.exit = Some(code);
                    }
                    let msg = if clean_exit(code) { format!("'{name}' closed") } else { format!("'{name}' exited with code {code}") };
                    self.set_status(&msg, code != 0);
                    self.reload_instances();
                }
                DownloadMsg::Login(m) => self.login_msg(m),
                DownloadMsg::Icon(id, icon) => {
                    self.icons.insert(id, Some(icon));
                }
                DownloadMsg::ProjectVersions(id, r) => {
                    self.project_versions.insert(id, Some(r));
                }
                DownloadMsg::ModInfoFailed(keys) => {
                    self.identifying = false;
                    for k in keys {
                        self.mod_info.remove(&k);
                    }
                }
                DownloadMsg::ModUpdates(inst, checked, found) => {
                    for k in checked {
                        self.mod_updates.remove(&format!("{inst}/{k}"));
                    }
                    for (file, ver) in found {
                        self.mod_updates.insert(format!("{inst}/{}", mod_key(&file)), ver);
                    }
                }
                DownloadMsg::ModsUpdated(inst, done, msg, err) => {
                    for k in done {
                        self.mod_updates.remove(&format!("{inst}/{k}"));
                    }
                    self.bg_done(msg, err);
                }
                DownloadMsg::Online(up) if up != self.online => {
                    self.online = up;
                    if up {
                        self.set_status("Back online", false);
                        self.check_session_in_background();
                        self.reload_mods();
                        for is_mod in [true, false] {
                            let empty = if is_mod { self.mod_search_results.is_empty() } else { self.modpack_search_results.is_empty() };
                            if empty {
                                self.rerun_search(is_mod);
                            }
                        }
                    } else {
                        self.set_status("Offline — saved results only; launching still works", false);
                    }
                }
                DownloadMsg::Online(_) => {}
                DownloadMsg::ModInfo(found) => {
                    self.identifying = false;
                    let want: Vec<_> = found
                        .values()
                        .filter_map(|m| Some((m.project.project_id.clone(), m.project.icon_url.clone()?)))
                        .collect();
                    for (k, v) in found {
                        self.mod_info.insert(k, Some(v));
                    }
                    self.request_icons(want);
                }
            }
        }
        self.sample_speed();
        self.ensure_versions();
        self.refresh_current_instance();
        if let Some(t) = self.status_time {
            if !self.status_is_error && t.elapsed().as_secs() > 12 {
                self.status_msg = "Ready".into();
                self.status_time = None;
            }
        }
        self.advance_animations();
    }

    /// Refresh an expired (or rejected) Microsoft session off the UI thread,
    /// so a day-old token never looks like the user must sign in again.
    fn check_session_in_background(&self) {
        let Some(acc) = self.active_account.clone() else { return };
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let r = auth::revalidate_account(&acc).await.map_err(|e| e.to_string());
            let _ = tx.send(DownloadMsg::Session(r));
        });
    }

    fn session_checked(&mut self, r: Result<(MinecraftAccount, Option<String>), String>) {
        match r {
            // Ignore stale results if the user switched account meanwhile.
            Ok((acc, note)) => {
                if self.active_account.as_ref().map(|a| &a.uuid) == Some(&acc.uuid) {
                    self.active_account = Some(acc);
                    if let Some(n) = note {
                        self.set_status(&n, false);
                    }
                }
            }
            Err(e) => self.set_status(&e, true),
        }
    }

    /// True while something on screen is moving and needs frame-rate redraws.
    fn animating(&self) -> bool {
        (self.cfg.anim
            && (self.tab_anim < 1.0 || self.sel_anim < 1.0 || self.status_flash < 1.0 || self.entrance > 0.0))
            || self.bg_download_active.is_some()
            || self.searching_mods
            || self.searching_modpacks
            || !self.download_rx.is_empty()
            || self.login.as_ref().is_some_and(LoginStage::busy)
            || self.input_mode != InputMode::Normal
    }

    pub async fn run_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<Option<TuiAction>> {
        self.check_session_in_background();
        self.fetch_versions_in_background();
        self.request_head();
        self.probe_online();
        // Warm both browse tabs so they open filled, not blank.
        self.execute_mod_search("");
        self.execute_modpack_search("");
        if self.active_account.is_none() {
            self.open_login();
        }
        while !self.exit {
            self.poll_background_downloads();
            terminal.draw(|f| self.render(f))?;
            // Idle: wake rarely (background results, status timeout). Moving:
            // run at the tick rate.
            let wait = if self.animating() { self.cfg.tick_ms } else { 100 };
            if !event::poll(std::time::Duration::from_millis(wait))? {
                continue;
            }
            // Handle every queued event before the next draw, so held keys
            // and fast typing never lag behind the screen.
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => self.handle_key(k).await,
                    Event::Mouse(m) if self.cfg.mouse => self.handle_mouse(m).await,
                    _ => {}
                }
                if self.exit || !event::poll(std::time::Duration::ZERO)? {
                    break;
                }
            }
        }
        // Game output is piped into this process, so a game left running
        // would die on a broken pipe anyway: stop it cleanly instead.
        for g in self.games.values_mut().filter(|g| g.running()) {
            g.stop(true);
        }
        Ok(self.action.take())
    }

    // ── mouse ──────────────────────────────────────────────────────────────
    // Hit-testing uses the rects captured during the last render, so clicks
    // land exactly where things were actually drawn.
    pub async fn handle_mouse(&mut self, mouse: MouseEvent) {
        self.mouse_x = mouse.column;
        self.mouse_y = mouse.row;
        match mouse.kind {
            MouseEventKind::ScrollUp if self.page.is_some() => self.scroll_log(3, true),
            MouseEventKind::ScrollDown if self.page.is_some() => self.scroll_log(3, false),
            MouseEventKind::ScrollUp => self.sel_step(-1),
            MouseEventKind::ScrollDown => self.sel_step(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let tab = self
                    .tab_hits
                    .iter()
                    .find(|(y, a, b, _)| mouse.row == *y && mouse.column >= *a && mouse.column < *b)
                    .map(|(_, _, _, t)| *t);
                if let Some(t) = tab {
                    self.goto(t);
                    self.input_mode = InputMode::Normal;
                    return;
                }
                let row = self
                    .list_hits
                    .iter()
                    .find(|(y, _)| *y == mouse.row)
                    .map(|(_, i)| *i);
                if let Some(i) = row {
                    self.select_row(i);
                }
            }
            _ => {}
        }
    }

    // ── keys ───────────────────────────────────────────────────────────────
    async fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.exit = true;
            return;
        }
        // The sign-in screen owns the keyboard while it is open.
        if self.login.is_some() {
            self.login_key(key);
            return;
        }
        // Typing modes first (they swallow everything except Ctrl-C above).
        match self.input_mode {
            InputMode::TypingSearch => {
                match edit_line(key, &mut self.mod_search_query) {
                    Edit::Done => {
                        self.input_mode = InputMode::Normal;
                        let q = self.mod_search_query.trim().to_string();
                        self.execute_mod_search(&q);
                    }
                    Edit::Cancel => self.input_mode = InputMode::Normal,
                    Edit::More => {}
                }
                return;
            }
            InputMode::TypingModpackSearch => {
                match edit_line(key, &mut self.modpack_search_query) {
                    Edit::Done => {
                        self.input_mode = InputMode::Normal;
                        let q = self.modpack_search_query.trim().to_string();
                        self.execute_modpack_search(&q);
                    }
                    Edit::Cancel => self.input_mode = InputMode::Normal,
                    Edit::More => {}
                }
                return;
            }
            InputMode::TypingCloneName => {
                match edit_line(key, &mut self.clone_input) {
                    Edit::Done => {
                        let n = self.clone_input.trim().to_string();
                        if !n.is_empty() {
                            if let Some(src) = self.clone_source.clone() {
                                match Instance::load(&src).and_then(|i| i.duplicate(&n)) {
                                    Ok(_) => {
                                        self.set_status(&format!("Duplicated '{src}' → '{n}'"), false);
                                        self.reload_instances();
                                    }
                                    Err(e) => self.set_status(&format!("Duplicate: {e}"), true),
                                }
                            }
                        }
                        self.clone_source = None;
                        self.clone_input.clear();
                        self.input_mode = InputMode::Normal;
                    }
                    Edit::Cancel => {
                        self.clone_source = None;
                        self.clone_input.clear();
                        self.input_mode = InputMode::Normal;
                    }
                    Edit::More => {}
                }
                return;
            }
            InputMode::TypingNewInstance => {
                self.wizard_key(key).await;
                return;
            }
            InputMode::EditingInstance => {
                self.edit_key(key);
                return;
            }
            InputMode::Normal => {}
        }

        // Help overlay swallows everything except the keys that dismiss it.
        if self.help_open {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter | KeyCode::Char(' ')
            ) {
                self.help_open = false;
            }
            return;
        }
        if key.code == KeyCode::Char('?') {
            self.help_open = true;
            return;
        }

        // Version picker steals keys while open.
        if self.picker.is_some() {
            self.picker_key(key);
            return;
        }

        // Delete modal steals keys.
        if let Some(t) = self.delete_confirm_target.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    if let Ok(i) = Instance::load(&t) {
                        let _ = i.delete();
                        self.set_status(&format!("Deleted '{t}'"), false);
                        self.reload_instances();
                    }
                    self.delete_confirm_target = None;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.delete_confirm_target = None;
                    self.set_status("Delete cancelled", false);
                }
                _ => {}
            }
            return;
        }

        // Global tab switches.
        match key.code {
            KeyCode::Char('1') => { self.goto(TabIndex::Instances); return; }
            KeyCode::Char('2') => { self.goto(TabIndex::Mods); return; }
            KeyCode::Char('3') => { self.goto(TabIndex::SearchMods); return; }
            KeyCode::Char('4') => { self.goto(TabIndex::Modpacks); return; }
            KeyCode::Char('5') => { self.goto(TabIndex::NewInstance); return; }
            KeyCode::Char('6') => { self.goto(TabIndex::Settings); return; }
            KeyCode::Tab => { let n = self.current_tab.next(); self.goto(n); return; }
            KeyCode::BackTab => { let p = self.current_tab.prev(); self.goto(p); return; }
            KeyCode::Char(']') => { let n = self.current_tab.next(); self.goto(n); return; }
            KeyCode::Char('[') => { let p = self.current_tab.prev(); self.goto(p); return; }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                let running = self.games.values().filter(|g| g.running()).count();
                let armed = self.quit_armed.is_some_and(|t| t.elapsed().as_secs() < 4);
                if running > 0 && !armed {
                    self.quit_armed = Some(Instant::now());
                    self.set_status("Minecraft is running — press Q again to stop it and quit", true);
                    return;
                }
                self.action = Some(TuiAction::Quit);
                self.exit = true;
                return;
            }
            _ => {}
        }
        // List movement shared by every list-backed tab.
        match key.code {
            KeyCode::Home | KeyCode::Char('g') => {
                self.select_row(0);
                return;
            }
            KeyCode::End | KeyCode::Char('G') => {
                let n = self.list_len();
                if n > 0 {
                    self.select_row(n - 1);
                }
                return;
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sel_step(8);
                return;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sel_step(-8);
                return;
            }
            _ => {}
        }
        // Left/Right flip tabs everywhere. On the wizard they only edit the
        // loader once typing has started (Enter), so arrows never trap you.
        match key.code {
            KeyCode::Right => { let n = self.current_tab.next(); self.goto(n); return; }
            KeyCode::Left => { let p = self.current_tab.prev(); self.goto(p); return; }
            _ => {}
        }

        match self.current_tab {
            TabIndex::Instances => self.instances_key(key).await,
            TabIndex::Mods => self.mods_key(key).await,
            TabIndex::SearchMods => self.search_key(key, true).await,
            TabIndex::Modpacks => self.search_key(key, false).await,
            TabIndex::NewInstance => self.new_instance_normal_key(key).await,
            TabIndex::Settings => self.settings_key(key).await,
        }
    }

    async fn instances_key(&mut self, key: KeyEvent) {
        if self.page.is_some() {
            self.page_key(key);
            return;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => { sel_up(&mut self.instance_list_state); self.reload_mods(); }
            KeyCode::Down | KeyCode::Char('j') => { sel_down(&mut self.instance_list_state, self.instances.len()); self.reload_mods(); }
            KeyCode::Enter | KeyCode::Char('l') => {
                if let Some(i) = self.current_instance() {
                    self.page = Some(i.name.clone());
                    self.log_scroll = 0;
                }
            }
            KeyCode::Char('m') => self.goto(TabIndex::Mods),
            KeyCode::Char('e') => self.open_edit(),
            KeyCode::Char('p') => self.export_current(),
            KeyCode::Char('s') | KeyCode::Char('/') => {
                self.goto(TabIndex::SearchMods);
                self.input_mode = InputMode::TypingSearch;
            }
            KeyCode::Char('n') => self.goto(TabIndex::NewInstance),
            KeyCode::Char('c') => {
                if let Some(n) = self.current_instance().map(|i| i.name.clone()) {
                    self.clone_source = Some(n.clone());
                    self.clone_input = format!("{n}-copy");
                    self.input_mode = InputMode::TypingCloneName;
                }
            }
            KeyCode::Char('d') => {
                if let Some(i) = self.current_instance() {
                    self.delete_confirm_target = Some(i.name.clone());
                }
            }
            _ => {}
        }
    }

    /// Scroll the open instance's log by `n` lines (`up` = towards older).
    fn scroll_log(&mut self, n: usize, up: bool) {
        let lines = self.page.as_ref().and_then(|p| self.games.get(p)).map_or(0, |g| g.log.len());
        self.log_scroll = if up { (self.log_scroll + n).min(lines) } else { self.log_scroll.saturating_sub(n) };
    }

    /// Keys on an instance page: launch, kill, scroll the log.
    fn page_key(&mut self, key: KeyEvent) {
        let Some(name) = self.page.clone() else { return };
        let lines = self.games.get(&name).map_or(0, |g| g.log.len());
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.page = None,
            KeyCode::Enter | KeyCode::Char('l') => self.launch(),
            KeyCode::Char('K') | KeyCode::Char('x') => match self.games.get_mut(&name).filter(|g| g.running()) {
                Some(g) => {
                    let forced = g.stopping;
                    g.stop(false);
                    self.set_status(
                        &if forced { format!("Killed '{name}'") } else { format!("Stopping '{name}'… press K again to force") },
                        false,
                    );
                }
                None => self.set_status("Minecraft is not running", false),
            },
            KeyCode::Up | KeyCode::Char('k') => self.log_scroll = (self.log_scroll + 1).min(lines),
            KeyCode::Down | KeyCode::Char('j') => self.log_scroll = self.log_scroll.saturating_sub(1),
            KeyCode::PageUp => self.log_scroll = (self.log_scroll + 20).min(lines),
            KeyCode::PageDown => self.log_scroll = self.log_scroll.saturating_sub(20),
            KeyCode::Char('g') | KeyCode::Home => self.log_scroll = lines,
            KeyCode::Char('G') | KeyCode::End => self.log_scroll = 0,
            KeyCode::Char('m') => self.goto(TabIndex::Mods),
            KeyCode::Char('e') => self.open_edit(),
            KeyCode::Char('p') => self.export_current(),
            _ => {}
        }
    }

    /// Launch the selected instance the configured way: inside the TUI with
    /// its log on the instance page (default), or in a new terminal window.
    fn launch(&mut self) {
        let Some(name) = self.current_instance().map(|i| i.name.clone()) else {
            self.set_status("No instance selected", true);
            return;
        };
        if self.launching.is_some() {
            self.set_status("Already launching — hang on", false);
            return;
        }
        if self.games.get(&name).is_some_and(Game::running) {
            self.set_status(&format!("'{name}' is already running — K stops it"), false);
            return;
        }
        if self.launch_new_term && launcher::find_terminal_emulator().is_some() {
            self.launch_detached();
        } else {
            self.launch_embedded();
        }
    }

    /// Run the game as a child of the TUI: its stdout and stderr stream into
    /// the instance page, and its exit is logged as a session.
    fn launch_embedded(&mut self) {
        let Some((name, ver)) = self.current_instance().map(|i| (i.name.clone(), i.effective_version_id())) else { return };
        let Some(acc) = auth::load_account() else {
            self.open_login();
            return;
        };
        self.launching = Some(name.clone());
        self.start_bg_task(format!("Launching {name}"));
        launcher::set_launch_stage(0);
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let started = async {
                let acc = auth::account_for_launch(&acc).await.map_err(|e| format!("{e:#}"))?;
                let _ = tx.send(DownloadMsg::Session(Ok((acc.clone(), None))));
                let mut l = Launcher::new(None, Some(name.clone()));
                l.spawn_minecraft(&acc, &ver, &[]).await.map_err(|e| format!("Launch failed: {e:#}"))
            }
            .await;
            let mut child = match started {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(DownloadMsg::Launched(Err(e)));
                    return;
                }
            };
            // One reader thread per stream; lines arrive in the UI loop.
            let readers: Vec<Box<dyn std::io::Read + Send>> = [
                child.stdout.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>),
                child.stderr.take().map(|e| Box::new(e) as Box<dyn std::io::Read + Send>),
            ]
            .into_iter()
            .flatten()
            .collect();
            for r in readers {
                let (tx, name) = (tx.clone(), name.clone());
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    for line in std::io::BufReader::new(r).lines().map_while(Result::ok) {
                        let _ = tx.send(DownloadMsg::GameLog(name.clone(), strip_ansi(&line)));
                    }
                });
            }
            let child = Arc::new(Mutex::new(child));
            let _ = tx.send(DownloadMsg::GameStarted(name.clone(), child.clone()));
            let t0 = Instant::now();
            let code = loop {
                // Lock only for the poll, never across the sleep.
                let status = match child.lock() {
                    Ok(mut c) => c.try_wait().ok(),
                    Err(_) => None,
                };
                match status {
                    Some(Some(st)) => break st.code().unwrap_or(-1),
                    Some(None) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                    None => break -1,
                }
            };
            if let Ok(inst) = Instance::load(&name) {
                let _ = inst.record_session(t0.elapsed().as_secs(), code);
            }
            let _ = tx.send(DownloadMsg::GameExited(name, code));
        });
    }

    /// Detached launch: game opens in a new terminal window, TUI keeps running.
    /// Preparing (session, files, Java) runs in the background so the UI
    /// never freezes, even when a first launch has to download the game.
    fn launch_detached(&mut self) {
        if self.launching.is_some() {
            self.set_status("Already launching — hang on", false);
            return;
        }
        let (name, ver) = match self.current_instance() {
            Some(i) => (i.name.clone(), i.effective_version_id()),
            None => {
                self.set_status("No instance selected", true);
                return;
            }
        };
        let Some(acc) = auth::load_account() else {
            self.open_login();
            return;
        };
        if crate::launcher::find_terminal_emulator().is_none() {
            self.launch_embedded();
            return;
        }
        self.launching = Some(name.clone());
        self.start_bg_task(format!("Launching {name}"));
        launcher::set_launch_stage(0);
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let r = async {
                let acc = auth::account_for_launch(&acc).await.map_err(|e| format!("{e:#}"))?;
                let _ = tx.send(DownloadMsg::Session(Ok((acc.clone(), None))));
                let mut l = Launcher::new(None, Some(name.clone()));
                let pid = l.launch_in_new_terminal(&acc, &ver, &[]).await.map_err(|e| format!("Launch failed: {e:#}"))?;
                Ok(format!("'{name}' running in a new window (pid {pid})"))
            }
            .await;
            let _ = tx.send(DownloadMsg::Launched(r));
        });
    }

    async fn mods_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.goto(TabIndex::Instances),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('w') => sel_up(&mut self.mods_list_state),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') => sel_down(&mut self.mods_list_state, self.installed_mods.len()),
            KeyCode::Char(' ') | KeyCode::Enter => {
                let sel = self.mods_list_state.selected().unwrap_or(0);
                if let (Some(inst), Some(m)) = (self.current_instance(), self.installed_mods.get(sel)) {
                    let name = m.display_name.clone();
                    let file = m.filename.clone();
                    match inst.toggle_mod(&file) {
                        Ok(on) => {
                            self.set_status(&format!("{} {}", if on { "Enabled" } else { "Disabled" }, name), false);
                            self.reload_mods();
                        }
                        Err(e) => self.set_status(&format!("Toggle: {e}"), true),
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                let sel = self.mods_list_state.selected().unwrap_or(0);
                if let (Some(inst), Some(m)) = (self.current_instance(), self.installed_mods.get(sel).cloned()) {
                    match inst.remove_mod(&m.filename) {
                        Ok(_) => { self.set_status(&format!("Deleted '{}'", m.display_name), false); self.reload_mods(); }
                        Err(e) => self.set_status(&format!("Delete: {e}"), true),
                    }
                }
            }
            KeyCode::Char('u') => self.update_selected_mod(),
            KeyCode::Char('U') => self.update_all_mods(),
            KeyCode::Char('a') | KeyCode::Char('/') => {
                self.goto(TabIndex::SearchMods);
                self.input_mode = InputMode::TypingSearch;
            }
            _ => {}
        }
    }

    /// Run the current query again (after a filter or connectivity change).
    fn rerun_search(&mut self, is_mod: bool) {
        if is_mod {
            let q = self.mod_search_query.trim().to_string();
            self.execute_mod_search(&q);
        } else {
            let q = self.modpack_search_query.trim().to_string();
            self.execute_modpack_search(&q);
        }
    }

    /// Sort actually sent: an empty query ranks by popularity, since
    /// "relevance" to nothing is meaningless.
    fn effective_sort(q: &str, idx: usize) -> &'static str {
        if q.is_empty() && idx == 0 { "downloads" } else { modpack::SORTS[idx % modpack::SORTS.len()] }
    }

    fn execute_mod_search(&mut self, q: &str) {
        self.searching_mods = true;
        self.mod_search_seq += 1;
        let seq = self.mod_search_seq;
        let (loader, mc) = match (self.mod_compat, self.current_instance()) {
            (true, Some(i)) => (Some(i.loader.clone()), Some(i.mc_version.clone())),
            _ => (None, None),
        };
        let sort = Self::effective_sort(q, self.mod_sort);
        let cat = modpack::MOD_CATEGORIES[self.mod_cat % modpack::MOD_CATEGORIES.len()];
        let q = q.to_string();
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let r = modpack::search(&modpack::SearchQuery {
                kind: "mod",
                query: &q,
                loader: loader.as_deref(),
                mc_version: mc.as_deref(),
                category: Some(cat),
                sort,
                limit: 30,
            })
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(DownloadMsg::ModResults(seq, q, r));
        });
    }

    fn apply_search(&mut self, is_mod: bool, q: &str, r: Result<Vec<ModHit>, String>) {
        let hits = match r {
            Ok(h) => h,
            Err(_) if !self.online => {
                self.set_status("Offline — nothing saved for this search yet", true);
                return;
            }
            Err(e) => {
                self.set_status(&format!("Search failed: {e}"), true);
                return;
            }
        };
        let n = hits.len();
        let icons = hits.iter().filter_map(|h| Some((h.project_id.clone(), h.icon_url.clone()?))).collect();
        self.request_icons(icons);
        let state = if is_mod {
            self.mod_search_results = hits;
            &mut self.mod_search_state
        } else {
            self.modpack_search_results = hits;
            &mut self.modpack_search_state
        };
        sel_clamp(state, n);
        if state.selected().is_none() && n > 0 {
            state.select(Some(0));
        }
        // Startup warm-up results arrive silently.
        if !matches!(self.current_tab, TabIndex::SearchMods | TabIndex::Modpacks) {
            return;
        }
        let kind = if is_mod { "mods" } else { "modpacks" };
        let msg = match (n, q.is_empty()) {
            (0, _) => format!("No {kind} for '{q}'"),
            (_, true) => format!("Popular {kind} — Enter installs, V picks a version"),
            _ => format!("{n} {kind} for '{q}' — Enter installs, V picks a version"),
        };
        self.set_status(&msg, false);
    }

    /// Shared search-list keys; `is_mod` picks mod vs modpack install on Enter.
    async fn search_key(&mut self, key: KeyEvent, is_mod: bool) {
        let len = if is_mod { self.mod_search_results.len() } else { self.modpack_search_results.len() };
        match key.code {
            KeyCode::Esc => {
                let q = if is_mod { &mut self.mod_search_query } else { &mut self.modpack_search_query };
                if q.is_empty() {
                    self.goto(TabIndex::Instances);
                } else {
                    // Clearing the query drops back to the popular list.
                    q.clear();
                    if is_mod { self.execute_mod_search(""); } else { self.execute_modpack_search(""); }
                }
            }
            KeyCode::Char('/') | KeyCode::Char('i') => {
                self.input_mode = if is_mod { InputMode::TypingSearch } else { InputMode::TypingModpackSearch };
            }
            KeyCode::Char('o') => {
                let (sort, q) = if is_mod {
                    self.mod_sort = (self.mod_sort + 1) % modpack::SORTS.len();
                    (self.mod_sort, self.mod_search_query.trim().to_string())
                } else {
                    self.pack_sort = (self.pack_sort + 1) % modpack::SORTS.len();
                    (self.pack_sort, self.modpack_search_query.trim().to_string())
                };
                self.set_status(&format!("Sort: {}", modpack::SORTS[sort]), false);
                if is_mod { self.execute_mod_search(&q); } else { self.execute_modpack_search(&q); }
            }
            KeyCode::Char('f') => {
                let (list, idx) = if is_mod {
                    (modpack::MOD_CATEGORIES, &mut self.mod_cat)
                } else {
                    (modpack::PACK_CATEGORIES, &mut self.pack_cat)
                };
                *idx = (*idx + 1) % list.len();
                let label = list[*idx];
                self.set_status(&format!("Category: {label}"), false);
                self.rerun_search(is_mod);
            }
            KeyCode::Char('c') if is_mod => {
                self.mod_compat = !self.mod_compat;
                self.set_status(
                    if self.mod_compat { "Showing mods for the selected instance" } else { "Showing mods for any loader and version" },
                    false,
                );
                self.rerun_search(true);
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('w') => {
                if is_mod { sel_up(&mut self.mod_search_state); } else { sel_up(&mut self.modpack_search_state); }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') => {
                if is_mod { sel_down(&mut self.mod_search_state, len); } else { sel_down(&mut self.modpack_search_state, len); }
            }
            KeyCode::Char('v') => {
                if let Some((hit, _)) = self.selected_hit() {
                    let (id, title) = (hit.project_id.clone(), hit.title.clone());
                    // Open immediately; the list fills in if still loading.
                    self.sel_changed_at = Instant::now() - std::time::Duration::from_secs(1);
                    self.ensure_versions();
                    self.picker = Some(Picker {
                        project_id: id,
                        title,
                        is_mod,
                        show_all: false,
                        state: ListState::default().with_selected(Some(0)),
                    });
                }
            }
            KeyCode::Enter => {
                if let Some((hit, _)) = self.selected_hit() {
                    let hit = hit.clone();
                    let ver = self.best_version(&hit.project_id, is_mod);
                    self.install_hit(hit, is_mod, ver);
                }
            }
            _ => {}
        }
    }

    /// Install a search hit in the background: a specific version when one
    /// is known, otherwise let the server-side matcher pick.
    fn install_hit(&mut self, hit: ModHit, is_mod: bool, ver: Option<ModrinthVersion>) {
        let label = match &ver {
            Some(v) => format!("{} {}", hit.title, v.version_number),
            None => hit.title.clone(),
        };
        let tx = self.download_tx.clone();
        if is_mod {
            let Some(inst) = self.current_instance().cloned() else {
                self.set_status("No instance selected", true);
                return;
            };
            self.start_bg_task(format!("Installing {label}"));
            self.set_status(&format!("Installing {label} into '{}'…", inst.name), false);
            tokio::spawn(async move {
                let r = match &ver {
                    Some(v) => modpack::install_mod_version(v, &inst).await,
                    None => modpack::install_mod_to_instance(&hit.slug, &inst).await,
                };
                let _ = tx.send(match r {
                    Ok(d) => DownloadMsg::Done(format!("✔ {} {} → '{}'", hit.title, d.version_number, inst.name)),
                    Err(e) => DownloadMsg::Error(format!("Install {}: {e}", hit.title)),
                });
            });
        } else {
            self.start_bg_task(format!("Installing {label}"));
            self.set_status(&format!("Installing modpack {label}…"), false);
            tokio::spawn(async move {
                let r = match &ver {
                    Some(v) => modpack::install_modpack_version(v, None).await,
                    None => modpack::install_modpack(&hit.slug, None).await,
                };
                let _ = tx.send(match r {
                    Ok(i) => DownloadMsg::Done(format!("✔ Modpack ready → instance '{}'", i.name)),
                    Err(e) => DownloadMsg::Error(format!("Modpack {}: {e}", hit.title)),
                });
            });
        }
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Some(p) = &self.picker else { return };
        let (id, is_mod, show_all) = (p.project_id.clone(), p.is_mod, p.show_all);
        let n = self.version_choices(&id, is_mod, show_all).0.len();
        let Some(p) = &mut self.picker else { return };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') => self.picker = None,
            KeyCode::Up | KeyCode::Char('k') => sel_up(&mut p.state),
            KeyCode::Down | KeyCode::Char('j') => sel_down(&mut p.state, n),
            KeyCode::PageUp => { let i = p.state.selected().unwrap_or(0) as isize; sel_updown(&mut p.state, i - 10, n) }
            KeyCode::PageDown => { let i = p.state.selected().unwrap_or(0) as isize; sel_updown(&mut p.state, i + 10, n) }
            KeyCode::Home | KeyCode::Char('g') => sel_updown(&mut p.state, 0, n),
            KeyCode::End | KeyCode::Char('G') => sel_updown(&mut p.state, n as isize - 1, n),
            KeyCode::Char('a') if is_mod => {
                p.show_all = !p.show_all;
                p.state.select(Some(0));
            }
            KeyCode::Enter => {
                let idx = p.state.selected().unwrap_or(0);
                let ver = self.version_choices(&id, is_mod, show_all).0.get(idx).map(|v| (*v).clone());
                let hit = self.selected_hit().map(|(h, _)| h.clone());
                if let (Some(ver), Some(hit)) = (ver, hit) {
                    self.picker = None;
                    self.install_hit(hit, is_mod, Some(ver));
                }
            }
            _ => {}
        }
    }

    fn execute_modpack_search(&mut self, q: &str) {
        self.searching_modpacks = true;
        self.pack_search_seq += 1;
        let seq = self.pack_search_seq;
        let sort = Self::effective_sort(q, self.pack_sort);
        let cat = modpack::PACK_CATEGORIES[self.pack_cat % modpack::PACK_CATEGORIES.len()];
        let q = q.to_string();
        let tx = self.download_tx.clone();
        tokio::spawn(async move {
            let r = modpack::search(&modpack::SearchQuery {
                kind: "modpack",
                query: &q,
                loader: None,
                mc_version: None,
                category: Some(cat),
                sort,
                limit: 30,
            })
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(DownloadMsg::PackResults(seq, q, r));
        });
    }

    // ── wizard ─────────────────────────────────────────────────────────────
    async fn wizard_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.new_inst_field = (self.new_inst_field + 1) % 5,
            KeyCode::BackTab | KeyCode::Up => self.new_inst_field = (self.new_inst_field + 4) % 5,
            // First Esc stops typing but stays on the tab; Esc again (Normal
            // mode) goes back to Instances. You never get stuck.
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Backspace => {
                match self.new_inst_field {
                    0 => { self.new_inst_name.pop(); }
                    1 => { self.new_inst_version_input.pop(); self.update_version_matches(); }
                    3 => { self.new_inst_ram.pop(); }
                    _ => {}
                }
            }
            KeyCode::Left => {
                match self.new_inst_field {
                    2 if self.new_inst_loader_idx > 0 => self.new_inst_loader_idx -= 1,
                    1 if self.matching_version_idx > 0 => {
                        self.matching_version_idx -= 1;
                        if let Some(v) = self.matching_versions.get(self.matching_version_idx) {
                            self.new_inst_version_input = v.clone();
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Right => {
                match self.new_inst_field {
                    2 if self.new_inst_loader_idx + 1 < LOADERS.len() => self.new_inst_loader_idx += 1,
                    1 if self.matching_version_idx + 1 < self.matching_versions.len() => {
                        self.matching_version_idx += 1;
                        if let Some(v) = self.matching_versions.get(self.matching_version_idx) {
                            self.new_inst_version_input = v.clone();
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Enter => {
                if self.new_inst_field == 4 || self.new_inst_field == 3 {
                    self.create_instance_from_wizard().await;
                } else {
                    self.new_inst_field = (self.new_inst_field + 1) % 5;
                }
            }
            KeyCode::Char(c) => {
                match self.new_inst_field {
                    0 => self.new_inst_name.push(c),
                    1 => { self.new_inst_version_input.push(c); self.update_version_matches(); }
                    2 if c == ' ' => self.new_inst_loader_idx = (self.new_inst_loader_idx + 1) % LOADERS.len(),
                    3 => self.new_inst_ram.push(c),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    async fn create_instance_from_wizard(&mut self) {
        let name = self.new_inst_name.trim().to_string();
        if name.is_empty() {
            self.set_status("Enter an instance name", true);
            self.new_inst_field = 0;
            return;
        }
        let mc = self.new_inst_version_input.trim().to_string();
        if mc.is_empty() {
            self.set_status("Enter a version (e.g. 1.21.1)", true);
            self.new_inst_field = 1;
            return;
        }
        let loader = LOADERS[self.new_inst_loader_idx].to_string();
        let ram = self.new_inst_ram.clone();
        match Instance::create(&name, &mc, &loader, None) {
            Ok(mut inst) => {
                inst.ram_max = Some(ram);
                let _ = inst.save();
                if loader == "fabric" {
                    self.start_bg_task(format!("Fabric {mc}"));
                    let tx = self.download_tx.clone();
                    tokio::spawn(async move {
                        match modpack::install_fabric(&mc).await {
                            Ok(_) => { let _ = tx.send(DownloadMsg::Done(format!("✔ '{name}' ({mc})"))); }
                            Err(e) => { let _ = tx.send(DownloadMsg::Error(format!("Fabric: {e}"))); }
                        }
                    });
                } else {
                    self.set_status(&format!("✔ '{name}' ({mc})"), false);
                }
                self.reload_instances();
                self.new_inst_name.clear();
                self.input_mode = InputMode::Normal;
                self.goto(TabIndex::Instances);
            }
            Err(e) => self.set_status(&format!("Create: {e}"), true),
        }
    }

    async fn new_instance_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.goto(TabIndex::Instances),
            KeyCode::Char('i') | KeyCode::Enter | KeyCode::Char(' ') => {
                self.input_mode = InputMode::TypingNewInstance;
            }
            _ => {}
        }
    }

    async fn settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.goto(TabIndex::Instances),
            KeyCode::Char('y') => {
                let next = self.theme.next().1;
                self.theme = next;
                self.set_status(&format!("Theme: {}", self.theme.name), false);
            }
            KeyCode::Char('m') => self.start_login(),
            KeyCode::Char('t') => {
                let on = !self.launch_new_term;
                match crate::config::set_new_terminal(on) {
                    Ok(_) => {
                        self.launch_new_term = on;
                        self.config.launch_new_terminal = on;
                        self.set_status(
                            if on { "Games now open in a new terminal window" } else { "Games now run inside Mirage — log on the instance page" },
                            false,
                        );
                    }
                    Err(e) => self.set_status(&format!("Could not save: {e}"), true),
                }
            }
            _ => {}
        }
    }

    // ── render ─────────────────────────────────────────────────────────────
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.term_width = area.width;
        frame.render_widget(Block::default().style(Style::default().bg(self.theme.bg).fg(self.theme.fg)), area);

        // Every band has a fixed height, so starting a download never shifts
        // the layout under the user's cursor.
        let [head, tabs, body, task, foot] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(area);
        self.body_y = body.y;
        self.tab_hits.clear();
        self.list_hits.clear();
        self.render_header(frame, head);
        self.render_tabs(frame, tabs);
        match self.current_tab {
            TabIndex::Instances if self.page.is_some() => self.render_instance_page(frame, body),
            TabIndex::Instances => self.render_instances(frame, body),
            TabIndex::Mods => self.render_mods(frame, body),
            TabIndex::SearchMods => self.render_search(frame, body, true),
            TabIndex::Modpacks => self.render_search(frame, body, false),
            TabIndex::NewInstance => self.render_wizard(frame, body),
            TabIndex::Settings => self.render_settings(frame, body),
        }
        self.render_task(frame, task);
        self.render_footer(frame, foot);
        self.render_modals(frame, area);
    }

    /// Row cursor: a pulsing accent bar on the selected row, blank otherwise.
    fn marker(&self, sel: bool) -> Span<'static> {
        if !sel {
            return Span::raw("  ");
        }
        Span::styled("▍ ", Style::default().fg(self.theme.accent).bold())
    }

    /// Placeholder result rows with a soft shimmer sweeping down them, shown
    /// while the first search is in flight so the list never looks empty.
    fn skeleton_rows(&self, area: Rect) -> Vec<Line<'static>> {
        let w = area.width as usize;
        let bar = |len: usize, lit: f32| {
            let c = if self.cfg.anim { lerp_rgb(self.theme.bg_sel, self.theme.border, lit) } else { self.theme.bg_sel };
            Span::styled(" ".repeat(len.min(w)), Style::default().bg(c))
        };
        let phase = if self.cfg.anim { (self.frame % 60) as f32 / 60.0 * 1.6 - 0.3 } else { 0.0 };
        let mut lines = Vec::new();
        for r in 0..(area.height / 2) as usize {
            let lit = (1.0 - ((r as f32 / 8.0) - phase).abs() * 3.0).clamp(0.0, 1.0);
            let title = 14 + (r * 7) % 13;
            let sub = 22 + (r * 11) % 17;
            lines.push(Line::from(vec![Span::raw("  "), bar(4, lit), Span::raw(" "), bar(title, lit)]));
            lines.push(Line::from(vec![Span::raw("  "), bar(4, lit), Span::raw(" "), bar(sub, lit * 0.6)]));
        }
        lines
    }

    /// Spinner glyph, advanced on the shared frame clock.
    fn spin(&self) -> char {
        if !self.cfg.anim {
            return SPIN[0];
        }
        SPIN[(self.frame / 2) as usize % SPIN.len()]
    }

    /// Border colour for body panels: a short accent glow right after a tab
    /// change that settles back to the hairline colour.
    fn panel_border(&self) -> Color {
        if !self.cfg.anim || self.entrance <= 0.0 {
            return self.theme.border;
        }
        lerp_rgb(self.theme.border, self.theme.accent, ease_out(self.entrance) * 0.5)
    }

    /// Background for the selected row: a quick tinted flash that settles
    /// into the plain selection colour.
    fn row_bg(&self) -> Color {
        if !self.cfg.anim {
            return self.theme.bg_sel;
        }
        lerp_rgb(self.theme.bg_sel, self.theme.accent, (1.0 - ease_out(self.sel_anim)) * 0.28)
    }

    /// Accent used to tag a loader ("fabric" / "quilt" / …) everywhere it appears.
    fn loader_color(&self, loader: &str) -> Color {
        match loader {
            "fabric" => self.theme.cyan,
            "quilt" => Color::Rgb(180, 100, 255),
            "forge" | "neoforge" => self.theme.warn,
            _ => self.theme.accent,
        }
    }

    fn render_task(&self, frame: &mut Frame, area: Rect) {
        let busy = self.bg_download_active.is_some();
        if !busy {
            // Idle: a hairline rule, so the band still reads as structure.
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "─".repeat(area.width as usize),
                    Style::default().fg(self.theme.border),
                ))),
                area,
            );
            return;
        }
        let title = self.bg_download_active.clone().unwrap_or_default();
        let secs = self.bg_task_start.map(|x| x.elapsed().as_secs_f64()).unwrap_or(0.0);
        let extra = if self.bg_task_count > 1 { format!(" +{}", self.bg_task_count - 1) } else { String::new() };
        let snap = download::progress().snapshot();
        let frac = snap.fraction();

        // Right side: real numbers whenever the downloader knows them.
        let mut stats: Vec<String> = Vec::new();
        if snap.bytes_total > 0 {
            stats.push(format!("{} / {}", fmt_size(snap.bytes), fmt_size(snap.bytes_total)));
        } else if snap.files_total > 1 {
            stats.push(format!("{}/{} files", snap.files, snap.files_total));
        }
        if self.speed > 1024.0 {
            stats.push(format!("{}/s", fmt_size(self.speed as u64)));
        }
        let eta = match frac {
            Some(_) if snap.bytes_total > 0 && self.speed > 1024.0 => {
                Some(snap.bytes_total.saturating_sub(snap.bytes) as f64 / self.speed)
            }
            Some(f) if f > 0.03 && f < 1.0 => Some(secs * (1.0 - f) / f),
            _ => None,
        };
        match eta {
            Some(e) => stats.push(format!("{} left", fmt_secs(e))),
            None => stats.push(format!("{} elapsed", fmt_secs(secs))),
        }
        let stats = stats.join("  ·  ");

        let label = format!(" {} {title}{extra}  ", self.spin());
        let sub = trunc(&snap.label, 28);
        let right_w = stats.chars().count() as u16 + 2;
        let left_w = (label.chars().count() + sub.chars().count()) as u16 + 2;
        let bar_w = area.width.saturating_sub(left_w + right_w + 2).min(48) as usize;

        let mut spans = vec![
            Span::styled(label, Style::default().fg(self.theme.primary).bold()),
            Span::styled(sub, Style::default().fg(self.theme.muted)),
            Span::raw("  "),
        ];
        if bar_w >= 6 {
            match frac {
                Some(f) => spans.extend(gauge_spans(&self.theme, f as f32, 1.0, bar_w.saturating_sub(5))),
                None => {
                    // Unknown size: a travelling comet reads as "working".
                    let period = bar_w + 12;
                    let head = if self.cfg.anim { (self.frame as usize) % period } else { 0 };
                    for i in 0..bar_w {
                        let d = (head + period - i) % period;
                        spans.push(if self.cfg.anim && d < 12 {
                            let t = 1.0 - d as f32 / 12.0;
                            Span::styled("━", Style::default().fg(lerp_rgb(self.theme.border, ramp(&self.theme, 1.0 - t), ease_out(t))))
                        } else {
                            Span::styled("─", Style::default().fg(self.theme.border))
                        });
                    }
                }
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("{stats}  "), Style::default().fg(self.theme.secondary))))
                .alignment(Alignment::Right),
            area,
        );
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let [l, r] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
        let head = vec![
            Span::styled("  ⛏ ", Style::default().fg(self.theme.accent).bold()),
            Span::styled("Mirage", Style::default().fg(self.theme.primary).bold()),
            Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), self.theme.muted),
        ];
        frame.render_widget(Paragraph::new(Line::from(head)), l);
        let (name, note, tone) = match &self.active_account {
            Some(a) if !self.online => (a.username.as_str(), "offline", self.theme.warn),
            Some(a) if a.is_expired() => (a.username.as_str(), "session expired", self.theme.warn),
            Some(a) => (a.username.as_str(), "signed in", self.theme.muted),
            None => ("sign in", "press 6", self.theme.error),
        };
        let [text, face] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(ICON_SMALL as u16 + 2)]).areas(r);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(name.to_string(), Style::default().fg(self.theme.fg).bold())),
                Line::from(Span::styled(note, Style::default().fg(tone))),
            ])
            .alignment(Alignment::Right),
            text,
        );
        if let Some(a) = &self.active_account {
            let rect = Rect { x: face.x + 1, y: face.y, width: ICON_SMALL as u16, height: 2 };
            self.draw_icon(frame, rect, Some(&format!("head:{}", a.uuid)), &a.username, false, self.theme.bg);
        }
    }

    fn render_tabs(&mut self, frame: &mut Frame, area: Rect) {
        let [labels, rail] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        let titles = TabIndex::ALL;
        // Every chip carries its own hotkey ("1 Instances"), so the number is
        // always adjacent to the tab it selects.
        let widths: Vec<u16> = titles.iter().map(|t| t.len() as u16 + 4).collect();
        let gap = 2u16;
        let step: Vec<u16> = widths.iter().map(|w| w + gap).collect();
        let total: u16 = step.iter().sum::<u16>() - gap;
        let x0 = labels.x + labels.width.saturating_sub(total) / 2;
        let active = self.current_tab as usize;
        let (mx, my) = (self.mouse_x, self.mouse_y);

        let mut spans = Vec::new();
        // Centre the strip by hand: the pad is part of the same line, so the
        // hit-test rectangles below stay honest.
        if x0 > labels.x {
            spans.push(Span::styled(" ".repeat((x0 - labels.x) as usize), self.theme.fg));
        }
        let mut x = x0;
        for (i, t) in titles.iter().enumerate() {
            let w = widths[i];
            self.tab_hits.push((labels.y, x, x + w, TabIndex::from_index(i)));
            let hovered = my == labels.y && mx >= x && mx < x + w;
            let text = format!(" {} {t} ", i + 1);
            let chip = if i == active {
                let t = ease_out(self.tab_anim);
                let fill = lerp_rgb(self.theme.bg_sel, self.theme.accent, t);
                Span::styled(text, Style::default().fg(lerp_rgb(self.theme.primary, self.theme.bg, t)).bg(fill).bold())
            } else if hovered {
                Span::styled(
                    text,
                    Style::default().fg(self.theme.primary).bg(self.theme.bg_sel),
                )
            } else {
                Span::styled(text, Style::default().fg(self.theme.secondary))
            };
            spans.push(chip);
            if i + 1 < titles.len() {
                spans.push(Span::styled(" ".repeat(gap as usize), self.theme.fg));
            }
            x += w + gap;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), labels);

        // Rail with a bright segment that slides from the previous tab to the
        // new one, so switching tabs reads as movement rather than a cut.
        let eased = ease_out(self.tab_anim);
        let start_of =
            |i: usize| -> u16 { x0 + step[..i.min(step.len())].iter().sum::<u16>() };
        // Fractional tab index → column, so a slide interrupted mid-way
        // resumes from where the bar actually is.
        let px_of = |f: f32| -> f32 {
            let lo = f.floor().max(0.0) as usize;
            let hi = (lo + 1).min(widths.len() - 1);
            lerp(start_of(lo) as f32, start_of(hi) as f32, f - f.floor())
        };
        let head = lerp(px_of(self.tab_pos_from), start_of(active) as f32, eased) as u16;
        let span = widths[active];
        let cells: Vec<Span> = (0..labels.width)
            .map(|c| {
                let x = labels.x + c;
                if x >= head && x < head.saturating_add(span) {
                    let t = (x - head) as f32 / span as f32;
                    return Span::styled("━", Style::default().fg(ramp(&self.theme, t)));
                }
                Span::styled("─", Style::default().fg(self.theme.border))
            })
            .collect();
        frame.render_widget(Paragraph::new(Line::from(cells)), rail);
    }

    fn render_instances(&mut self, frame: &mut Frame, area: Rect) {
        let [l, r] = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);
        let (sel_bg, border) = (self.row_bg(), self.panel_border());
        let selected = self.instance_list_state.selected();
        let width = inner(l).width as usize;
        let items: Vec<ListItem> = self
            .instances
            .iter()
            .enumerate()
            .map(|(i, inst)| {
                let sel = selected == Some(i);
                let n = self.instance_mods_counts.get(i).copied().unwrap_or(0);
                let name_style = if sel {
                    Style::default().fg(self.theme.primary).bold()
                } else {
                    Style::default().fg(self.theme.fg)
                };
                let tw = width.saturating_sub(2 + 5 + 10).max(6);
                let mut sub = format!("{}  ·  {}", inst.mc_version, if n == 1 { "1 mod".into() } else { format!("{n} mods") });
                if self.games.get(&inst.name).is_some_and(Game::running) {
                    sub.push_str("  ·  ● running");
                } else if let Some(ago) = inst.last_played.as_deref().and_then(fmt_ago) {
                    sub.push_str(&format!("  ·  {ago}"));
                }
                ListItem::new(vec![
                    Line::from(vec![
                        self.marker(sel),
                        Span::raw("     "),
                        Span::styled(pad(&trunc(&inst.name, tw), tw), name_style),
                        Span::styled(format!("{:>9}", inst.loader), Style::default().fg(self.loader_color(&inst.loader))),
                    ]),
                    Line::from(vec![
                        Span::raw("       "),
                        Span::styled(trunc(&sub, tw + 9), Style::default().fg(self.theme.muted)),
                    ]),
                ])
                .style(row_style(sel_bg, sel))
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(block(&self.theme, " Instances ", border))
                .highlight_style(row_style(sel_bg, true)),
            l,
            &mut self.instance_list_state,
        );
        let off = self.instance_list_state.offset();
        push_hits(&mut self.list_hits, inner(l), off, self.instances.len(), 2);
        self.draw_row_icons(frame, l, off, self.instances.len(), selected, |i| {
            let inst = &self.instances[i];
            (inst.modrinth_project.as_deref(), inst.name.as_str())
        });

        let Some(inst) = self.current_instance().cloned() else {
            let mut v = vec![Line::from(""), Line::from("")];
            v.extend(LOGO.iter().map(|row| Line::from(gradient_spans(&self.theme, row, 0.0, true)).centered()));
            v.push(Line::from(""));
            v.push(Line::from(Span::styled("Let's get you playing", self.theme.secondary).bold()).centered());
            v.push(Line::from(""));
            v.extend(self.setup_steps());
            v.push(Line::from(""));
            v.push(Line::from(Span::styled("press N to create an instance", self.theme.muted)).centered());
            frame.render_widget(Paragraph::new(v).block(block(&self.theme, " Details ", border)), r);
            return;
        };
        frame.render_widget(block(&self.theme, " Details ", border), r);
        let body = inner(r);
        let [top, status, mods, keys] = Layout::vertical([
            Constraint::Length(9),
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .areas(body);

        // Header: icon, name, version, one line of stat chips.
        let n_mods = self.installed_mods.len();
        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(inst.name.clone(), Style::default().fg(self.theme.primary).bold())),
            Line::from(vec![
                Span::styled(inst.mc_version.clone(), Style::default().fg(self.theme.cyan)),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(inst.loader.clone(), Style::default().fg(self.loader_color(&inst.loader))),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(
                    format!("{} – {} RAM", inst.ram_min.as_deref().unwrap_or("2G"), inst.ram_max.as_deref().unwrap_or("4G")),
                    self.theme.secondary,
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("⏱ ", Style::default().fg(self.theme.warn)),
                Span::styled(fmt_playtime(inst.playtime_seconds), Style::default().fg(self.theme.primary).bold()),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(
                    match inst.last_played.as_deref().and_then(fmt_ago) {
                        Some(ago) => format!("⟳ {ago}"),
                        None => "⟳ never launched".into(),
                    },
                    self.theme.secondary,
                ),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(format!("{n_mods} mods"), self.theme.secondary),
            ]),
        ];
        if let Some(note) = inst.notes.as_deref().filter(|n| !n.is_empty()) {
            lines.push(Line::from(Span::styled(trunc(note, top.width.saturating_sub(22) as usize), self.theme.muted)));
        }
        self.render_icon_header(frame, top, inst.modrinth_project.as_deref(), &inst.name, lines);

        // Play button doubles as status: launch steps, running, last result, or ready.
        let running = self.games.get(&inst.name).is_some_and(Game::running);
        let (tone, label): (Color, Vec<Span<'static>>) = if self.launching.as_deref() == Some(inst.name.as_str()) {
            (self.theme.primary, self.stage_spans())
        } else if running {
            (self.theme.accent, vec![Span::styled("● Running — Enter opens its log", Style::default().fg(self.theme.accent).bold())])
        } else if let Some((_, text)) = self.last_session.as_ref().filter(|(ok, _)| !*ok) {
            (self.theme.error, vec![Span::styled(format!("✖ {}", trunc(text, status.width.saturating_sub(8) as usize)), Style::default().fg(self.theme.error))])
        } else {
            (self.theme.accent, vec![Span::styled("▶  PLAY  ·  Enter", Style::default().fg(self.theme.accent).bold())])
        };
        frame.render_widget(
            Paragraph::new(Line::from(label))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(tone))),
            pad_rect(status, 1),
        );

        // Mods preview: enabled ones bright, disabled ones dim.
        let on = self.installed_mods.iter().filter(|m| m.enabled).count();
        let mut m = vec![sec(&self.theme, &format!("Mods · {on} on / {}", self.installed_mods.len()))];
        if self.installed_mods.is_empty() {
            m.push(Line::from(Span::styled("  none yet — press S to find some", self.theme.muted)));
        } else {
            // Two fixed columns, as many rows as fit; the rest is "+N more".
            let col_w = (mods.width.saturating_sub(4) / 2).max(10) as usize;
            let rows = mods.height.saturating_sub(4).max(1) as usize;
            let total = self.installed_mods.len();
            let shown = total.min(rows * 2);
            let shown = if shown < total { shown - 1 } else { shown };
            let cell = |md: &InstalledMod| -> Vec<Span<'static>> {
                let title = self
                    .mod_info
                    .get(mod_key(&md.filename))
                    .and_then(|o| o.as_ref())
                    .map(|x| x.project.title.clone())
                    .unwrap_or_else(|| md.display_name.clone());
                let (dot, tone, text) = if md.enabled {
                    ("● ", self.theme.accent, self.theme.fg)
                } else {
                    ("○ ", self.theme.muted, self.theme.muted)
                };
                vec![
                    Span::styled(dot, Style::default().fg(tone)),
                    Span::styled(pad(&trunc(&title, col_w - 3), col_w - 2), Style::default().fg(text)),
                ]
            };
            let mut cells: Vec<Vec<Span>> = self.installed_mods[..shown].iter().map(cell).collect();
            if shown < total {
                cells.push(vec![Span::styled(format!("+{} more — M opens the list", total - shown), self.theme.muted)]);
            }
            for pair in cells.chunks(2) {
                let mut line = vec![Span::raw("  ")];
                for c in pair {
                    line.extend(c.iter().cloned());
                }
                m.push(Line::from(line));
            }
        }
        frame.render_widget(Paragraph::new(m), mods);

        let mut k = key_hint(&self.theme, "Enter", "open", self.theme.accent);
        k.extend(key_hint(&self.theme, "E", "settings", self.theme.cyan));
        k.extend(key_hint(&self.theme, "M", "mods", self.theme.cyan));
        k.extend(key_hint(&self.theme, "S", "search", self.theme.cyan));
        k.extend(key_hint(&self.theme, "C", "clone", self.theme.cyan));
        k.extend(key_hint(&self.theme, "P", "export", self.theme.cyan));
        k.extend(key_hint(&self.theme, "D", "delete", self.theme.error));
        frame.render_widget(Paragraph::new(Line::from(k)).wrap(Wrap { trim: true }), pad_rect(keys, 1));
    }
    /// Draw a project icon into `rect` (4×2 or 16×8 cells): real pixels via
    /// the terminal's graphics protocol when available, blended half blocks
    /// otherwise, and an initial tile while loading or when there is none.
    fn draw_icon(&self, frame: &mut Frame, rect: Rect, project_id: Option<&str>, title: &str, large: bool, bg: Color) {
        let cols = if large { ICON_LARGE } else { ICON_SMALL };
        let rect = rect.intersection(frame.area());
        if rect.width < cols as u16 || rect.height < cols as u16 / 2 {
            return;
        }
        match project_id.and_then(|id| self.icons.get(id)) {
            Some(Some(icon)) => match if large { &icon.gfx_large } else { &icon.gfx_small } {
                Some(p) => frame.render_widget(Image::new(p), rect),
                None => frame.render_widget(Paragraph::new(icon_lines(if large { &icon.large } else { &icon.small }, bg)), rect),
            },
            _ => {
                let tile = icon_placeholder(&self.theme, title, cols, self.cell_aspect);
                let h = (tile.len() as u16).min(rect.height);
                let rect = Rect { y: rect.y + (rect.height - h) / 2, height: h, ..rect };
                frame.render_widget(Paragraph::new(tile), rect);
            }
        }
    }

    /// Icons for the visible two-line rows of a list drawn at `list`.
    /// `row(i)` gives (project id, title) for list index `i`.
    fn draw_row_icons<'a>(
        &self,
        frame: &mut Frame,
        list: Rect,
        offset: usize,
        count: usize,
        selected: Option<usize>,
        row: impl Fn(usize) -> (Option<&'a str>, &'a str),
    ) {
        let body = inner(list);
        for r in 0..(body.height / 2) as usize {
            let i = offset + r;
            if i >= count {
                break;
            }
            let bg = if selected == Some(i) { self.row_bg() } else { self.theme.surface };
            let (id, title) = row(i);
            let rect = Rect { x: body.x + 2, y: body.y + r as u16 * 2, width: ICON_SMALL as u16, height: 2 };
            self.draw_icon(frame, rect, id, title, false, bg);
        }
    }

    /// Launch steps as `✔ Signing in › ⠋ Game files › ○ Java › ○ Starting`.
    fn stage_spans(&self) -> Vec<Span<'static>> {
        let stage = launcher::launch_stage();
        let mut spans = Vec::new();
        for (i, name) in launcher::LAUNCH_STAGES.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ›  ", self.theme.muted));
            }
            spans.push(match i.cmp(&stage) {
                std::cmp::Ordering::Less => Span::styled(format!("✔ {name}"), Style::default().fg(self.theme.accent)),
                std::cmp::Ordering::Equal => {
                    Span::styled(format!("{} {name}", self.spin()), Style::default().fg(self.theme.primary).bold())
                }
                std::cmp::Ordering::Greater => Span::styled(format!("○ {name}"), self.theme.muted),
            });
        }
        spans
    }

    /// One instance: its header, launch / stop, and the game's live log.
    fn render_instance_page(&mut self, frame: &mut Frame, area: Rect) {
        let name = self.page.clone().unwrap_or_default();
        let Some(inst) = self.instances.iter().find(|i| i.name == name).cloned() else {
            self.page = None;
            return self.render_instances(frame, area);
        };
        let border = self.panel_border();
        let [head, log_area] = Layout::vertical([Constraint::Length(11), Constraint::Fill(1)]).areas(area);
        let game = self.games.get(&name);
        let running = game.is_some_and(Game::running);

        let mut keys = vec![Span::raw(" ")];
        if running {
            keys.extend(key_hint(&self.theme, "K", "stop Minecraft", self.theme.error));
        } else {
            keys.extend(key_hint(&self.theme, "Enter", "launch", self.theme.accent));
        }
        keys.extend(key_hint(&self.theme, "E", "settings", self.theme.cyan));
        keys.extend(key_hint(&self.theme, "M", "mods", self.theme.cyan));
        keys.extend(key_hint(&self.theme, "P", "export", self.theme.cyan));
        keys.extend(key_hint(&self.theme, "Esc", "back", self.theme.secondary));
        frame.render_widget(
            block(&self.theme, format!(" {name} "), border).title_bottom(Line::from(keys)),
            head,
        );

        let state: Line<'static> = if self.launching.as_deref() == Some(name.as_str()) {
            Line::from(self.stage_spans())
        } else if let Some(g) = game.filter(|g| g.running()) {
            let pid = g.child.lock().map(|c| c.id()).unwrap_or(0);
            Line::from(vec![
                Span::styled("● Running", Style::default().fg(self.theme.accent).bold()),
                Span::styled(format!("  ·  {}  ·  pid {pid}", fmt_secs(g.started.elapsed().as_secs_f64())), self.theme.secondary),
            ])
        } else if let Some(code) = game.and_then(|g| g.exit) {
            if clean_exit(code) {
                Line::from(Span::styled("✔ Closed normally", Style::default().fg(self.theme.accent)))
            } else {
                let text = self.last_session.as_ref().filter(|(ok, _)| !ok).map(|(_, t)| t.clone()).unwrap_or(format!("exited with code {code}"));
                Line::from(Span::styled(format!("✖ {text}"), Style::default().fg(self.theme.error)))
            }
        } else {
            Line::from(Span::styled(
                if self.launch_new_term { "Ready · Enter opens the game in a new window" } else { "Ready · Enter launches here, with the log below" },
                self.theme.secondary,
            ))
        };
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(inst.mc_version.clone(), Style::default().fg(self.theme.cyan)),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(inst.loader.clone(), Style::default().fg(self.loader_color(&inst.loader))),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(
                    format!("{} – {} RAM", inst.ram_min.as_deref().unwrap_or("2G"), inst.ram_max.as_deref().unwrap_or("4G")),
                    self.theme.secondary,
                ),
                Span::styled("  ·  ", self.theme.muted),
                Span::styled(format!("{} mods", self.installed_mods.iter().filter(|m| m.enabled).count()), self.theme.secondary),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("⏱ ", Style::default().fg(self.theme.warn)),
                Span::styled(fmt_playtime(inst.playtime_seconds), Style::default().fg(self.theme.primary).bold()),
                Span::styled(" played", self.theme.secondary),
                Span::styled(
                    match inst.last_played.as_deref().and_then(fmt_ago) {
                        Some(ago) => format!("   ⟳ last played {ago}"),
                        None => "   ⟳ never launched".into(),
                    },
                    self.theme.muted,
                ),
            ]),
            Line::from(""),
            state,
        ];
        self.render_icon_header(frame, inner(head), inst.modrinth_project.as_deref(), &inst.name, lines);

        // Log: hard-wrapped so scrolling counts exact rows.
        let no_log = VecDeque::new();
        let log = game.map_or(&no_log, |g| &g.log);
        self.log_scroll = self.log_scroll.min(log.len());
        let follow = if self.log_scroll == 0 {
            " following ".to_string()
        } else {
            format!(" ↑ {} lines up · End follows ", self.log_scroll)
        };
        let log_block = block(&self.theme, format!(" Log · {} lines ", log.len()), border)
            .title(Line::from(Span::styled(follow, self.theme.muted)).right_aligned())
            .title_bottom(Line::from(Span::styled(" ↑↓ scroll · PgUp/PgDn page · g top · End follow ", self.theme.muted)));
        let body = pad_rect(inner(log_area), 1);
        frame.render_widget(log_block, log_area);
        if log.is_empty() {
            let (title, hint) = match (running, self.launch_new_term) {
                (true, _) => ("Waiting for output…", ""),
                (false, false) => ("No log yet", "press Enter to launch — Minecraft's output streams here"),
                (false, true) => ("Games open in their own window", "switch to in-app launch with T in Settings to see the log here"),
            };
            frame.render_widget(Paragraph::new(empty_panel(&self.theme, title, hint)), body);
            return;
        }
        let (w, h) = (body.width.max(1) as usize, body.height as usize);
        let end = log.len() - self.log_scroll;
        let mut rows: Vec<Line> = Vec::new();
        let mut i = end;
        while i > 0 && rows.len() < h {
            i -= 1;
            let style = log_style(&self.theme, &log[i]);
            let mut chunk: Vec<Line> = hard_wrap(&log[i], w).into_iter().map(|r| Line::styled(r, style)).collect();
            chunk.append(&mut rows);
            rows = chunk;
        }
        let skip = rows.len().saturating_sub(h);
        frame.render_widget(Paragraph::new(rows.split_off(skip)), body);
    }

    fn render_mods(&mut self, frame: &mut Frame, area: Rect) {
        let name = self.current_instance().map(|i| i.name.clone()).unwrap_or_else(|| "None".into());
        let wide = area.width >= 80;
        let [l, r] = if wide {
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area)
        } else {
            [area, Rect::default()]
        };
        let (sel_bg, border) = (self.row_bg(), self.panel_border());
        let selected = self.mods_list_state.selected();
        let enabled = self.installed_mods.iter().filter(|m| m.enabled).count();
        let width = inner(l).width as usize;
        let items: Vec<ListItem> = if self.installed_mods.is_empty() {
            vec![ListItem::new(Line::from(vec![
                Span::styled("  ", self.theme.fg),
                Span::styled("no mods installed — press A to browse Modrinth", self.theme.secondary),
            ]))]
        } else {
            self.installed_mods
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let sel = selected == Some(i);
                    let info = self.mod_info.get(mod_key(&m.filename)).and_then(|o| o.as_ref());
                    let title = info.map(|x| x.project.title.clone()).unwrap_or_else(|| m.display_name.clone());
                    // ON reads as a filled dot, OFF as a hollow one.
                    let (mark, tone) = if m.enabled { ("●", self.theme.accent) } else { ("○", self.theme.muted) };
                    let dl = info.and_then(|x| x.project.downloads).map(|d| format!("⬇ {}", fmt_dl(d))).unwrap_or_default();
                    let tw = width.saturating_sub(2 + 4 + 3 + 10).max(6);
                    let has_update = self.update_for(&name, &m.filename).is_some();
                    let title_style = match (m.enabled, sel) {
                        (false, _) => Style::default().fg(self.theme.muted),
                        (true, true) => Style::default().fg(self.theme.primary).bold(),
                        (true, false) => Style::default().fg(self.theme.fg),
                    };
                    let sub = match info {
                        Some(x) => format!("{}  ·  {}", x.version_number, fmt_size(m.size_bytes)),
                        None => format!("{}  ·  {}", m.filename, fmt_size(m.size_bytes)),
                    };
                    let mut l1 = vec![self.marker(sel), Span::raw("    ")];
                    l1.push(Span::styled(format!(" {mark} "), Style::default().fg(tone)));
                    if has_update {
                        l1.push(Span::styled(pad(&trunc(&title, tw - 2), tw - 2), title_style));
                        l1.push(Span::styled("↑ ", Style::default().fg(self.theme.cyan).bold()));
                    } else {
                        l1.push(Span::styled(pad(&trunc(&title, tw), tw), title_style));
                    }
                    l1.push(Span::styled(format!("{dl:>9}"), Style::default().fg(self.theme.warn)));
                    let mut l2 = vec![Span::raw("         ")];
                    l2.push(Span::styled(trunc(&sub, tw + 9), Style::default().fg(self.theme.muted)));
                    ListItem::new(vec![Line::from(l1), Line::from(l2)]).style(row_style(sel_bg, sel))
                })
                .collect()
        };
        let mods_title = format!(" Mods · {name} ");
        let updates = self.installed_mods.iter().filter(|m| self.update_for(&name, &m.filename).is_some()).count();
        let counter = if updates > 0 {
            format!(" {enabled}/{} on · {updates} update{} · U updates all ", self.installed_mods.len(), if updates == 1 { "" } else { "s" })
        } else {
            format!(" {enabled}/{} on ", self.installed_mods.len())
        };
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    block(&self.theme, mods_title, border)
                        .title_bottom(Line::from(Span::styled(counter, Style::default().fg(self.theme.muted)))),
                )
                .highlight_style(row_style(sel_bg, true)),
            l,
            &mut self.mods_list_state,
        );
        push_hits(&mut self.list_hits, inner(l), self.mods_list_state.offset(), self.installed_mods.len(), 2);
        self.draw_row_icons(frame, l, self.mods_list_state.offset(), self.installed_mods.len(), selected, |i| {
            let m = &self.installed_mods[i];
            match self.mod_info.get(mod_key(&m.filename)).and_then(|o| o.as_ref()) {
                Some(x) => (Some(x.project.project_id.as_str()), x.project.title.as_str()),
                None => (None, m.display_name.as_str()),
            }
        });
        if !wide {
            return;
        }

        let sel = self.mods_list_state.selected().unwrap_or(0);
        let Some(m) = self.installed_mods.get(sel).cloned() else {
            frame.render_widget(
                Paragraph::new(empty_panel(&self.theme, "Nothing selected", "press A to install a mod"))
                    .block(block(&self.theme, " Details ", border)),
                r,
            );
            return;
        };
        let info = self.mod_info.get(mod_key(&m.filename)).cloned().flatten();
        frame.render_widget(block(&self.theme, " Details ", border), r);
        let body = inner(r);
        let [top, desc, hints] =
            Layout::vertical([Constraint::Length(9), Constraint::Fill(1), Constraint::Length(2)]).areas(body);
        let title = info.as_ref().map(|x| x.project.title.clone()).unwrap_or_else(|| m.display_name.clone());
        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(title.clone(), Style::default().fg(self.theme.primary).bold())),
        ];
        match &info {
            Some(x) => {
                lines.push(Line::from(Span::styled(format!("version {}", x.version_number), self.theme.secondary)));
                lines.push(Line::from(""));
                lines.push(stat_line(&self.theme, "⬇", x.project.downloads, "downloads", self.theme.warn));
                lines.push(stat_line(&self.theme, "♥", x.project.follows, "followers", self.theme.error));
            }
            None => {
                let state = if self.identifying {
                    "looking up on Modrinth…"
                } else if !self.online {
                    "offline — details unavailable"
                } else {
                    "not found on Modrinth"
                };
                lines.push(Line::from(Span::styled(state, self.theme.muted)));
                lines.push(Line::from(""));
            }
        }
        if let Some(v) = self.update_for(&name, &m.filename) {
            lines.push(Line::from(Span::styled(format!("↑ update available → {}", v.version_number), Style::default().fg(self.theme.cyan).bold())));
        }
        lines.push(Line::from(vec![
            Span::styled(if m.enabled { "● enabled" } else { "○ disabled" }, Style::default().fg(if m.enabled { self.theme.accent } else { self.theme.muted })),
            Span::styled(format!("   {}", fmt_size(m.size_bytes)), self.theme.secondary),
        ]));
        let id = info.as_ref().map(|x| x.project.project_id.clone());
        self.render_icon_header(frame, top, id.as_deref(), &title, lines);

        let mut d = vec![Line::from(Span::styled(m.filename.clone(), self.theme.muted)), Line::from("")];
        if let Some(x) = &info {
            d.push(Line::from(Span::styled(x.project.description.clone(), self.theme.fg)));
        }
        frame.render_widget(Paragraph::new(d).wrap(Wrap { trim: true }), pad_rect(desc, 1));
        let mut k = key_hint(&self.theme, "Space", "toggle", self.theme.accent);
        k.extend(key_hint(&self.theme, "U", "update", self.theme.cyan));
        k.extend(key_hint(&self.theme, "D", "remove", self.theme.error));
        k.extend(key_hint(&self.theme, "A", "add mods", self.theme.cyan));
        frame.render_widget(Paragraph::new(Line::from(k)), pad_rect(hints, 1));
    }

    /// Big icon on the left, text lines to its right.
    fn render_icon_header(&self, frame: &mut Frame, area: Rect, id: Option<&str>, title: &str, lines: Vec<Line<'static>>) {
        let [ic, info] = Layout::horizontal([Constraint::Length(ICON_LARGE as u16 + 4), Constraint::Fill(1)]).areas(area);
        let rect = Rect { x: ic.x + 2, y: ic.y + 1, width: ICON_LARGE as u16, height: ICON_LARGE as u16 / 2 };
        self.draw_icon(frame, rect, id, title, true, self.theme.surface);
        frame.render_widget(Paragraph::new(lines), info);
    }

    /// One renderer for both search tabs: search bar, results with icons and
    /// download counts, and a Modrinth-style detail panel with versions.
    fn render_search(&mut self, frame: &mut Frame, area: Rect, is_mod: bool) {
        let [bar, body] = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(area);
        let (q, typing) = if is_mod {
            (self.mod_search_query.clone(), self.input_mode == InputMode::TypingSearch)
        } else {
            (self.modpack_search_query.clone(), self.input_mode == InputMode::TypingModpackSearch)
        };
        let searching = if is_mod { self.searching_mods } else { self.searching_modpacks };
        let sort = Self::effective_sort(q.trim(), if is_mod { self.mod_sort } else { self.pack_sort });
        let hint = if q.is_empty() && !typing {
            if is_mod { "press / to search mods — showing popular" } else { "press / to search modpacks — showing popular" }
        } else {
            &q
        };
        let border = self.panel_border();
        let tail = if searching {
            format!("  {} searching…", self.spin())
        } else if typing {
            self.cursor()
        } else {
            String::new()
        };
        let scope = match (is_mod, self.current_instance()) {
            (true, Some(i)) if self.mod_compat => format!(" for {} · {} {} ", i.name, i.loader, i.mc_version),
            (true, Some(_)) => " any loader or version ".to_string(),
            _ => String::new(),
        };
        let cat = modpack::MOD_CATEGORIES.get(self.mod_cat).copied().filter(|_| is_mod).or_else(|| modpack::PACK_CATEGORIES.get(self.pack_cat).copied().filter(|_| !is_mod)).unwrap_or("any");
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ⌕ ", Style::default().fg(self.theme.cyan).bold()),
                Span::styled(
                    format!("{hint}{tail}"),
                    if typing || searching { self.theme.primary } else { self.theme.muted },
                ),
            ]))
            .block(
                block(&self.theme, if is_mod { " Modrinth · mods " } else { " Modrinth · modpacks " }, if typing { self.theme.accent } else { border })
                    .title(Line::from(vec![
                        Span::styled(scope, self.theme.secondary),
                        Span::styled(if cat == "any" { String::new() } else { format!(" {cat} ") }, self.theme.cyan),
                        Span::styled(format!(" sort: {sort} "), self.theme.muted),
                    ]).right_aligned()),
            ),
            bar,
        );

        let wide = body.width >= 90;
        let [list, detail] = if wide {
            Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(body)
        } else {
            [body, Rect::default()]
        };
        let hits = if is_mod { &self.mod_search_results } else { &self.modpack_search_results };
        let sel = if is_mod { self.mod_search_state.selected() } else { self.modpack_search_state.selected() };
        let sel_bg = self.row_bg();
        let width = inner(list).width as usize;
        let tw = width.saturating_sub(2 + 4 + 1 + 9).max(6);
        let items: Vec<ListItem> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let is_sel = sel == Some(i);
                let mut l1 = vec![self.marker(is_sel), Span::raw("     ")];
                l1.push(Span::styled(
                    pad(&trunc(&h.title, tw), tw),
                    if is_sel { Style::default().fg(self.theme.primary).bold() } else { Style::default().fg(self.theme.fg) },
                ));
                l1.push(Span::styled(format!("{:>9}", format!("⬇ {}", fmt_dl(h.downloads.unwrap_or(0)))), Style::default().fg(self.theme.warn)));
                let sub = match &h.author {
                    Some(a) => format!("by {a} · {}", h.description),
                    None => h.description.clone(),
                };
                let mut l2 = vec![Span::raw("       ")];
                l2.push(Span::styled(trunc(&sub, tw + 9), Style::default().fg(self.theme.muted)));
                ListItem::new(vec![Line::from(l1), Line::from(l2)]).style(row_style(sel_bg, is_sel))
            })
            .collect();
        let n = hits.len();
        let empty = n == 0;
        let list_block = block(&self.theme, format!(" Results · {n} "), border);
        if empty {
            if searching {
                frame.render_widget(Paragraph::new(self.skeleton_rows(inner(list))).block(list_block), list);
            } else {
                frame.render_widget(Paragraph::new(empty_panel(&self.theme, "no results — try another search", "")).block(list_block), list);
            }
        } else {
            let state = if is_mod { &mut self.mod_search_state } else { &mut self.modpack_search_state };
            frame.render_stateful_widget(List::new(items).block(list_block).highlight_style(row_style(sel_bg, true)), list, state);
            let off = state.offset();
            push_hits(&mut self.list_hits, inner(list), off, n, 2);
            let hits = if is_mod { &self.mod_search_results } else { &self.modpack_search_results };
            self.draw_row_icons(frame, list, off, n, sel, |i| (Some(hits[i].project_id.as_str()), hits[i].title.as_str()));
        }
        if wide {
            self.render_project(frame, detail, is_mod);
        }
    }

    /// Modrinth-style project page: icon, stats, description, versions.
    fn render_project(&self, frame: &mut Frame, area: Rect, is_mod: bool) {
        let border = self.panel_border();
        let Some((h, _)) = self.selected_hit() else {
            frame.render_widget(
                Paragraph::new(empty_panel(&self.theme, "Nothing selected", "search, then pick a result"))
                    .block(block(&self.theme, " Project ", border)),
                area,
            );
            return;
        };
        frame.render_widget(block(&self.theme, " Project ", border), area);
        let body = inner(area);
        let [top, desc, vers, hints] = Layout::vertical([
            Constraint::Length(9),
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(body);

        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(h.title.clone(), Style::default().fg(self.theme.primary).bold())),
        ];
        if let Some(a) = &h.author {
            lines.push(Line::from(Span::styled(format!("by {a}"), self.theme.secondary)));
        }
        lines.push(Line::from(""));
        lines.push(stat_line(&self.theme, "⬇", h.downloads, "downloads", self.theme.warn));
        lines.push(stat_line(&self.theme, "♥", h.follows, "followers", self.theme.error));
        if let Some(ago) = h.date_modified.as_deref().and_then(fmt_ago) {
            lines.push(Line::from(Span::styled(format!("⟳ updated {ago}"), self.theme.muted)));
        }
        let tags: Vec<&str> = h
            .categories
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(|c| !matches!(*c, "fabric" | "forge" | "neoforge" | "quilt" | "liteloader" | "modloader" | "rift"))
            .take(4)
            .collect();
        if !tags.is_empty() {
            let mut t = Vec::new();
            for tag in tags {
                t.push(Span::styled(format!(" {tag} "), Style::default().fg(self.theme.cyan).bg(self.theme.bg_sel)));
                t.push(Span::raw(" "));
            }
            lines.push(Line::from(t));
        }
        self.render_icon_header(frame, top, Some(&h.project_id), &h.title, lines);
        frame.render_widget(
            Paragraph::new(Span::styled(h.description.clone(), self.theme.fg)).wrap(Wrap { trim: true }),
            pad_rect(desc, 1),
        );

        // Versions preview: what Enter would install, and what else exists.
        let vw = vers.width.saturating_sub(2) as usize;
        let mut v = Vec::new();
        match self.project_versions.get(&h.project_id) {
            None | Some(None) => {
                v.push(sec(&self.theme, "Versions"));
                v.push(Line::from(Span::styled(format!("  {} loading versions…", self.spin()), self.theme.muted)));
            }
            Some(Some(Err(e))) => {
                v.push(sec(&self.theme, "Versions"));
                v.push(Line::from(Span::styled(format!("  {}", trunc(e, vw)), self.theme.error)));
            }
            Some(Some(Ok(all))) => {
                let (choices, filtered) = self.version_choices(&h.project_id, is_mod, false);
                let best = self.best_version(&h.project_id, is_mod).map(|b| b.id);
                let head = match (filtered, self.current_instance()) {
                    (true, Some(i)) => format!("Versions · {} of {} fit {} {}", choices.len(), all.len(), i.loader, i.mc_version),
                    _ if is_mod => format!("Versions · none fit this instance — {} total", all.len()),
                    _ => format!("Versions · {}", all.len()),
                };
                v.push(sec(&self.theme, &head));
                let room = vers.height.saturating_sub(1) as usize;
                for ver in choices.iter().take(room) {
                    v.push(self.version_row(ver, vw, best.as_deref() == Some(ver.id.as_str())));
                }
            }
        }
        frame.render_widget(Paragraph::new(v), vers);
        let mut k = key_hint(&self.theme, "Enter", "install ★", self.theme.accent);
        k.extend(key_hint(&self.theme, "V", "pick version", self.theme.cyan));
        k.extend(key_hint(&self.theme, "O", "sort", self.theme.secondary));
        frame.render_widget(Paragraph::new(Line::from(k)), pad_rect(hints, 1));
    }

    /// `★ 0.6.5+mc1.21.1   release  1.21.1 +2   fabric   ⬇ 1.2M   2025-06-01`
    fn version_row(&self, v: &ModrinthVersion, width: usize, best: bool) -> Line<'static> {
        let (badge, tone) = match v.version_type.as_str() {
            "beta" => ("beta", self.theme.warn),
            "alpha" => ("alpha", self.theme.error),
            _ => ("release", self.theme.accent),
        };
        let games = match v.game_versions.len() {
            0 => String::new(),
            1 => v.game_versions[0].clone(),
            n => format!("{} +{}", v.game_versions[n - 1], n - 1),
        };
        let loaders = v.loaders.join(",");
        let wide = width >= 72;
        // Fixed columns: marker 3, badge 9, games 13, then loaders 11,
        // downloads 9 and date 11 when there is room.
        let num_w = width.saturating_sub(if wide { 3 + 9 + 13 + 11 + 9 + 11 } else { 3 + 9 + 13 }).max(8);
        let mut spans = vec![
            Span::styled(if best { " ★ " } else { "   " }, Style::default().fg(self.theme.accent).bold()),
            Span::styled(
                pad(&trunc(&v.version_number, num_w), num_w),
                if best { Style::default().fg(self.theme.primary).bold() } else { Style::default().fg(self.theme.fg) },
            ),
            Span::styled(format!(" {badge:<8}"), Style::default().fg(tone)),
            Span::styled(format!("{:<13}", trunc(&games, 12)), Style::default().fg(self.theme.cyan)),
        ];
        if wide {
            spans.push(Span::styled(format!("{:<11}", trunc(&loaders, 10)), Style::default().fg(self.loader_color(v.loaders.first().map(String::as_str).unwrap_or("")))));
            spans.push(Span::styled(format!("{:>8} ", format!("⬇ {}", fmt_dl(v.downloads))), Style::default().fg(self.theme.warn)));
            spans.push(Span::styled(format!(" {}", v.date_published.get(..10).unwrap_or("")), Style::default().fg(self.theme.muted)));
        }
        Line::from(spans)
    }

    fn cursor(&self) -> String {
        if !self.cfg.anim || (self.frame / 24).is_multiple_of(2) { " ▌".into() } else { "  ".into() }
    }

    fn render_wizard(&mut self, frame: &mut Frame, area: Rect) {
        let [f, v] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
        let fld = self.new_inst_field;
        let typing = self.input_mode == InputMode::TypingNewInstance;
        let blink = if typing { self.cursor() } else { String::new() };
        let border = self.panel_border();
        let sel_bg = self.row_bg();
        let show = |i: usize, val: &str, ph: &str, blink: &str| {
            let s = if val.is_empty() { ph } else { val };
            let c = if fld == i { blink } else { "" };
            format!(" {s}{c} ")
        };
        // Each field is a chip: the row you are editing is filled, the rest
        // stay quiet, so it is always obvious where typing lands.
        let chip = |i: usize, text: String, tone: Color| -> Line<'static> {
            let focused = fld == i;
            Line::from(vec![
                Span::styled(
                    format!(" {} ", i + 1),
                    if focused {
                        Style::default().fg(self.theme.bg_sel).bg(tone).bold()
                    } else {
                        Style::default().fg(self.theme.muted)
                    },
                ),
                Span::styled(
                    text,
                    if focused {
                        Style::default().fg(self.theme.primary).bg(self.theme.bg_sel).bold()
                    } else {
                        Style::default().fg(self.theme.fg)
                    },
                ),
            ])
        };
        let loader = LOADERS[self.new_inst_loader_idx].to_uppercase();
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled("  Name", Style::default().fg(if fld == 0 { self.theme.accent } else { self.theme.muted }))),
            chip(0, show(0, &self.new_inst_name, "e.g. Survival", &blink), self.theme.accent),
            Line::from(""),
            Line::from(Span::styled("  Version", Style::default().fg(if fld == 1 { self.theme.accent } else { self.theme.muted }))),
            chip(1, show(1, &self.new_inst_version_input, "1.21.1", &blink), self.theme.accent),
            Line::from(""),
            Line::from(Span::styled("  Loader", Style::default().fg(if fld == 2 { self.theme.accent } else { self.theme.muted }))),
            chip(2, format!("◀  {loader}  ▶"), self.theme.cyan),
            Line::from(""),
            Line::from(Span::styled("  Max RAM", Style::default().fg(if fld == 3 { self.theme.accent } else { self.theme.muted }))),
            chip(3, show(3, &self.new_inst_ram, "4G", &blink), self.theme.accent),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "   Create instance   ",
                    if fld == 4 {
                        Style::default().fg(self.theme.bg_sel).bg(self.theme.accent).bold()
                    } else {
                        Style::default().fg(self.theme.secondary).bg(self.theme.bg_sel)
                    },
                ),
                Span::styled(
                    if typing { "   Tab next · Esc stops typing" } else { "   Enter confirms" },
                    Style::default().fg(self.theme.muted),
                ),
            ]),
            Line::from(""),
            rule(&self.theme, f.width.saturating_sub(4)),
        ];
        frame.render_widget(
            Paragraph::new(lines).block(block(&self.theme, " New instance ", border)),
            f,
        );
        let items: Vec<ListItem> = self
            .matching_versions
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let sel = i == self.matching_version_idx;
                ListItem::new(Line::from(vec![
                    self.marker(sel),
                    Span::styled(
                        x.clone(),
                        if sel {
                            Style::default().fg(self.theme.primary).bold()
                        } else {
                            Style::default().fg(self.theme.fg)
                        },
                    ),
                ]))
                .style(row_style(sel_bg, sel))
            })
            .collect();
        // Stateful so the list scrolls to keep the picked version visible.
        let mut state = ListState::default().with_selected(Some(self.matching_version_idx));
        frame.render_stateful_widget(
            List::new(items)
                .block(block(&self.theme, " Versions · ←→ picks ", border))
                .highlight_style(row_style(sel_bg, true)),
            v,
            &mut state,
        );
        push_hits(&mut self.list_hits, inner(v), state.offset(), self.matching_versions.len(), 1);
    }

    fn render_settings(&self, frame: &mut Frame, area: Rect) {
        let cfg = &self.config;
        let border = self.panel_border();
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
        let mut account = vec![Line::from("")];
        if self.active_account.is_none() {
            account.push(sec(&self.theme, "Account"));
            account.push(Line::from("    not logged in"));
            account.push(Line::from(""));
        }
        let login = if self.active_account.is_some() { "switch account" } else { "microsoft login" };
        account.push(Line::from(key_hint(&self.theme, "M", login, self.theme.accent)));
        account.push(Line::from(""));
        account.push(sec(&self.theme, "Launch"));
        account.push(Line::from(key_hint(
            &self.theme,
            "T",
            if self.launch_new_term { "launch in · new window" } else { "launch in · Mirage" },
            self.theme.cyan,
        )));
        account.push(Line::from(vec![
            Span::styled("    ", self.theme.fg),
            Span::styled(
                if self.launch_new_term { "games open in their own terminal window" } else { "games run inside Mirage, log on the instance page" },
                Style::default().fg(self.theme.muted),
            ),
        ]));
        account.push(Line::from(""));
        account.push(sec(&self.theme, "Appearance"));
        account.push(Line::from(key_hint(&self.theme, "Y", "cycle theme", self.theme.cyan)));
        let mut themes = vec![Span::raw("    ")];
        for name in Theme::all() {
            themes.push(if name == self.theme.name {
                Span::styled(format!(" {name} "), Style::default().fg(self.theme.bg_sel).bg(self.theme.accent).bold())
            } else {
                Span::styled(format!(" {name} "), Style::default().fg(self.theme.muted))
            });
        }
        account.push(Line::from(themes));

        let mut runtime = vec![Line::from("")];
        runtime.push(sec(&self.theme, "Memory"));
        runtime.push(kv(&self.theme, "RAM", &format!("{} – {}", cfg.ram_min, cfg.ram_max), self.theme.warn, r.width));
        runtime.push(Line::from(""));
        runtime.push(sec(&self.theme, "Files"));
        runtime.push(kv(
            &self.theme,
            "Config",
            &short_path(&crate::config::config_path()),
            self.theme.muted,
            r.width,
        ));
        runtime.push(kv(
            &self.theme,
            "Data",
            &short_path(&crate::instance::base_instances_dir()),
            self.theme.muted,
            r.width,
        ));

        frame.render_widget(block(&self.theme, " Preferences ", border), l);
        let mut body = inner(l);
        if let Some(a) = &self.active_account {
            let [top, rest] = Layout::vertical([Constraint::Length(10), Constraint::Fill(1)]).areas(body);
            let lines = vec![
                Line::from(""),
                Line::from(""),
                Line::from(Span::styled(a.username.clone(), Style::default().fg(self.theme.primary).bold())),
                Line::from(Span::styled("Signed in", self.theme.secondary)),
            ];
            self.render_icon_header(frame, top, Some(&format!("head:{}", a.uuid)), &a.username, lines);
            body = rest;
        }
        frame.render_widget(Paragraph::new(account).wrap(Wrap { trim: false }), body);
        frame.render_widget(
            Paragraph::new(runtime)
                .block(block(&self.theme, " System ", border))
                .wrap(Wrap { trim: false }),
            r,
        );
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let c = if self.status_is_error { self.theme.error } else { self.theme.accent };
        let [status, keys] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        // Fresh statuses flash bright for a moment, then settle to the normal
        // text colour — enough to catch the eye without a modal.
        let flash = 1.0 - ease_out(self.status_flash);
        let text = lerp_rgb(self.theme.fg, self.theme.primary, flash);
        let dot = c;
        let icon = if self.status_is_error { "  ✖ " } else { "  ● " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(icon, Style::default().fg(dot)),
                Span::styled(&self.status_msg, Style::default().fg(text)),
            ])),
            status,
        );
        let [hint, help] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(14)]).areas(keys);
        let mut spans = vec![Span::raw("  ")];
        for part in self.current_tab.keys().split(" • ") {
            let (k, label) = part.split_once(' ').unwrap_or((part, ""));
            spans.push(Span::styled(k.to_string(), Style::default().fg(self.theme.accent).bold()));
            spans.push(Span::styled(format!(" {label}   "), Style::default().fg(self.theme.muted)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), hint);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ? ", Style::default().fg(self.theme.secondary).bg(self.theme.bg_sel).bold()),
                Span::styled(" all keys ", Style::default().fg(self.theme.muted)),
            ]))
            .alignment(Alignment::Right),
            help,
        );
    }

    fn render_modals(&self, frame: &mut Frame, area: Rect) {
        if self.help_open {
            let mut lines = vec![Line::from("")];
            let mut group = |title: &str, rows: Vec<(&str, &str)>| {
                lines.push(Line::from(Span::styled(
                    format!("  {title}"),
                    Style::default().fg(self.theme.accent).bold(),
                )));
                for (k, d) in rows {
                    lines.push(Line::from(vec![
                        Span::styled(format!("    {k:<14}"), Style::default().fg(self.theme.primary)),
                        Span::styled(d.to_string(), Style::default().fg(self.theme.secondary)),
                    ]));
                }
                lines.push(Line::from(""));
            };
            group("Move", vec![
                ("↑ ↓ / j k", "select a row"),
                ("g / G", "jump to first / last"),
                ("Ctrl-d Ctrl-u", "jump half a page"),
                ("Enter / Space", "primary action for the tab"),
                ("Esc", "back to Instances"),
            ]);
            group("Tabs", vec![
                ("1 … 6", "jump straight to a tab"),
                ("Tab / ← →", "next / previous tab"),
                ("[ / ]", "cycle tabs"),
                ("?", "toggle this help"),
                ("q", "quit"),
            ]);
            group("Instances", vec![
                ("Enter", "play the selected instance"),
                ("E", "RAM, Java path and JVM flags"),
                ("M / S", "installed mods / find mods"),
                ("C / D / N", "copy / delete / new instance"),
                ("P", "export a .mrpack to Downloads"),
            ]);
            group("Mods & modpacks", vec![
                ("Space", "toggle a mod on or off"),
                ("U / Shift-U", "update the mod / every mod"),
                ("A or /", "search Modrinth"),
                ("Enter", "install the best matching version (★)"),
                ("V", "choose an exact version"),
                ("O", "cycle sort: relevance, downloads, …"),
                ("F", "cycle category filter"),
                ("C", "mods: this instance only / any version"),
            ]);
            popup(frame, area, &self.theme, "Keys", self.theme.accent, Alignment::Left, lines);
        }
        if let Some(p) = &self.picker {
            self.render_picker(frame, area, p);
        }
        if let Some(stage) = &self.login {
            self.render_login(frame, area, stage);
        }
        if let Some(t) = &self.delete_confirm_target {
            popup(frame, area, &self.theme, "Delete instance", self.theme.error, Alignment::Center, vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("Remove ", self.theme.fg),
                    Span::styled(t.clone(), Style::default().fg(self.theme.error).bold()),
                    Span::styled(" and all of its mods?", self.theme.fg),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled(" y ", Style::default().fg(self.theme.bg_sel).bg(self.theme.error).bold()),
                    Span::styled(" delete      ", self.theme.secondary),
                    Span::styled(" n ", Style::default().fg(self.theme.bg_sel).bg(self.theme.muted).bold()),
                    Span::styled(" cancel", self.theme.secondary),
                ]),
            ]);
        }
        if let Some(ed) = &self.edit {
            let th = &self.theme;
            let mut lines = vec![Line::from("")];
            for (i, (label, hint)) in EDIT_FIELDS.iter().enumerate() {
                let active = ed.field == i;
                let (mark, tone) = if active { ("▍ ", th.accent) } else { ("  ", th.secondary) };
                lines.push(Line::from(vec![
                    Span::styled(mark, Style::default().fg(th.accent).bold()),
                    Span::styled(label.to_string(), Style::default().fg(tone).bold()),
                ]));
                let val = &ed.vals[i];
                lines.push(Line::from(if val.is_empty() && !active {
                    vec![Span::raw("    "), Span::styled(hint.to_string(), th.muted)]
                } else {
                    vec![
                        Span::raw("    "),
                        Span::styled(val.clone(), Style::default().fg(th.primary).bold()),
                        Span::styled(if active { self.cursor() } else { String::new() }, th.primary),
                    ]
                }));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled("Tab next field · Enter save · Esc cancel", th.muted)));
            popup(frame, area, th, &format!("Settings · {}", ed.name), th.cyan, Alignment::Left, lines);
        }
        if self.input_mode == InputMode::TypingCloneName {
            popup(frame, area, &self.theme, "Duplicate instance", self.theme.cyan, Alignment::Center, vec![
                Line::from(""),
                Line::from(Span::styled("Name for the copy", self.theme.secondary)),
                Line::from(vec![
                    Span::styled("  ⌕ ", Style::default().fg(self.theme.cyan)),
                    Span::styled(format!("{}{}", self.clone_input, self.cursor()), self.theme.primary).bold(),
                ]),
                Line::from(""),
                Line::from(Span::styled("Enter to duplicate · Esc to cancel", self.theme.muted)),
            ]);
        }
    }
}

impl App {
    /// The sign-in screen: a welcome with the setup checklist, then the
    /// step-by-step device-code flow.
    fn render_login(&self, frame: &mut Frame, area: Rect, stage: &LoginStage) {
        let th = &self.theme;
        let dim = Style::default().fg(th.muted);
        let hint = |keys: &[(&str, &str)]| {
            let mut v = vec![Span::raw("  ")];
            for (k, d) in keys {
                v.push(Span::styled(format!(" {k} "), Style::default().fg(th.bg_sel).bg(th.accent).bold()));
                v.push(Span::styled(format!(" {d}   "), dim));
            }
            Line::from(v)
        };
        // One row of the flow: ✔ done, spinner active, ○ waiting.
        let row = |state: u8, text: &str| {
            let (mark, tone) = match state {
                2 => ("✔".to_string(), th.accent),
                1 => (self.spin().to_string(), th.primary),
                _ => ("○".to_string(), th.muted),
            };
            Line::from(vec![
                Span::styled(format!("  {mark} "), Style::default().fg(tone).bold()),
                Span::styled(text.to_string(), Style::default().fg(if state == 0 { th.muted } else { th.fg })),
            ])
        };
        let flow = |code: u8, verify: u8| {
            vec![
                row(2, "Get a sign-in code"),
                row(code, "Approve it in your browser"),
                row(verify, "Check your Xbox and Minecraft profile"),
            ]
        };
        let mut lines = vec![Line::from("")];
        let (title, color) = match stage {
            LoginStage::Welcome => {
                lines.extend(LOGO.iter().map(|r| Line::from(gradient_spans(th, r, 0.0, true)).centered()));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Welcome to Mirage", Style::default().fg(th.primary).bold())).centered());
                lines.push(Line::from(Span::styled("A fast terminal launcher for Minecraft", th.secondary)).centered());
                lines.push(Line::from(""));
                lines.extend(self.setup_steps());
                lines.push(Line::from(""));
                lines.push(hint(&[("Enter", "sign in"), ("Esc", "later")]));
                ("Welcome", th.accent)
            }
            LoginStage::Requesting => {
                lines.push(row(1, "Getting a sign-in code…"));
                lines.push(row(0, "Approve it in your browser"));
                lines.push(row(0, "Check your Xbox and Minecraft profile"));
                lines.push(Line::from(""));
                lines.push(hint(&[("Esc", "cancel")]));
                ("Sign in", th.accent)
            }
            LoginStage::Code { code, uri, copied, since } => {
                lines.extend(flow(1, 0));
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::raw("      Open  "), Span::styled(uri.clone(), Style::default().fg(th.cyan).underlined())]));
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::raw("      Enter "),
                    Span::styled(format!(" {code} "), Style::default().fg(th.bg_sel).bg(th.warn).bold()),
                    Span::styled(if *copied { "   ✔ copied" } else { "" }, Style::default().fg(th.accent)),
                ]));
                lines.push(Line::from(""));
                let left = auth::LOGIN_TIMEOUT_SECS.saturating_sub(since.elapsed().as_secs());
                lines.push(Line::from(Span::styled(format!("  Waiting for you… {}:{:02} left", left / 60, left % 60), dim)));
                lines.push(Line::from(""));
                lines.push(hint(&[("C", "copy code"), ("O", "open browser"), ("Esc", "cancel")]));
                ("Sign in with Microsoft", th.accent)
            }
            LoginStage::Verifying => {
                lines.extend(flow(2, 1));
                lines.push(Line::from(""));
                lines.push(hint(&[("Esc", "cancel")]));
                ("Almost there", th.accent)
            }
            LoginStage::Failed(e) => {
                lines.push(Line::from(Span::styled("  ✖ Sign-in failed", Style::default().fg(th.error).bold())));
                lines.push(Line::from(Span::styled(format!("    {}", trunc(e, 60)), th.secondary)));
                lines.push(Line::from(""));
                lines.push(hint(&[("Enter", "try again"), ("Esc", "close")]));
                ("Sign in", th.error)
            }
        };
        lines.push(Line::from(""));
        popup(frame, area, th, title, color, Alignment::Left, lines);
    }

    fn render_picker(&self, frame: &mut Frame, area: Rect, p: &Picker) {
        let w = area.width.saturating_sub(4).min(104);
        let h = area.height.saturating_sub(2).min(26);
        let pop = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
        frame.render_widget(Clear, pop);
        let keys = if p.is_mod {
            " ↑↓ move · Enter install · A all versions · Esc close "
        } else {
            " ↑↓ move · Enter install · Esc close "
        };
        let outer = block(&self.theme, format!(" Versions · {} ", trunc(&p.title, 40)), self.theme.accent)
            .title_bottom(Line::from(Span::styled(keys, self.theme.muted)));
        frame.render_widget(outer, pop);
        let body = pad_rect(inner(pop), 1);
        let [head, list] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(body);
        let (choices, filtered) = self.version_choices(&p.project_id, p.is_mod, p.show_all);
        let note = match self.project_versions.get(&p.project_id) {
            None | Some(None) => format!("{} loading versions…", self.spin()),
            Some(Some(Err(e))) => format!("could not load versions: {e}"),
            Some(Some(Ok(all))) => match (filtered, self.current_instance()) {
                (true, Some(i)) => format!("{} of {} versions fit {} {} — A shows all", choices.len(), all.len(), i.loader, i.mc_version),
                _ => format!("all {} versions", all.len()),
            },
        };
        frame.render_widget(Paragraph::new(Span::styled(note, self.theme.secondary)), head);
        let best = self.best_version(&p.project_id, p.is_mod).map(|b| b.id);
        let sel = p.state.selected();
        let sel_bg = self.theme.bg_sel;
        let items: Vec<ListItem> = choices
            .iter()
            .enumerate()
            .map(|(i, v)| {
                ListItem::new(self.version_row(v, list.width as usize, best.as_deref() == Some(v.id.as_str())))
                    .style(row_style(sel_bg, sel == Some(i)))
            })
            .collect();
        let mut state = p.state;
        frame.render_stateful_widget(List::new(items).highlight_style(row_style(sel_bg, true)), list, &mut state);
    }
}

// ── shared line editor: one place for all text inputs ──────────────────────
enum Edit {
    Done,
    Cancel,
    More,
}
fn edit_line(key: KeyEvent, buf: &mut String) -> Edit {
    match key.code {
        KeyCode::Enter => Edit::Done,
        KeyCode::Esc => Edit::Cancel,
        KeyCode::Backspace => { buf.pop(); Edit::More }
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => { buf.clear(); Edit::More }
        KeyCode::Char('w') if key.modifiers == KeyModifiers::CONTROL => {
            while buf.ends_with(' ') { buf.pop(); }
            while !buf.is_empty() && !buf.ends_with(' ') { buf.pop(); }
            Edit::More
        }
        KeyCode::Char(c) => { buf.push(c); Edit::More }
        _ => Edit::More,
    }
}

// ── tiny render helpers ────────────────────────────────────────────────────
/// A card: surface background, hairline rounded border, quiet bold title.
fn block(th: &Theme, title: impl Into<String>, border: Color) -> Block<'static> {
    Block::default()
        .title(title.into())
        .title_style(Style::default().fg(th.secondary).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(th.surface).fg(th.fg))
}
fn row_style(bg: Color, sel: bool) -> Style {
    if sel { Style::default().bg(bg) } else { Style::default() }
}
/// Content rect of a rounded block drawn at `area`.
fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}
/// Record which screen row shows which list index, so a click can select it.
fn push_hits(hits: &mut Vec<(u16, usize)>, area: Rect, offset: usize, count: usize, row_h: u16) {
    let rows = (area.height / row_h.max(1)) as usize;
    for r in 0..rows {
        let idx = offset + r;
        if idx >= count {
            break;
        }
        hits.push((area.y + r as u16 * row_h, idx));
    }
}
/// A keycap chip and its description, e.g. `[M] mods`.
fn key_hint(th: &Theme, key: &str, label: &str, tone: Color) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!(" {key} "), Style::default().fg(tone).bg(th.bg_sel).bold()),
        Span::styled(format!(" {label}   "), Style::default().fg(th.secondary)),
    ]
}
/// Aligned `label   value` row for the inspector panels. `width` is the panel
/// width, so long paths get clipped instead of wrapping onto a second line.
fn kv(th: &Theme, k: &str, v: &str, c: Color, width: u16) -> Line<'static> {
    let room = width.saturating_sub(16) as usize;
    Line::from(vec![
        Span::styled(format!("    {k:<10}"), Style::default().fg(th.muted)),
        Span::styled(trunc(v, room.max(8)), Style::default().fg(c)),
    ])
}
/// Section heading inside a panel.
fn sec(th: &Theme, t: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {t}"), Style::default().fg(th.accent)).bold())
}
fn rule(th: &Theme, w: u16) -> Line<'static> {
    Line::from(Span::styled("─".repeat(w as usize), Style::default().fg(th.border)))
}
fn fmt_size(b: u64) -> String {
    let kb = b / 1024;
    if kb > 1024 { format!("{:.1} MB", kb as f64 / 1024.0) } else { format!("{kb} KB") }
}
/// Human playtime; "-" when the instance has never been launched.
fn fmt_playtime(secs: u64) -> String {
    if secs == 0 {
        return "never".into();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 { format!("{h}h {m:02}m") } else { format!("{m}m") }
}
/// Paths shortened against `$HOME` so the inspector rows stay on one line.
fn short_path(p: &Path) -> String {
    let full = p.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && full.starts_with(&home) => full.replacen(&home, "~", 1),
        _ => full,
    }
}
/// Centred empty state for a panel that has nothing to show yet.
fn empty_panel(th: &Theme, title: &str, hint: &str) -> Vec<Line<'static>> {
    let pad = 2;
    vec![
        Line::from(""),
        Line::from(""),
        Line::from(vec![Span::styled(" ".repeat(pad), th.fg), Span::styled(title.to_string(), th.secondary).bold()]),
        Line::from(vec![Span::styled(" ".repeat(pad), th.fg), Span::styled(hint.to_string(), th.muted)]),
    ]
}
/// `⬇ 6.2M downloads` style stat row; hidden when the value is unknown.
fn stat_line(th: &Theme, icon: &str, v: Option<u64>, label: &str, tone: Color) -> Line<'static> {
    match v {
        Some(v) => Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(tone)),
            Span::styled(fmt_dl(v), Style::default().fg(th.primary).bold()),
            Span::styled(format!(" {label}"), Style::default().fg(th.secondary)),
        ]),
        None => Line::from(""),
    }
}
/// Right-pad to `n` characters (char-based, like `trunc`).
fn pad(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len >= n { s.to_string() } else { format!("{s}{}", " ".repeat(n - len)) }
}
/// Shrink a rect horizontally by `x` cells on each side.
fn pad_rect(r: Rect, x: u16) -> Rect {
    Rect { x: r.x + x.min(r.width / 2), width: r.width.saturating_sub(2 * x), ..r }
}
/// `1m 05s`, `42s`, `1h 03m`.
fn fmt_secs(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}
/// RFC 3339 timestamp → "3 days ago".
fn fmt_ago(ts: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let d = chrono::Utc::now().signed_duration_since(t);
    let (n, unit) = match d.num_seconds() {
        s if s < 3600 => return Some("just now".into()),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 86_400 * 30 => (s / 86_400, "day"),
        s if s < 86_400 * 365 => (s / (86_400 * 30), "month"),
        s => (s / (86_400 * 365), "year"),
    };
    Some(format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" }))
}
fn fmt_dl(n: u64) -> String {
    if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1_000_000.0) }
    else if n >= 1_000 { format!("{:.0}K", n as f64 / 1_000.0) }
    else { format!("{n}") }
}
/// "ended normally" / "crashed: <description>" for an instance's last
/// session, reading the newest crash report only when the exit was bad.
/// 0, or a signal the user sent (130 = Ctrl-C/SIGINT, 143 = SIGTERM, e.g.
/// closing the game's terminal window): not a crash.
fn clean_exit(code: i32) -> bool {
    matches!(code, 0 | 130 | 143)
}
fn session_summary(inst: &Instance, log: Option<&VecDeque<String>>) -> Option<(bool, String)> {
    let code = inst.last_exit?;
    if clean_exit(code) {
        return Some((true, "last session ended normally".into()));
    }
    let newest = std::fs::read_dir(inst.dir().join("crash-reports")).ok().and_then(|dir| {
        dir.flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .max_by_key(|(t, _)| *t)
            .map(|(_, p)| p)
    });
    let reason = newest
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|txt| txt.lines().find_map(|l| l.strip_prefix("Description:").map(|d| d.trim().to_string())));
    // A recognised cause beats the crash report's generic description: the
    // game's own output first, then the log file it left behind.
    let hint = crash::diagnose(log.into_iter().flatten().map(String::as_str))
        .or_else(|| crash::diagnose(crash::tail_latest_log(&inst.dir()).iter().map(String::as_str)));
    let reason = hint.or(reason);
    Some((false, match reason {
        Some(r) => format!("crashed (code {code}): {r}"),
        None => format!("crashed (exit code {code})"),
    }))
}

/// Drop ANSI escape sequences (some mods colour their output).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Colour for a Minecraft log line by its level.
fn log_style(th: &Theme, line: &str) -> Style {
    if line.contains("/ERROR]") || line.contains("/FATAL]") || line.contains("Exception") || line.starts_with("Caused by") {
        Style::default().fg(th.error)
    } else if line.contains("/WARN]") {
        Style::default().fg(th.warn)
    } else if line.trim_start().starts_with("at ") {
        Style::default().fg(th.muted)
    } else {
        Style::default().fg(th.fg)
    }
}

/// Split `s` into rows of at most `w` characters (hard wrap), so log
/// scrolling can count rows exactly.
fn hard_wrap(s: &str, w: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(w.max(1)).map(|c| c.iter().collect()).collect()
}

/// Key for `mod_info`: the jar name without a trailing `.disabled`, so a
/// toggled mod keeps its Modrinth match.
fn mod_key(filename: &str) -> &str {
    filename.trim_end_matches(".disabled")
}
/// Clip to at most `n` characters (ellipsis included). Char-based, so
/// multibyte titles from Modrinth never split mid-codepoint.
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}
/// Modal sized to its content (clamped to the screen), so nothing is cut off.
fn popup(frame: &mut Frame, area: Rect, th: &Theme, title: &str, color: Color, align: Alignment, lines: Vec<Line<'_>>) {
    let content_w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
    let w = (content_w + 6).max(area.width * 52 / 100).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let pop = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, pop);
    frame.render_widget(
        Paragraph::new(lines).block(block(th, format!(" {title} "), color)).alignment(align),
        pop,
    );
}

pub async fn run_tui() -> anyhow::Result<()> {
    crate::term::Term::set_quiet(true);
    let r = run_tui_inner().await;
    crate::term::Term::set_quiet(false);
    r
}

async fn run_tui_inner() -> anyhow::Result<()> {
    let mut term = ratatui::init();
    // Ask the terminal which image protocol it speaks (kitty, sixel,
    // iTerm2). Must run before the event loop reads stdin.
    let picker = ratatui_image::picker::Picker::from_query_stdio().ok();
    // Cell size in pixels, from the picker or else the terminal's own report.
    let cell = picker.as_ref().map(|p| { let f = p.font_size(); (f.width, f.height) }).or_else(|| {
        let w = crossterm::terminal::window_size().ok()?;
        (w.columns > 0 && w.rows > 0 && w.width > 0).then(|| (w.width / w.columns, w.height / w.rows))
    });
    let gfx = picker.filter(|p| p.protocol_type() != ratatui_image::picker::ProtocolType::Halfblocks);
    if Cfg::load().mouse {
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).ok();
    }
    let mut app = App::new();
    app.gfx = gfx;
    if let Some((w, h)) = cell.filter(|(w, h)| *w > 0 && *h > 0) {
        app.cell_aspect = (h as f32 / w as f32).clamp(1.5, 3.0);
    }
    // App::new() already queued instance icons without the picker, which
    // decodes them as blurry half blocks. Drop them and fetch again.
    app.icons.clear();
    app.reload_instances();
    let action = app.run_loop(&mut term).await;
    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture).ok();
    ratatui::restore();
    action?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn heads_decode_for_legacy_and_hd_skins() {
        let png = |w, h| {
            let mut b = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([200, 150, 100, 0])))
                .write_to(&mut b, image::ImageFormat::Png)
                .unwrap();
            b.into_inner()
        };
        for (w, h) in [(64, 32), (64, 64), (128, 128)] {
            let icon = Icon::decode_head(&png(w, h), None).expect("skin decodes");
            assert_eq!(icon.small.px[0][3], 255, "{w}x{h}: face is opaque");
        }
        assert!(Icon::decode_head(&png(50, 50), None).is_none());
    }

    /// Point the config and data dirs at a scratch directory so the smoke
    /// tests never touch the user's real instances.
    ///
    /// The env vars are process-global, so the returned guard serializes the
    /// tests that use them; the dir is wiped so reruns start clean.
    fn isolate(tag: &str) -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("mirage-tui-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        guard
    }

    fn render_all_tabs(draws: usize) {
        let _env = isolate("tabs");
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut app = App::new();
        // Several passes exercise the animation paths (settling, idle, and
        // the moving download rail) rather than only the first frame.
        for i in 0..draws {
            if i > 0 {
                app.advance_animations();
            }
            for tab in [
                TabIndex::Instances,
                TabIndex::Mods,
                TabIndex::SearchMods,
                TabIndex::Modpacks,
                TabIndex::NewInstance,
                TabIndex::Settings,
            ] {
                app.goto(tab);
                term.draw(|f| app.render(f)).unwrap();
            }
            app.help_open = true;
            term.draw(|f| app.render(f)).unwrap();
            app.help_open = false;
        }
    }

    #[test]
    fn trunc_is_char_safe() {
        assert_eq!(trunc("héllo wörld", 5), "héll…");
        assert_eq!(trunc("日本語のモッド", 3), "日本…");
        assert_eq!(trunc("short", 10), "short");
        assert_eq!(trunc("abc", 0), "…");
    }

    #[tokio::test]
    async fn renders_help_and_tiny_terminals_without_panicking() {
        let _env = isolate("tiny");
        let mut app = App::new();
        app.mod_search_results = vec![ModHit {
            project_id: "x".into(),
            title: "Ünïcödé 日本語 mod with a very long title indeed".into(),
            description: "描述 — émoji 🚀 description".into(),
            slug: "x".into(),
            author: Some("ä".into()),
            icon_url: None,
            downloads: Some(12),
            categories: None,
            versions: None,
            follows: Some(3),
            date_modified: Some("2025-01-02T03:04:05Z".into()),
        }];
        app.mod_search_state.select(Some(0));
        // A decoded icon, a loaded version list, an open picker and a running
        // download, so every new render path runs at every size.
        let icon_png = {
            let mut buf = Vec::new();
            image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 60, 90, 255]))
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .unwrap();
            buf
        };
        app.icons.insert("x".into(), Icon::decode(&icon_png, None));
        let ver: ModrinthVersion = serde_json::from_value(serde_json::json!({
            "id": "v1", "project_id": "x", "name": "n", "version_number": "1.0.0+ünï",
            "game_versions": ["1.20.1", "1.21.1"], "loaders": ["fabric"], "files": [],
            "dependencies": [], "version_type": "beta", "date_published": "2025", "downloads": 42
        }))
        .unwrap();
        app.project_versions.insert("x".into(), Some(Ok(vec![ver; 30])));
        app.start_bg_task("Installing Ünïcödé".into());
        download::progress().files_total.fetch_add(3, std::sync::atomic::Ordering::Relaxed);
        download::progress().files.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for (w, h) in [(120, 40), (60, 16), (20, 6), (3, 3), (1, 1)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            for i in 0..6 {
                app.goto(TabIndex::from_index(i));
                for _ in 0..3 {
                    term.draw(|f| app.render(f)).unwrap();
                    app.advance_animations();
                }
                app.help_open = true;
                term.draw(|f| app.render(f)).unwrap();
                app.help_open = false;
                app.delete_confirm_target = Some("survival".into());
                term.draw(|f| app.render(f)).unwrap();
                app.delete_confirm_target = None;
                if i == 2 {
                    app.picker = Some(Picker {
                        project_id: "x".into(),
                        title: "Ünïcödé".into(),
                        is_mod: true,
                        show_all: false,
                        state: ListState::default().with_selected(Some(3)),
                    });
                    term.draw(|f| app.render(f)).unwrap();
                    app.picker = None;
                }
            }
        }
    }

    #[tokio::test]
    async fn renders_every_tab_without_panicking() {
        render_all_tabs(3);
    }

    #[test]
    fn renders_while_a_download_is_running() {
        let _env = isolate("busy");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut app = App::new();
        app.start_bg_task("Sodium".into());
        app.set_status("Downloading Sodium…", false);
        for _ in 0..5 {
            app.advance_animations();
            term.draw(|f| app.render(f)).unwrap();
        }
        assert!(app.bg_download_active.is_some());
    }

    #[tokio::test]
    async fn selection_moves_and_clamps() {
        let _env = isolate("selection");
        let mut app = App::new();
        app.goto(TabIndex::NewInstance);
        app.matching_version_idx = 0;
        let n = app.matching_versions.len();
        assert!(n > 0);
        app.sel_step(1);
        assert_eq!(app.matching_version_idx, 1.min(n - 1));
        app.sel_step(-5);
        assert_eq!(app.matching_version_idx, 0);
        app.sel_step(50);
        assert_eq!(app.matching_version_idx, n - 1);
    }

    /// Not an assertion — writes a rendered frame to disk so the layout can
    /// be eyeballed during development (`cargo test dump_frame -- --nocapture`).
    /// Write a frame as plain text plus a `.cells` file (symbol, fg, bg per
    /// cell) that a script can turn into a coloured screenshot.
    fn dump(term: &Terminal<TestBackend>, name: &str) {
        let buf = term.backend().buffer();
        let (mut text, mut cells) = (String::new(), String::new());
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let c = &buf[(x, y)];
                text.push_str(c.symbol());
                cells.push_str(&format!("{}\t{:?}\t{:?}\n", c.symbol().replace('\t', " "), c.fg, c.bg));
            }
            text.push('\n');
        }
        std::fs::write(format!("/tmp/mirage-frames/{name}.txt"), text).unwrap();
        std::fs::write(format!("/tmp/mirage-frames/{name}.cells"), cells).unwrap();
    }

    #[tokio::test]
    async fn dump_frame() {
        let _env = isolate("dump2");
        let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
        // Fabricate a couple of instances with mods so the dense layouts get
        // exercised too, not just the empty states.
        for (name, ver, loader) in [
            ("survival", "1.21.1", "fabric"),
            ("vanilla-opt", "1.20.4", "vanilla"),
            ("kitchen-sink-smp", "1.19.4", "quilt"),
        ] {
            let inst = Instance::create(name, ver, loader, None).unwrap();
            for (i, modname) in ["sodium-fabric-0.6.5", "lithium-fabric-0.14.3", "fabric-api-0.9x"]
                .iter()
                .enumerate()
            {
                let file = if i == 2 { format!("{modname}.jar.disabled") } else { format!("{modname}.jar") };
                let _ = std::fs::write(inst.mods_dir().join(file), vec![b'x'; (12 + i * 900) * 1024]);
            }
            let _ = inst;
        }
        let mut app = App::new();
        app.cfg.anim = false; // steady state, not mid-transition
        app.reload_instances();
        app.instance_list_state.select(Some(0));
        app.reload_mods();
        app.mod_search_results = vec![
            ("Sodium", "sodium", "Modern rendering engine and client-side optimization mod", "jellysquid", 6_200_000),
            ("Fabric API", "fabric-api", "Lightweight and modular API providing common hooks", "modmuss50", 4_800_000),
            ("Lithium", "lithium", "No-compromises game logic and performance optimization", "jellysquid", 3_100_000),
            ("Iris Shaders", "iris", "A modern shaders mod compatible with OptiFine shaderpacks", "coderbot", 2_400_000),
            ("Mod Menu", "modmenu", "Adds a mod menu to view the list of mods you have installed", "Prospector", 1_900_000),
        ]
        .into_iter()
        .map(|(title, slug, desc, author, dl)| ModHit {
            project_id: match slug {
                "sodium" => "AANobbMI",
                "fabric-api" => "P7dR8mSH",
                "lithium" => "gvQqBUqZ",
                "iris" => "YL57xq9U",
                _ => slug,
            }
            .into(),
            title: title.into(),
            description: desc.into(),
            slug: slug.into(),
            author: Some(author.into()),
            icon_url: None,
            downloads: Some(dl),
            categories: Some(vec!["optimization".into(), "fabric".into()]),
            versions: None,
            follows: Some(dl / 40),
            date_modified: Some("2026-09-20T10:00:00Z".into()),
        })
        .collect();
        app.mod_search_state.select(Some(1));
        // Real icons from the user's cache when present, so the dump shows
        // how actual artwork looks; placeholders otherwise.
        if let Ok(home) = std::env::var("HOME") {
            for h in &app.mod_search_results {
                let f = Path::new(&home).join(".local/share/mirage/cache/icons").join(&h.project_id);
                if let Some(icon) = std::fs::read(f).ok().and_then(|b| Icon::decode(&b, None)) {
                    app.icons.insert(h.project_id.clone(), Some(icon));
                }
            }
        }
        app.modpack_search_results = app.mod_search_results.clone();
        app.modpack_search_state.select(Some(2));
        std::fs::create_dir_all("/tmp/mirage-frames").unwrap();
        for tab in [
            TabIndex::Instances,
            TabIndex::Mods,
            TabIndex::SearchMods,
            TabIndex::Modpacks,
            TabIndex::NewInstance,
            TabIndex::Settings,
        ] {
            app.goto(tab);
            term.draw(|f| app.render(f)).unwrap();
            dump(&term, &format!("{tab:?}"));
        }

        // Version picker over the search tab, with a download running.
        let vers: Vec<ModrinthVersion> = (0..12)
            .map(|i| {
                serde_json::from_value(serde_json::json!({
                    "id": format!("v{i}"), "project_id": "P7dR8mSH", "name": "n",
                    "version_number": format!("0.{}.0+1.21.1", 110 - i),
                    "game_versions": ["1.21", "1.21.1"], "loaders": ["fabric"], "files": [],
                    "dependencies": [], "version_type": if i == 0 { "beta" } else { "release" },
                    "date_published": "2026-09-01T00:00:00Z", "downloads": 90_000 - i * 5000
                }))
                .unwrap()
            })
            .collect();
        // Instances tab mid-launch, with playtime and a crashed last session.
        app.instance_list_state.select(Some(0));
        app.instances[0].playtime_seconds = 3 * 3600 + 12 * 60;
        app.instances[0].last_played = Some((chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339());
        app.last_session = Some((false, "crashed (code 1): Rendering overlay".into()));
        app.goto(TabIndex::Instances);
        term.draw(|f| app.render(f)).unwrap();
        dump(&term, "InstancesCrash");
        app.launching = Some(app.instances[0].name.clone());
        launcher::set_launch_stage(2);
        term.draw(|f| app.render(f)).unwrap();
        dump(&term, "InstancesLaunching");
        app.launching = None;

        app.instance_list_state.select(Some(1));
        app.project_versions.insert("P7dR8mSH".into(), Some(Ok(vers)));
        app.start_bg_task("Installing Sodium 0.6.5".into());
        download::progress().files_total.store(1, std::sync::atomic::Ordering::Relaxed);
        download::progress().set_label("retrieving sodium-fabric-0.6.5.jar");
        download::progress().files.store(0, std::sync::atomic::Ordering::Relaxed);
        app.speed = 3.2 * 1024.0 * 1024.0;
        app.goto(TabIndex::SearchMods);
        for name in ["SearchBusy", "Picker"] {
            if name == "Picker" {
                app.picker = Some(Picker {
                    project_id: "P7dR8mSH".into(),
                    title: "Fabric API".into(),
                    is_mod: true,
                    show_all: false,
                    state: ListState::default().with_selected(Some(1)),
                });
            }
            term.draw(|f| app.render(f)).unwrap();
            dump(&term, name);
        }
    }

    #[tokio::test]
    async fn tab_navigation_wraps_and_tracks_the_indicator() {
        let _env = isolate("tabs-wrap");
        let mut app = App::new();
        assert_eq!(app.current_tab, TabIndex::Instances);
        app.goto(TabIndex::Settings);
        assert_eq!(app.current_tab.next(), TabIndex::Instances);
        assert_eq!(app.current_tab.prev(), TabIndex::NewInstance);
        app.goto(TabIndex::Mods);
        assert_eq!(app.tab_pos, 1.0);
        assert_eq!(TabIndex::from_index(3), TabIndex::Modpacks);
        assert_eq!(TabIndex::from_index(99), TabIndex::Instances);
    }

    #[tokio::test]
    async fn sessions_fold_into_playtime_and_crash_summary() {
        let _env = isolate("sessions");
        let inst = Instance::create("pt", "1.21.1", "fabric", None).unwrap();
        inst.record_session(600, 0).unwrap();
        inst.record_session(125, 1).unwrap();
        let crash = inst.dir().join("crash-reports");
        std::fs::create_dir_all(&crash).unwrap();
        std::fs::write(crash.join("crash-1.txt"), "---- Minecraft Crash Report ----\nDescription: Rendering overlay\n").unwrap();

        let loaded = Instance::load("pt").unwrap();
        assert_eq!(loaded.playtime_seconds, 725);
        assert_eq!(loaded.last_exit, Some(1));
        assert!(!loaded.sessions_path().exists(), "log is cleared once folded");
        assert_eq!(Instance::load("pt").unwrap().playtime_seconds, 725, "folding is not repeated");
        let (ok, text) = session_summary(&loaded, None).unwrap();
        assert!(!ok);
        assert!(text.contains("Rendering overlay"), "{text}");

        inst.record_session(5, 0).unwrap();
        assert!(session_summary(&Instance::load("pt").unwrap(), None).unwrap().0);
    }

    #[test]
    fn placeholder_tile_stays_square_on_tall_cells() {
        let th = Theme::DEFAULT;
        assert_eq!(icon_placeholder(&th, "x", ICON_LARGE, 2.0).len(), 8);
        assert_eq!(icon_placeholder(&th, "x", ICON_LARGE, 2.4).len(), 7);
        assert_eq!(icon_placeholder(&th, "x", ICON_SMALL, 2.4).len(), 2, "list tiles keep their two rows");
    }

    #[tokio::test]
    async fn instance_settings_validate_then_save() {
        let _env = isolate("edit");
        Instance::create("ed", "1.21.1", "fabric", None).unwrap();
        let mut app = App::new();
        app.open_edit();
        assert_eq!(app.input_mode, InputMode::EditingInstance);

        let set = |app: &mut App, v: [&str; 4]| app.edit.as_mut().unwrap().vals = v.map(String::from);
        set(&mut app, ["8G", "2G", "", ""]);
        app.save_edit();
        assert!(app.edit.is_some() && app.status_is_error, "min above max is refused");
        set(&mut app, ["2G", "6x", "", ""]);
        app.save_edit();
        assert_eq!(app.edit.as_ref().unwrap().field, 1, "bad size jumps to its field");
        set(&mut app, ["2G", "6G", "/no/such/java", ""]);
        app.save_edit();
        assert_eq!(app.edit.as_ref().unwrap().field, 2);

        set(&mut app, ["2G", "6G", "", "-Dfoo=1  -XX:+UseZGC"]);
        app.save_edit();
        assert!(app.edit.is_none() && app.input_mode == InputMode::Normal);
        let saved = Instance::load("ed").unwrap();
        assert_eq!(saved.ram_max.as_deref(), Some("6G"));
        assert_eq!(saved.jvm_args, ["-Dfoo=1", "-XX:+UseZGC"]);
        assert!(saved.java_path.is_none());
    }

    #[tokio::test]
    async fn mods_list_flags_updates_and_the_popup_renders() {
        let _env = isolate("updates");
        let inst = Instance::create("up", "1.21.1", "fabric", None).unwrap();
        std::fs::write(inst.mods_dir().join("sodium-0.5.jar"), b"x").unwrap();
        let mut app = App::new();
        app.online = false; // no Modrinth lookups from a unit test
        app.reload_instances();
        app.goto(TabIndex::Mods);
        let ver: ModrinthVersion = serde_json::from_value(serde_json::json!({
            "id": "v2", "project_id": "AANobbMI", "name": "Sodium", "version_number": "0.6.0",
            "game_versions": ["1.21.1"], "loaders": ["fabric"], "files": [], "dependencies": [],
            "version_type": "release", "date_published": "2026-09-01T00:00:00Z", "downloads": 1
        }))
        .unwrap();
        app.mod_updates.insert("up/sodium-0.5.jar".into(), ver);
        let text = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
        };
        let shown = text(&mut app);
        assert!(shown.contains("↑ update available → 0.6.0"), "detail shows the new version");
        assert!(shown.contains("1 update"), "list counter shows pending updates");

        app.goto(TabIndex::Instances);
        app.open_edit();
        assert!(text(&mut app).contains("Settings · up"));
    }

    #[test]
    fn theme_cycles_through_every_registered_theme() {
        let mut theme = Theme::DEFAULT;
        let mut seen = vec![theme.name];
        for _ in 0..Theme::all().len() - 1 {
            theme = theme.next().1;
            seen.push(theme.name);
        }
        for name in Theme::all() {
            assert!(seen.contains(&name), "theme {name} unreachable by cycling");
        }
        // Cycler must come back to the start rather than drifting.
        assert_eq!(theme.next().1.name, Theme::DEFAULT.name);
        assert_eq!(Theme::load("nord").name, "nord");
        assert_eq!(Theme::load("nonsense").name, Theme::DEFAULT.name);
    }
}
