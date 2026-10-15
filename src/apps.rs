//! Discovery of installed applications on each platform.

use std::path::PathBuf;

use crate::util::normalize;

/// How an application gets removed.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Uninstaller {
    /// Remove the bundle/folder/file itself (macOS .app, AppImage, portable apps).
    RemovePath(PathBuf),
    /// Run the vendor uninstaller command line (Windows registry UninstallString).
    Command(String),
    Flatpak(String),
    Snap(String),
    /// A package owned by the system package manager (dpkg/rpm/pacman).
    Package(String),
    /// No known way; only the related data can be removed.
    None,
}

#[derive(Clone, Debug)]
pub struct InstalledApp {
    pub name: String,
    pub version: Option<String>,
    pub publisher: Option<String>,
    /// Bundle id (macOS), registry key (Windows), desktop/flatpak id (Linux).
    pub id: Option<String>,
    pub location: Option<PathBuf>,
    pub uninstaller: Uninstaller,
    pub source: &'static str,
}

impl InstalledApp {
    /// Normalized names this app's data folders are likely to use.
    pub fn tokens(&self) -> Vec<String> {
        let mut raw: Vec<String> = vec![self.name.clone()];
        if let Some(id) = &self.id {
            raw.push(id.clone());
            let parts: Vec<&str> = id.split('.').collect();
            if let Some(last) = parts.last() {
                raw.push(last.to_string());
                // ru.yandex.desktop.music -> "yandexmusic"
                if parts.len() >= 3 {
                    raw.push(format!("{}{}", parts[1], last));
                }
            }
        }
        if let Some(loc) = &self.location {
            if let Some(stem) = loc.file_stem() {
                raw.push(stem.to_string_lossy().into_owned());
            }
        }
        match &self.uninstaller {
            Uninstaller::Flatpak(s) | Uninstaller::Snap(s) | Uninstaller::Package(s) => {
                raw.push(s.clone())
            }
            _ => {}
        }
        let mut out: Vec<String> = raw
            .iter()
            .map(|s| normalize(s))
            .filter(|s| s.len() >= 3)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Lowercased reverse-DNS id, if the app has one (com.vendor.app).
    pub fn reverse_dns_id(&self) -> Option<String> {
        self.id
            .as_ref()
            .filter(|id| id.matches('.').count() >= 2)
            .map(|id| id.to_lowercase())
    }

    pub fn can_uninstall(&self) -> bool {
        !matches!(self.uninstaller, Uninstaller::None)
    }
}

pub fn list_installed() -> Vec<InstalledApp> {
    let mut apps = platform::list();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.name == b.name && a.location == b.location);
    apps
}

