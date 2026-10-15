//! Search for data folders/files that belong to an app or to apps that are no longer installed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::apps::InstalledApp;
use crate::util::{normalize, path_size, strip_known_ext};

#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub size: u64,
    /// Human readable location category ("Application Support", "AppData", ...).
    pub location: String,
}

/// Directories where applications keep their per-user/per-machine data.
pub fn search_roots() -> Vec<PathBuf> {
    let home = crate::util::home();
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let lib = home.join("Library");
        for d in [
            "Application Support",
            "Caches",
            "Preferences",
            "Logs",
            "Saved Application State",
            "Containers",
            "Group Containers",
            "HTTPStorages",
            "WebKit",
            "LaunchAgents",
            "Cookies",
            "Application Scripts",
        ] {
            roots.push(lib.join(d));
        }
        roots.push(PathBuf::from("/Library/Application Support"));
        roots.push(PathBuf::from("/Library/Caches"));
        roots.push(PathBuf::from("/Library/LaunchAgents"));
        roots.push(PathBuf::from("/Library/LaunchDaemons"));
    }
    #[cfg(windows)]
    {
        for var in ["APPDATA", "LOCALAPPDATA", "PROGRAMDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(v) = std::env::var_os(var) {
                roots.push(PathBuf::from(v));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        roots.push(home.join(".config"));
        roots.push(home.join(".local/share"));
        roots.push(home.join(".cache"));
        roots.push(home.join(".var/app"));
        roots.push(home.join(".local/state"));
    }
    let _ = &home;
    roots.retain(|r| r.is_dir());
    roots.dedup();
    roots
}

fn root_label(root: &Path) -> String {
    let home = crate::util::home();
    match root.strip_prefix(&home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => root.display().to_string(),
    }
}

