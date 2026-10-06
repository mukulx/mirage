use crossterm::style::{Color, Stylize};

pub struct Theme;

impl Theme {
    pub const ACCENT: Color = Color::AnsiValue(47);
    pub const INFO: Color = Color::AnsiValue(117);
    pub const WARN: Color = Color::AnsiValue(221);
    pub const MUTED: Color = Color::AnsiValue(245);
    pub const BRIGHT: Color = Color::AnsiValue(251);
    pub const ORANGE: Color = Color::AnsiValue(173);
    pub const GREEN: Color = Color::AnsiValue(46);
    pub const RED: Color = Color::AnsiValue(196);
    pub const CYAN: Color = Color::AnsiValue(51);
    pub const BG_HI: Color = Color::AnsiValue(240);
}

pub struct Term;

use std::sync::atomic::{AtomicBool, Ordering};
static QUIET: AtomicBool = AtomicBool::new(false);

impl Term {
    /// When true (TUI active), all Term output is suppressed so background
    /// tasks can't corrupt the alternate screen.
    pub fn set_quiet(q: bool) {
        QUIET.store(q, Ordering::Relaxed);
    }
    pub fn is_quiet() -> bool {
        QUIET.load(Ordering::Relaxed)
    }

    pub fn header(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!(
            "\n  {} {}",
            "▌".with(Theme::ACCENT).bold(),
            msg.with(Theme::ACCENT).bold()
        );
    }

    pub fn subheader(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!("  {}", msg.with(Theme::INFO).bold());
    }

    pub fn info(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!("  {} {}", "▸".with(Theme::INFO), msg);
    }

    pub fn done(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!(
            "  {} {}",
            "✔".with(Theme::GREEN).bold(),
            msg.with(Theme::BRIGHT)
        );
    }

    pub fn success(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!(
            "  {} {}",
            "✔".with(Theme::GREEN).bold(),
            msg.with(Theme::GREEN)
        );
    }

    pub fn warn(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!(
            "  {} {}",
            "⚠".with(Theme::WARN).bold(),
            msg.with(Theme::WARN)
        );
    }

    pub fn err(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        eprintln!(
            "  {} {}",
            "✗".with(Theme::RED).bold(),
            msg.with(Theme::RED)
        );
    }

    pub fn cmd(msg: &str) {
        if Self::is_quiet() {
            return;
        }
        println!(
            "  {} {}",
            "$".with(Theme::INFO).bold(),
            msg.with(Theme::BRIGHT)
        );
    }

    pub fn label(key: &str, val: &str) {
        if Self::is_quiet() {
            return;
        }
        println!(
            "  {:18} {}",
            format!("{}:", key).with(Theme::MUTED),
            val.with(Theme::BRIGHT)
        );
    }

    pub fn item(bullet: &str, text: &str) {
        if Self::is_quiet() {
            return;
        }
        println!("    {} {}", bullet.with(Theme::ACCENT), text);
    }

    pub fn banner() {
        fn o(s: &str) {
            println!("  {}", s.with(Theme::CYAN));
        }
        o("█▀▄▀█ ▀█▀ █▀█ ▄▀█ █▀▀ █▀▀");
        o("█ ▀ █ ▄█▄ █▀▄ █▀█ █▄█ ██▄");
        println!(
            "  {} {}",
            "A fast terminal launcher for Minecraft".with(Theme::MUTED),
            format!("v{}", env!("CARGO_PKG_VERSION")).with(Theme::ACCENT).bold()
        );
        println!();
    }
}
