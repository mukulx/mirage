//! Turn the output of a failed game session into one plain-language hint.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Last lines of `<instance>/logs/latest.log` (empty when there is none).
pub fn tail_latest_log(instance_dir: &Path) -> Vec<String> {
    const WINDOW: u64 = 256 * 1024;
    let Ok(mut f) = std::fs::File::open(instance_dir.join("logs").join("latest.log")) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(WINDOW))).is_err() || f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf).lines().map(String::from).collect()
}

/// The most likely cause of a crash, from the game's output, newest lines
/// last. `None` when nothing recognisable is in there.
pub fn diagnose<'a>(lines: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let all: Vec<&str> = lines.into_iter().collect();
    let lines = &all[all.len().saturating_sub(5000)..];
    let find = |pred: &dyn Fn(&str) -> bool| lines.iter().copied().find(|l| pred(l));
    let has = |needles: &[&str]| find(&|l| needles.iter().any(|n| l.contains(n)));

    if let Some(l) = has(&["UnsupportedClassVersionError"]) {
        // "... (class file version 65.0), ... only recognizes class file versions up to 61.0"
        let need = l
            .split("class file version ")
            .nth(1)
            .and_then(|r| r.split('.').next())
            .and_then(|n| n.trim().parse::<u32>().ok())
            .map(|n| n.saturating_sub(44));
        return Some(match need {
            Some(j) => format!("needs Java {j} or newer — install it or set its path with E"),
            None => "Java is too old for this game or a mod — set a newer one with E".into(),
        });
    }
    if has(&["OutOfMemoryError"]).is_some() {
        return Some("ran out of memory — raise Max RAM with E".into());
    }
    if has(&["Could not reserve enough space", "Invalid maximum heap size", "Invalid initial heap size", "Too small initial heap"]).is_some() {
        return Some("RAM setting is too big for this machine — lower it with E".into());
    }
    if let Some(l) = has(&["which is missing", "but only the wrong version is present"]) {
        let l = l.find("Mod '").map_or(l, |i| &l[i..]).trim();
        return Some(format!("mod dependency problem: {l}"));
    }
    if has(&["Incompatible mods found"]).is_some() {
        return Some("incompatible mods or missing dependencies — see the log".into());
    }
    if find(&|l| l.to_ascii_lowercase().contains("duplicate mod")).is_some() {
        return Some("the same mod is installed twice — remove one copy in Mods".into());
    }
    if let Some(l) = has(&["Mixin apply for mod "]) {
        let id = l.split("Mixin apply for mod ").nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or("a mod");
        return Some(format!("'{id}' failed to patch the game — it likely targets another Minecraft version"));
    }
    if has(&["MixinApplyError", "InvalidMixinException", "Mixin transformation of", "Mixin apply failed"]).is_some() {
        return Some("a mod failed to patch the game — it doesn't fit this version or clashes with another".into());
    }
    if has(&["GLFW error", "Pixel format not accepted", "GLXBadFBConfig"]).is_some() {
        return Some("graphics problem — update GPU drivers or remove rendering mods".into());
    }
    if has(&["NoClassDefFoundError", "ClassNotFoundException", "NoSuchMethodError", "NoSuchFieldError"]).is_some() {
        return Some("a mod lacks a library or targets another version — check dependencies, update mods".into());
    }
    lines.iter().rev().find_map(|l| l.trim().strip_prefix("Caused by: ")).map(|c| format!("error: {c}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(log: &str) -> Option<String> {
        diagnose(log.lines())
    }

    #[test]
    fn recognises_common_failures() {
        let java = hint("Exception in thread \"main\" java.lang.UnsupportedClassVersionError: x (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 61.0");
        assert!(java.unwrap().contains("Java 21"));
        assert!(hint("java.lang.OutOfMemoryError: Java heap space").unwrap().contains("memory"));
        assert!(hint("Error occurred during initialization of VM\nCould not reserve enough space for object heap").unwrap().contains("too big"));
        let dep = hint("[main/ERROR]: - Mod 'Sodium' (sodium) 0.5 requires any version of fabric-api, which is missing!").unwrap();
        assert!(dep.contains("Mod 'Sodium'") && dep.contains("fabric-api"), "{dep}");
        assert!(hint("Mixin apply for mod lithium failed lithium.mixins.json").unwrap().contains("'lithium'"));
        assert!(hint("boom\nCaused by: java.lang.IllegalStateException: nope").unwrap().contains("IllegalStateException"));
        assert!(hint("all fine here").is_none());
    }
}