fn children(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Does the entry name (e.g. "com.google.Chrome.plist", "Code", "Mozilla") belong to the app?
fn matches_app(entry: &str, tokens: &[String], rdns: Option<&str>) -> bool {
    let raw = strip_known_ext(entry).to_lowercase();
    if let Some(id) = rdns {
        // com.foo.app, com.foo.app.helper, group.com.foo.app, TEAMID.com.foo.app
        if raw == id
            || raw.starts_with(&format!("{id}."))
            || raw.ends_with(&format!(".{id}"))
            || raw.contains(&format!(".{id}."))
        {
            return true;
        }
    }
    let norm = normalize(&raw);
    !norm.is_empty() && tokens.iter().any(|t| *t == norm)
}

/// Everything in the search roots that looks like it belongs to `app`.
pub fn related_to(app: &InstalledApp) -> Vec<Found> {
    let tokens = app.tokens();
    let rdns = app.reverse_dns_id();
    let publisher = app.publisher.as_deref().map(normalize).filter(|p| p.len() >= 3);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let location = app.location.clone();

    for root in search_roots() {
        let label = root_label(&root);
        for child in children(&root) {
            if location.as_ref().is_some_and(|l| child.starts_with(l) || l.starts_with(&child)) {
                continue;
            }
            let name = file_name(&child);
            if matches_app(&name, &tokens, rdns.as_deref()) {
                if seen.insert(child.clone()) {
                    out.push(Found { size: path_size(&child), path: child, location: label.clone() });
                }
                continue;
            }
            // Vendor folder, e.g. AppData/Roaming/Mozilla/Firefox or ~/.config/JetBrains/IdeaIC.
            let is_vendor = publisher.as_ref().is_some_and(|p| *p == normalize(&name) || p.starts_with(&normalize(&name)) && name.len() >= 4);
            if child.is_dir() && (is_vendor || cfg!(windows)) {
                for grand in children(&child) {
                    if matches_app(&file_name(&grand), &tokens, rdns.as_deref()) && seen.insert(grand.clone()) {
                        out.push(Found { size: path_size(&grand), location: format!("{label}/{name}"), path: grand });
                    }
                }
            }
        }
    }
    for p in crate::apps::desktop_entry_paths(app) {
        if seen.insert(p.clone()) {
            out.push(Found { size: path_size(&p), location: "Ярлык".into(), path: p });
        }
    }
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

/// Names that belong to the OS itself and must never be reported as leftovers.
fn is_system_entry(name: &str) -> bool {
    let lower = name.to_lowercase();
    // com.apple.x, group.com.apple.x, TEAMID.com.apple.x
    if name.starts_with('.') || lower.starts_with("com.apple.") || lower.contains(".com.apple.") {
        return true;
    }
    #[cfg(target_os = "macos")]
    const SYSTEM: &[&str] = &[
        "apple", "addressbook", "callhistorydb", "callhistorytransactions", "clouddocs",
        "crashreporter", "diagnosticreports", "dock", "familycircle", "fileprovider",
        "icdd", "knowledge", "mobilesync", "networkserviceproxy", "syncservices", "ubiquity",
        "accounts", "assistant", "assistants", "audio", "bluetooth", "cloudkit", "contacts",
        "containermanager", "coredata", "corefollowup", "default", "desktop", "dmd",
        "geoservices", "homekit", "icloud", "identityservices", "installer", "keychains",
        "launchservices", "locationaccessstored", "messages", "metadata", "mobiledocuments",
        "passkit", "photos", "quicklook", "sharedfilelist", "siri", "spotlight",
        "systemmigration", "tcc", "videoconference", "replayd", "gamekit", "safari",
        "mail", "music", "screen", "cloudstorage", "animoji", "stickers", "telemetry",
        "byhost", "loginitems", "pbs", "updates", "applicationsupport", "googleupdater",
        "commcenter", "fsevents", "keyboardservices", "liveactivities", "translation",
        "weather", "xcode", "developer", "fontregistry", "sharedimagecache", "persistent",
        "caches", "cache", "logs", "preferences", "crashpad", "temp", "tmp",
        "familycircled", "askpermissiond", "btserver", "coresimulator", "ilifemediabrowser",
        "akd", "nsurlsessiond", "parsecd", "suggestd", "triald", "biomesyncd", "mediaanalysisd",
    ];
    #[cfg(windows)]
    const SYSTEM: &[&str] = &[
        "microsoft", "packages", "temp", "windows", "windowsapps", "packagecache",
        "commonfiles", "comms", "connecteddevicesplatform", "d3dscache", "crashdumps",
        "programs", "microsoftnet", "msbuild", "referenceassemblies", "windowspowershell",
        "windowsdefender", "windowsmail", "windowsmediaplayer", "windowsnt",
        "windowsphotoviewer", "windowsportabledevices", "windowssidebar", "internetexplorer",
        "modifiablewindowsapps", "uninstallinformation", "ssh", "regid19911002commicrosoft",
        "usoshared", "usoprivate", "softwaredistribution", "desktop", "documents",
        "applicationdata", "startmenu", "templates", "ntuser", "peernetworking", "history",
        "intel", "nvidia", "nvidiacorporation", "amd", "realtek", "dell", "hp", "lenovo",
        "virtualstore", "placeholdertilelogofolder", "publisher", "iconcache",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    const SYSTEM: &[&str] = &[
        "autostart", "fontconfig", "gtk20", "gtk30", "gtk40", "dconf", "pulse", "systemd",
        "userdirsdirs", "userdirslocale", "mimeappslist", "ibus", "menus", "applications",
        "icons", "fonts", "mime", "trash", "recentlyusedxbel", "keyrings", "gvfsmetadata",
        "nautilus", "tracker", "tracker3", "desktopdirectories", "sounds", "themes",
        "flatpak", "snap", "xorg", "session", "sessions", "thumbnails", "meshadercache",
        "mesashadercache", "evolution", "goa10", "gnomesession", "gnomeshell", "gnomesettingsdaemon",
        "kdedefaults", "plasmashellrc", "kwinrc", "kdeglobals", "akonadi", "baloo", "kactivitymanagerd",
        "wireplumber", "pipewire", "environmentd", "xdgdesktopportal", "ubuntuadvantage",
        "zeitgeist", "gnomecontrolcenter", "gnomeinitialsetupdone", "monitorsxml", "user",
        "recentlyused", "gstreamer10", "qtproject", "trolltech", "kconf_update", "kconfupdate",
        "networkmanager", "bluetooth", "containers", "dbus", "dbus1",
    ];
    // Sandbox containers named by UUID: their owner is only readable with Full Disk Access,
    // so they can never be attributed safely.
    if is_uuid(name) {
        return true;
    }
    let norm = normalize(name);
    // com.google.GoogleUpdater -> googleupdater
    let last = normalize(strip_known_ext(name).rsplit('.').next().unwrap_or(name));
    SYSTEM.iter().any(|s| norm == *s || (name.contains('.') && last == *s))
        || (cfg!(windows) && (norm.starts_with("microsoft") || norm.starts_with("windows")))
        || (cfg!(target_os = "linux") && (norm.starts_with("gnome") || norm.starts_with("kde") || norm.starts_with("plasma")))
}

fn is_uuid(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Data folders of command line tools whose binary has a different name.
const TOOL_ALIASES: &[(&str, &[&str])] = &[
    ("homebrew", &["brew"]),
    ("pypoetry", &["poetry"]),
    ("cocoapods", &["pod"]),
    ("msplaywright", &["playwright", "npx"]),
    ("nodegyp", &["node", "npm"]),
    ("typescript", &["tsc", "node", "npm"]),
    ("npm", &["npm"]),
    ("pip", &["pip3", "pip", "python3"]),
    ("pipenv", &["pipenv"]),
    ("gobuild", &["go"]),
    ("yarn", &["yarn", "corepack"]),
    ("pnpm", &["pnpm", "corepack"]),
    ("rustup", &["rustup", "cargo"]),
    ("jetbrains", &[]),
];

fn tool_installed(norm: &str) -> bool {
    TOOL_ALIASES
        .iter()
        .find(|(dir, _)| *dir == norm)
        .is_some_and(|(_, bins)| bins.iter().any(|b| crate::util::in_path(b)))
}

/// "com.docker.install" -> "com.docker" (vendor part of a reverse-DNS id).
fn rdns_vendor(id: &str) -> Option<&str> {
    let mut parts = id.splitn(3, '.');
    let (a, b) = (parts.next()?, parts.next()?);
    parts.next()?;
    Some(&id[..a.len() + 1 + b.len()])
}

/// Entries in the search roots that do not match any installed application.
pub fn find_orphans(installed: &[InstalledApp], ignore: &HashSet<PathBuf>) -> Vec<Found> {
    let mut tokens: HashSet<String> = installed.iter().flat_map(|a| a.tokens()).collect();
    tokens.extend(crate::apps::system_tokens());
    let rdns: Vec<String> = installed.iter().filter_map(|a| a.reverse_dns_id()).collect();
    let publishers: HashSet<String> = installed
        .iter()
        .filter_map(|a| a.publisher.as_deref())
        .map(|p| normalize(p.split([',', '(']).next().unwrap_or(p)))
        .filter(|p| p.len() >= 3)
        .collect();
    let locations: Vec<&PathBuf> = installed.iter().filter_map(|a| a.location.as_ref()).collect();
    let token_list: Vec<String> = tokens.iter().cloned().collect();
    // Same vendor as an installed app (com.docker.install for com.docker.docker).
    // Generic hosting prefixes are excluded: they say nothing about the vendor.
    let vendors: HashSet<String> = rdns
        .iter()
        .filter_map(|id| rdns_vendor(id))
        .filter(|v| !["com.github", "io.github", "com.todesktop", "org.electron", "com.electron"].contains(v))
        .map(|v| format!("{v}."))
        .collect();

    let mut out = Vec::new();
    for root in search_roots() {
        let label = root_label(&root);
        for child in children(&root) {
            if ignore.contains(&child) {
                continue;
            }
            let name = file_name(&child);
            if is_system_entry(&name) {
                continue;
            }
            if locations.iter().any(|l| l.starts_with(&child) || child.starts_with(l)) {
                continue;
            }
            let raw = strip_known_ext(&name);
            let norm = normalize(raw);
            if norm.len() < 2 {
                continue;
            }
            let lower = raw.to_lowercase();
            // "Docker Desktop" -> "docker"
            let first_word = normalize(raw.split([' ', '-', '_']).next().unwrap_or(raw));
            let owned = token_list.iter().any(|t| *t == norm)
                || (first_word.len() >= 4 && tokens.contains(&first_word))
                || vendors.iter().any(|v| lower.starts_with(v.as_str()) || lower.contains(&format!(".{v}")))
                || tool_installed(&norm)
                || rdns.iter().any(|id| matches_app(&name, &[], Some(id)))
                || publishers.iter().any(|p| *p == norm || (norm.len() >= 4 && p.starts_with(&norm)))
                // Reverse-DNS names whose last component is an installed app name.
                || raw.rsplit('.').next().is_some_and(|last| tokens.contains(&normalize(last)))
                // CLI tools keep config in the same places; keep them if the binary still exists.
                || crate::util::in_path(raw)
                || crate::util::in_path(&raw.to_lowercase());
            if !owned {
                out.push(Found { size: path_size(&child), path: child, location: label.clone() });
            }
        }
    }
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}