/// Extra identifiers of system components that are never uninstalled by the user
/// but own data folders (used to avoid reporting them as leftovers).
pub fn system_tokens() -> Vec<String> {
    platform::system_tokens()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::path::Path;

    fn read_bundle(path: &Path) -> Option<InstalledApp> {
        let plist = plist::Value::from_file(path.join("Contents/Info.plist")).ok()?;
        let dict = plist.as_dictionary()?;
        let get = |k: &str| dict.get(k).and_then(|v| v.as_string()).map(str::to_string);
        let fallback = path.file_stem()?.to_string_lossy().into_owned();
        let name = get("CFBundleDisplayName")
            .or_else(|| get("CFBundleName"))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(fallback);
        Some(InstalledApp {
            name,
            version: get("CFBundleShortVersionString").or_else(|| get("CFBundleVersion")),
            publisher: get("NSHumanReadableCopyright"),
            id: get("CFBundleIdentifier"),
            location: Some(path.to_path_buf()),
            uninstaller: Uninstaller::RemovePath(path.to_path_buf()),
            source: "Applications",
        })
    }

    fn bundles_in(dir: &Path, depth: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut it = walkdir::WalkDir::new(dir).max_depth(depth).sort_by_file_name().into_iter();
        while let Some(entry) = it.next() {
            let Ok(entry) = entry else { continue };
            if entry.depth() > 0 && entry.path().extension().is_some_and(|x| x == "app") {
                // skip_current_dir() on a non-directory (e.g. the Safari.app symlink)
                // would skip the rest of the *parent* directory instead.
                if entry.file_type().is_dir() {
                    it.skip_current_dir();
                }
                out.push(entry.into_path());
            }
        }
        out
    }

    pub fn list() -> Vec<InstalledApp> {
        let mut dirs = vec![PathBuf::from("/Applications")];
        dirs.push(crate::util::home().join("Applications"));
        dirs.iter()
            .flat_map(|d| bundles_in(d, 3))
            .filter_map(|p| read_bundle(&p))
            .collect()
    }

    pub fn system_tokens() -> Vec<String> {
        ["/System/Applications", "/System/Library/CoreServices"]
            .iter()
            .flat_map(|d| bundles_in(Path::new(d), 3))
            .filter_map(|p| read_bundle(&p))
            .flat_map(|a| a.tokens())
            .collect()
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use winreg::enums::*;
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    const UNINSTALL_WOW: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";

    fn read_root(root: &RegKey, path: &str, source: &'static str, out: &mut Vec<InstalledApp>) {
        let Ok(key) = root.open_subkey_with_flags(path, KEY_READ) else {
            return;
        };
        for sub in key.enum_keys().filter_map(Result::ok) {
            let Ok(k) = key.open_subkey_with_flags(&sub, KEY_READ) else {
                continue;
            };
            let s = |n: &str| k.get_value::<String, _>(n).ok().filter(|v| !v.trim().is_empty());
            let Some(name) = s("DisplayName") else { continue };
            if k.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1
                || s("ParentKeyName").is_some()
                || s("ReleaseType").is_some_and(|t| t.contains("Update") || t.contains("Hotfix"))
            {
                continue;
            }
            let uninstall = s("UninstallString").or_else(|| s("QuietUninstallString"));
            let location = s("InstallLocation")
                .map(|l| PathBuf::from(l.trim_matches('"')))
                .filter(|p| p.is_dir());
            let uninstaller = match (&uninstall, &location) {
                (Some(cmd), _) => Uninstaller::Command(cmd.clone()),
                (None, Some(loc)) => Uninstaller::RemovePath(loc.clone()),
                _ => Uninstaller::None,
            };
            out.push(InstalledApp {
                name,
                version: s("DisplayVersion"),
                publisher: s("Publisher"),
                id: Some(sub.clone()),
                location,
                uninstaller,
                source,
            });
        }
    }

    pub fn list() -> Vec<InstalledApp> {
        let mut out = Vec::new();
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        read_root(&hklm, UNINSTALL, "Реестр (HKLM)", &mut out);
        read_root(&hklm, UNINSTALL_WOW, "Реестр (HKLM x86)", &mut out);
        read_root(&hkcu, UNINSTALL, "Реестр (HKCU)", &mut out);
        out
    }

    pub fn system_tokens() -> Vec<String> {
        Vec::new()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;

    fn desktop_dirs() -> Vec<PathBuf> {
        let home = crate::util::home();
        let mut dirs = vec![
            PathBuf::from("/usr/share/applications"),
            PathBuf::from("/usr/local/share/applications"),
            PathBuf::from("/var/lib/flatpak/exports/share/applications"),
            PathBuf::from("/var/lib/snapd/desktop/applications"),
            home.join(".local/share/applications"),
            home.join(".local/share/flatpak/exports/share/applications"),
        ];
        dirs.dedup();
        dirs
    }

    struct Desktop {
        path: PathBuf,
        name: String,
        exec: Option<String>,
    }

    fn parse_desktop(path: &Path) -> Option<Desktop> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut in_entry = false;
        let (mut name, mut exec, mut hidden, mut is_app) = (None, None, false, true);
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "Name" => name = Some(v.trim().to_string()),
                "Exec" => exec = Some(v.trim().to_string()),
                "NoDisplay" | "Hidden" => hidden |= v.trim() == "true",
                "Type" => is_app = v.trim() == "Application",
                _ => {}
            }
        }
        if hidden || !is_app {
            return None;
        }
        Some(Desktop { path: path.to_path_buf(), name: name?, exec })
    }

    fn exec_binary(exec: &str) -> Option<PathBuf> {
        let first = exec.split_whitespace().find(|t| !t.contains('='))?;
        let p = PathBuf::from(first.trim_matches('"'));
        p.is_absolute().then_some(p)
    }

    /// Map desktop files to the system package that owns them (dpkg or rpm), in one call.
    fn owners(files: &[PathBuf]) -> HashMap<PathBuf, String> {
        let mut map = HashMap::new();
        if files.is_empty() {
            return map;
        }
        if crate::util::in_path("dpkg") {
            if let Ok(out) = Command::new("dpkg").arg("-S").args(files).output() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some((pkg, path)) = line.split_once(": ") {
                        let pkg = pkg.split(',').next().unwrap_or(pkg).split(':').next().unwrap_or(pkg);
                        map.insert(PathBuf::from(path.trim()), pkg.trim().to_string());
                    }
                }
            }
        } else if crate::util::in_path("rpm") {
            for f in files {
                if let Ok(out) = Command::new("rpm").args(["-qf", "--qf", "%{NAME}"]).arg(f).output() {
                    if out.status.success() {
                        map.insert(f.clone(), String::from_utf8_lossy(&out.stdout).trim().to_string());
                    }
                }
            }
        } else if crate::util::in_path("pacman") {
            for f in files {
                if let Ok(out) = Command::new("pacman").args(["-Qqo"]).arg(f).output() {
                    if out.status.success() {
                        map.insert(f.clone(), String::from_utf8_lossy(&out.stdout).trim().to_string());
                    }
                }
            }
        }
        map
    }

    pub fn list() -> Vec<InstalledApp> {
        let entries: Vec<Desktop> = desktop_dirs()
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
            .filter_map(|p| parse_desktop(&p))
            .collect();

        let system_files: Vec<PathBuf> = entries
            .iter()
            .filter(|d| d.path.starts_with("/usr"))
            .map(|d| d.path.clone())
            .collect();
        let owners = owners(&system_files);

        entries
            .into_iter()
            .map(|d| {
                let stem = d.path.file_stem().unwrap().to_string_lossy().into_owned();
                let path_str = d.path.to_string_lossy();
                let binary = d.exec.as_deref().and_then(exec_binary);
                let (uninstaller, source, location) = if path_str.contains("/flatpak/") {
                    (Uninstaller::Flatpak(stem.clone()), "Flatpak", None)
                } else if path_str.contains("/snapd/") {
                    let snap = stem.split('_').next().unwrap_or(&stem).to_string();
                    (Uninstaller::Snap(snap.clone()), "Snap", Some(PathBuf::from("/snap").join(snap)))
                } else if let Some(pkg) = owners.get(&d.path) {
                    (Uninstaller::Package(pkg.clone()), "Пакет", binary.clone())
                } else if let Some(b) = binary.as_ref().filter(|b| b.starts_with(crate::util::home())) {
                    // AppImage or a portable app in the home directory.
                    (Uninstaller::RemovePath(b.clone()), "Локально", Some(b.clone()))
                } else {
                    (Uninstaller::None, "Ярлык", binary.clone())
                };
                InstalledApp {
                    name: d.name,
                    version: None,
                    publisher: None,
                    id: Some(stem),
                    location,
                    uninstaller,
                    source,
                }
            })
            .collect()
    }

    pub fn system_tokens() -> Vec<String> {
        Vec::new()
    }
}

/// Folders and files that belong to an app but are not its main location.
pub fn desktop_entry_paths(app: &InstalledApp) -> Vec<PathBuf> {
    let _ = app;
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let (Uninstaller::RemovePath(_), Some(id)) = (&app.uninstaller, &app.id) {
            let p = crate::util::home()
                .join(".local/share/applications")
                .join(format!("{id}.desktop"));
            if p.exists() {
                return vec![p];
            }
        }
    }
    Vec::new()
}

