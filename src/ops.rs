//! Destructive operations: deleting files and running uninstallers.

use std::path::Path;
use std::process::Command;

use crate::apps::{InstalledApp, Uninstaller};
use crate::util::is_safe_to_delete;

/// Move to trash, or delete permanently.
pub fn remove(path: &Path, permanent: bool) -> Result<(), String> {
    if !is_safe_to_delete(path) {
        return Err("путь защищён от удаления".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if permanent {
        if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
        .map_err(|e| explain(e.to_string()))
    } else {
        move_to_trash(path).map_err(explain)
    }
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    // The default Finder/AppleScript method runs with Finder's permissions, and
    // Finder is not allowed into other apps' containers (~/Library/Containers).
    // NSFileManager runs in our own process, so it gets our Full Disk Access.
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

/// Message shown when macOS privacy protection (TCC) denies access.
pub const FULL_DISK_ACCESS_HINT: &str = "нет доступа: macOS защищает эту папку (данные других приложений). \
Права root/sudo это не обходят — выдайте CCL (или Терминалу, из которого он запущен) \
«Полный доступ к диску» в Системные настройки → Конфиденциальность и безопасность и перезапустите CCL";

pub fn is_permission_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "operation not permitted",
        "permission denied",
        "os error 1)",
        "os error 13)",
        "(-5000)",
        "нет необходимых прав",
        "отказано в доступе",
        "access is denied",
        "not allowed",
    ]
    .iter()
    .any(|p| m.contains(p))
}

fn explain(msg: String) -> String {
    if cfg!(target_os = "macos") && is_permission_error(&msg) {
        FULL_DISK_ACCESS_HINT.to_string()
    } else {
        msg
    }
}

/// Open the system settings page where Full Disk Access is granted.
#[cfg(target_os = "macos")]
pub fn open_full_disk_access_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn();
}

/// Delete everything inside a directory but keep the directory itself.
/// Returns the number of entries that could not be removed.
pub fn empty_dir(path: &Path, permanent: bool) -> Result<usize, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return remove(path, permanent).map(|_| 0);
    }
    let mut failed = 0;
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())?.flatten() {
        if remove(&entry.path(), permanent).is_err() {
            failed += 1;
        }
    }
    Ok(failed)
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("не удалось запустить: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(format!("код {}: {}", out.status, text.trim()))
    }
}

/// Uninstall the application itself (not its data).
pub fn uninstall(app: &InstalledApp, permanent: bool) -> Result<String, String> {
    match &app.uninstaller {
        Uninstaller::RemovePath(p) => remove(p, permanent).map(|_| format!("удалено: {}", p.display())),
        Uninstaller::Command(cmd) => run_command_line(cmd),
        Uninstaller::Flatpak(id) => run(Command::new("flatpak").args(["uninstall", "-y", "--noninteractive", id])),
        Uninstaller::Snap(name) => run(Command::new("pkexec").args(["snap", "remove", name])),
        Uninstaller::Package(pkg) => remove_package(pkg),
        Uninstaller::None => Err("для этой программы нет известного способа удаления".into()),
    }
}

#[cfg(windows)]
fn run_command_line(cmd: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    // UninstallString is a full command line; let cmd.exe parse its quoting.
    let mut c = Command::new("cmd");
    c.raw_arg("/C").raw_arg(cmd);
    run(&mut c)
}

#[cfg(not(windows))]
fn run_command_line(cmd: &str) -> Result<String, String> {
    run(Command::new("sh").args(["-c", cmd]))
}

fn remove_package(pkg: &str) -> Result<String, String> {
    use crate::util::in_path;
    let args: Vec<&str> = if in_path("apt-get") {
        vec!["apt-get", "remove", "-y", pkg]
    } else if in_path("dnf") {
        vec!["dnf", "remove", "-y", pkg]
    } else if in_path("zypper") {
        vec!["zypper", "--non-interactive", "remove", pkg]
    } else if in_path("pacman") {
        vec!["pacman", "-R", "--noconfirm", pkg]
    } else if in_path("rpm") {
        vec!["rpm", "-e", pkg]
    } else {
        return Err("не найден пакетный менеджер".into());
    };
    // pkexec shows the graphical password prompt.
    run(Command::new("pkexec").args(args))
}
