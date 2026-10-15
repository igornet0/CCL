//! Cache locations, their sizes and the user's remembered keep/delete choices.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util::path_size;

#[derive(Clone, Debug)]
pub struct CacheItem {
    pub path: PathBuf,
    pub name: String,
    pub group: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Not decided: not deleted, not remembered.
    Skip,
    /// Protected: never deleted, remembered.
    Keep,
    /// Deleted on cleanup, remembered and preselected next time.
    Delete,
}

/// Remembered decisions, stored in the user config directory.
#[derive(Default, Serialize, Deserialize)]
pub struct Policy {
    pub keep: BTreeSet<PathBuf>,
    pub delete: BTreeSet<PathBuf>,
    /// Leftovers the user marked as "not a leftover".
    #[serde(default)]
    pub ignore_leftovers: BTreeSet<PathBuf>,
}

impl Policy {
    fn file() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ccl").join("policy.json"))
    }

    pub fn load() -> Self {
        Self::file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let file = Self::file().ok_or("нет каталога настроек")?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(file, json).map_err(|e| e.to_string())
    }

    pub fn choice(&self, path: &Path) -> Choice {
        if self.keep.contains(path) {
            Choice::Keep
        } else if self.delete.contains(path) {
            Choice::Delete
        } else {
            Choice::Skip
        }
    }

    pub fn set(&mut self, path: &Path, choice: Choice) {
        self.keep.remove(path);
        self.delete.remove(path);
        match choice {
            Choice::Keep => {
                self.keep.insert(path.to_path_buf());
            }
            Choice::Delete => {
                self.delete.insert(path.to_path_buf());
            }
            Choice::Skip => {}
        }
    }
}

/// Top-level cache directories: every child becomes a separate item.
pub fn cache_roots() -> Vec<(String, PathBuf)> {
    let home = crate::util::home();
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        roots.push(("Кэши приложений".into(), home.join("Library/Caches")));
        roots.push(("Логи".into(), home.join("Library/Logs")));
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            roots.push(("Кэш браузера IE/Edge".into(), local.join(r"Microsoft\Windows\INetCache")));
            roots.push(("Отчёты об ошибках".into(), local.join("CrashDumps")));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        roots.push(("Кэши приложений".into(), home.join(".cache")));
    }
    roots.push(("Временные файлы".into(), std::env::temp_dir()));
    let _ = &home;
    roots.retain(|(_, p)| p.is_dir());
    roots.dedup_by(|a, b| a.1 == b.1);
    roots
}

/// Data directories that contain per-app cache folders (Electron/Chromium style).
fn nested_cache_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    #[cfg(target_os = "macos")]
    bases.push(crate::util::home().join("Library/Application Support"));
    #[cfg(windows)]
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(var) {
            bases.push(PathBuf::from(v));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    bases.push(crate::util::home().join(".config"));
    bases.retain(|b| b.is_dir());
    bases
}

const NESTED_CACHE_NAMES: [&str; 8] = [
    "Cache",
    "cache",
    "Code Cache",
    "GPUCache",
    "cache2",
    "ShaderCache",
    "GrShaderCache",
    "CachedData",
];

pub fn scan() -> Vec<CacheItem> {
    let mut items = Vec::new();
    for (group, root) in cache_roots() {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for entry in rd.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".DS_Store" || name == ".localized" {
                continue;
            }
            let size = path_size(&path);
            items.push(CacheItem { path, name, group: group.clone(), size });
        }
    }
    for base in nested_cache_bases() {
        let walker = walkdir::WalkDir::new(&base)
            .min_depth(2)
            .max_depth(5)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.file_type().is_dir());
        let mut it = walker;
        while let Some(entry) = it.next() {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy();
            if NESTED_CACHE_NAMES.contains(&name.as_ref()) {
                let path = entry.path().to_path_buf();
                it.skip_current_dir();
                let rel = path.strip_prefix(&base).unwrap_or(&path).display().to_string();
                let size = path_size(&path);
                if size > 0 {
                    items.push(CacheItem { path, name: rel, group: "Кэши внутри данных приложений".into(), size });
                }
            }
        }
    }
    items.sort_by(|a, b| a.group.cmp(&b.group).then(b.size.cmp(&a.size)));
    items
}
