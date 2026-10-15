#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod cache;
mod leftovers;
mod ops;
mod ui;
mod util;

use eframe::egui;

fn load_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CCL — удаление программ и очистка")
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([800.0, 500.0]);
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("CCL", options, Box::new(|cc| Ok(Box::new(ui::CclApp::new(cc)))))
}

#[cfg(test)]
mod smoke {
    #[test]
    #[ignore]
    fn scan_real_system() {
        let apps = crate::apps::list_installed();
        println!("apps: {}", apps.len());
        for a in apps.iter().filter(|a| ["Visual Studio Code", "Google Chrome", "Telegram", "Slack"].iter().any(|n| a.name.contains(n))) {
            println!("== {} {:?}", a.name, a.id);
            for f in crate::leftovers::related_to(a) {
                println!("   {} {}", crate::util::format_size(f.size), f.path.display());
            }
        }
        let orphans = crate::leftovers::find_orphans(&apps, &Default::default());
        println!("orphans: {}", orphans.len());
        for f in orphans.iter().take(40) {
            println!("   {} {}", crate::util::format_size(f.size), f.path.display());
        }
        let caches = crate::cache::scan();
        println!("caches: {} total {}", caches.len(), crate::util::format_size(caches.iter().map(|c| c.size).sum()));
        for c in caches.iter().take(10) {
            println!("   [{}] {} {}", c.group, c.name, crate::util::format_size(c.size));
        }
    }
}
