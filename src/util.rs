use std::path::{Path, PathBuf};

/// Total size in bytes of a file or a directory tree (symlinks are not followed).
pub fn path_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Lowercase alphanumerics only: "Visual Studio Code" -> "visualstudiocode".
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Strips well-known suffixes that apps append to their data entries.
pub fn strip_known_ext(name: &str) -> &str {
    const EXTS: [&str; 7] = [
        ".plist",
        ".savedState",
        ".binarycookies",
        ".sfl2",
        ".lockfile",
        ".desktop",
        ".json",
    ];
    for ext in EXTS {
        if let Some(stripped) = name.strip_suffix(ext) {
            return stripped;
        }
    }
    name
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Whether an executable with this name exists somewhere in PATH.
pub fn in_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let p = dir.join(name);
        p.is_file() || (cfg!(windows) && dir.join(format!("{name}.exe")).is_file())
    })
}

/// Guard against deleting obviously dangerous locations.
pub fn is_safe_to_delete(path: &Path) -> bool {
    if !path.is_absolute() || path.components().count() < 3 {
        return false;
    }
    let home = home();
    let forbidden = [
        home.clone(),
        home.join("Library"),
        home.join("Documents"),
        home.join("Desktop"),
        home.join(".config"),
        home.join(".local"),
        home.join(".local/share"),
        home.join(".cache"),
    ];
    const SYSTEM: [&str; 16] = [
        "/usr", "/usr/bin", "/usr/lib", "/usr/share", "/usr/local", "/bin", "/etc", "/opt",
        "/var", "/System", "/Library", "/Applications", "/Users", "/home",
        "C:\\Windows", "C:\\Program Files",
    ];
    if forbidden.iter().any(|f| f == path) || SYSTEM.iter().any(|s| Path::new(s) == path) {
        return false;
    }
    if crate::leftovers::search_roots().iter().any(|r| r == path) {
        return false;
    }
    !crate::cache::cache_roots().iter().any(|(_, r)| r == path)
}
