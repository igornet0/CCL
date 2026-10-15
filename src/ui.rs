use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;

use crate::apps::{InstalledApp, Uninstaller};
use crate::cache::{CacheItem, Choice, Policy};
use crate::leftovers::Found;
use crate::util::format_size;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Apps,
    Leftovers,
    Cache,
}

struct Row<T> {
    item: T,
    checked: bool,
}

enum Msg {
    Apps(Vec<InstalledApp>),
    Related { key: String, app_size: u64, found: Vec<Found> },
    Orphans(Vec<Found>),
    Caches(Vec<CacheItem>),
    Log(String),
    Removed(Vec<PathBuf>),
    Finished(Option<Refresh>),
}

#[derive(Clone, Copy)]
enum Refresh {
    Apps,
    Caches,
}

enum Confirm {
    Uninstall { app: InstalledApp, paths: Vec<PathBuf> },
    DeleteData(Vec<PathBuf>),
    CleanCache(Vec<PathBuf>),
}

/// Sends messages from worker threads and wakes the UI up.
#[derive(Clone)]
struct Reporter {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Reporter {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
    fn log(&self, s: impl Into<String>) {
        self.send(Msg::Log(s.into()));
    }
}

pub struct CclApp {
    tab: Tab,
    use_trash: bool,
    reporter: Reporter,
    rx: Receiver<Msg>,
    busy: Option<String>,
    log: Vec<String>,
    /// A deletion was blocked by macOS privacy protection.
    need_disk_access: bool,
    confirm: Option<Confirm>,
    policy: Policy,

    apps: Vec<InstalledApp>,
    apps_loading: bool,
    app_filter: String,
    selected: Option<usize>,
    related: Vec<Row<Found>>,
    related_key: Option<String>,
    related_loading: bool,
    app_size: Option<u64>,

    orphans: Vec<Row<Found>>,
    orphans_loading: bool,
    orphans_scanned: bool,

    caches: Vec<(CacheItem, Choice)>,
    caches_loading: bool,
    caches_scanned: bool,
    cache_filter: String,
    cache_permanent: bool,
}

fn app_key(app: &InstalledApp) -> String {
    format!("{}|{:?}", app.name, app.location)
}

fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

fn display_path(p: &std::path::Path) -> String {
    let home = crate::util::home();
    match p.strip_prefix(&home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => p.display().to_string(),
    }
}

impl CclApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = channel();
        let mut app = Self {
            tab: Tab::Apps,
            use_trash: true,
            reporter: Reporter { tx, ctx: cc.egui_ctx.clone() },
            rx,
            busy: None,
            log: Vec::new(),
            need_disk_access: false,
            confirm: None,
            policy: Policy::load(),
            apps: Vec::new(),
            apps_loading: false,
            app_filter: String::new(),
            selected: None,
            related: Vec::new(),
            related_key: None,
            related_loading: false,
            app_size: None,
            orphans: Vec::new(),
            orphans_loading: false,
            orphans_scanned: false,
            caches: Vec::new(),
            caches_loading: false,
            caches_scanned: false,
            cache_filter: String::new(),
            cache_permanent: true,
        };
        app.load_apps();
        app
    }

    fn spawn(&self, job: impl FnOnce(&Reporter) + Send + 'static) {
        let r = self.reporter.clone();
        std::thread::spawn(move || job(&r));
    }

    fn load_apps(&mut self) {
        self.apps_loading = true;
        self.spawn(|r| r.send(Msg::Apps(crate::apps::list_installed())));
    }

    fn load_related(&mut self, app: InstalledApp) {
        let key = app_key(&app);
        self.related_key = Some(key.clone());
        self.related.clear();
        self.related_loading = true;
        self.app_size = None;
        self.spawn(move |r| {
            let app_size = app.location.as_deref().map(crate::util::path_size).unwrap_or(0);
            let found = crate::leftovers::related_to(&app);
            r.send(Msg::Related { key, app_size, found });
        });
    }

    fn scan_orphans(&mut self) {
        self.orphans_loading = true;
        self.orphans_scanned = true;
        let ignore: HashSet<PathBuf> = self.policy.ignore_leftovers.iter().cloned().collect();
        self.spawn(move |r| {
            let apps = crate::apps::list_installed();
            r.send(Msg::Orphans(crate::leftovers::find_orphans(&apps, &ignore)));
        });
    }

    fn scan_caches(&mut self) {
        self.caches_loading = true;
        self.caches_scanned = true;
        self.spawn(|r| r.send(Msg::Caches(crate::cache::scan())));
    }

    fn push_log(&mut self, s: String) {
        if s.contains(crate::ops::FULL_DISK_ACCESS_HINT) || (s.starts_with('⚠') && s.contains("нет прав")) {
            self.need_disk_access = cfg!(target_os = "macos");
        }
        self.log.push(s);
        if self.log.len() > 500 {
            self.log.drain(..self.log.len() - 500);
        }
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Apps(apps) => {
                    self.push_log(format!("Найдено программ: {}", apps.len()));
                    self.apps = apps;
                    self.apps_loading = false;
                    self.selected = None;
                    self.related_key = None;
                    self.related.clear();
                }
                Msg::Related { key, app_size, found } => {
                    if self.related_key.as_deref() == Some(key.as_str()) {
                        self.related = found.into_iter().map(|item| Row { item, checked: true }).collect();
                        self.app_size = Some(app_size);
                        self.related_loading = false;
                    }
                }
                Msg::Orphans(found) => {
                    self.push_log(format!("Найдено возможных остатков: {}", found.len()));
                    self.orphans = found.into_iter().map(|item| Row { item, checked: false }).collect();
                    self.orphans_loading = false;
                }
                Msg::Caches(items) => {
                    self.caches = items
                        .into_iter()
                        .map(|i| {
                            let c = self.policy.choice(&i.path);
                            (i, c)
                        })
                        .collect();
                    self.caches_loading = false;
                }
                Msg::Log(s) => self.push_log(s),
                Msg::Removed(paths) => {
                    let set: HashSet<PathBuf> = paths.into_iter().collect();
                    self.orphans.retain(|r| !set.contains(&r.item.path));
                    self.related.retain(|r| !set.contains(&r.item.path));
                }
                Msg::Finished(refresh) => {
                    self.busy = None;
                    match refresh {
                        Some(Refresh::Apps) => self.load_apps(),
                        Some(Refresh::Caches) => self.scan_caches(),
                        None => {}
                    }
                }
            }
        }
    }

    fn run_confirmed(&mut self, confirm: Confirm) {
        let permanent = !self.use_trash;
        match confirm {
            Confirm::Uninstall { app, paths } => {
                self.busy = Some(format!("Удаление {}…", app.name));
                self.spawn(move |r| {
                    if app.can_uninstall() {
                        match crate::ops::uninstall(&app, permanent) {
                            Ok(out) => r.log(format!("✔ {}: программа удалена {}", app.name, out)),
                            Err(e) => r.log(format!("✖ {}: {e}", app.name)),
                        }
                    }
                    let removed = delete_all(r, &paths, permanent);
                    r.send(Msg::Removed(removed));
                    r.send(Msg::Finished(Some(Refresh::Apps)));
                });
            }
            Confirm::DeleteData(paths) => {
                self.busy = Some("Удаление файлов…".into());
                self.spawn(move |r| {
                    let removed = delete_all(r, &paths, permanent);
                    r.send(Msg::Removed(removed));
                    r.send(Msg::Finished(None));
                });
            }
            Confirm::CleanCache(paths) => {
                let permanent = self.cache_permanent;
                self.busy = Some("Очистка кэша…".into());
                self.spawn(move |r| {
                    let mut failed_total = 0;
                    for p in &paths {
                        match crate::ops::empty_dir(p, permanent) {
                            Ok(0) => r.log(format!("✔ очищено: {}", display_path(p))),
                            Ok(n) => {
                                failed_total += n;
                                r.log(format!("⚠ {}: не удалось удалить {n} элементов (заняты или нет прав)", display_path(p)))
                            }
                            Err(e) => r.log(format!("✖ {}: {e}", display_path(p))),
                        }
                    }
                    r.log(format!("Очистка кэша завершена, пропущено элементов: {failed_total}"));
                    r.send(Msg::Finished(Some(Refresh::Caches)));
                });
            }
        }
    }

    // ---------------------------------------------------------------- tabs

    fn apps_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.app_filter).hint_text("Поиск программы…").desired_width(f32::INFINITY));
        });
        ui.horizontal(|ui| {
            ui.label(format!("Программ: {}", self.apps.len()));
            if self.apps_loading {
                ui.spinner();
            } else if ui.small_button("⟳ Обновить").clicked() {
                self.load_apps();
            }
        });
        ui.separator();
        let filter = self.app_filter.to_lowercase();
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, app) in self.apps.iter().enumerate() {
                if !filter.is_empty() && !app.name.to_lowercase().contains(&filter) {
                    continue;
                }
                let text = match &app.version {
                    Some(v) => format!("{}  ·  {v}", app.name),
                    None => app.name.clone(),
                };
                if ui.selectable_label(self.selected == Some(i), text).clicked() {
                    clicked = Some(i);
                }
            }
        });
        if let Some(i) = clicked {
            if self.selected != Some(i) {
                self.selected = Some(i);
                self.load_related(self.apps[i].clone());
            }
        }
    }

    fn app_details(&mut self, ui: &mut egui::Ui) {
        let Some(app) = self.selected.and_then(|i| self.apps.get(i)).cloned() else {
            ui.centered_and_justified(|ui| ui.label("Выберите программу в списке слева"));
            return;
        };
        ui.heading(&app.name);
        egui::Grid::new("app_info").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(egui::RichText::new(k).weak());
                ui.label(v);
                ui.end_row();
            };
            if let Some(v) = &app.version {
                row("Версия", v.clone());
            }
            if let Some(p) = &app.publisher {
                row("Издатель", p.clone());
            }
            if let Some(id) = &app.id {
                row("Идентификатор", id.clone());
            }
            if let Some(l) = &app.location {
                row("Расположение", display_path(l));
            }
            row("Источник", app.source.to_string());
            row(
                "Размер программы",
                self.app_size.map(format_size).unwrap_or_else(|| "…".into()),
            );
            let how = match &app.uninstaller {
                Uninstaller::RemovePath(_) => "удаление папки/файла программы".to_string(),
                Uninstaller::Command(c) => format!("штатный деинсталлятор: {c}"),
                Uninstaller::Flatpak(id) => format!("flatpak uninstall {id}"),
                Uninstaller::Snap(n) => format!("snap remove {n}"),
                Uninstaller::Package(p) => format!("пакетный менеджер, пакет {p}"),
                Uninstaller::None => "нет (можно удалить только данные)".to_string(),
            };
            row("Способ удаления", how);
        });

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Папки и файлы, созданные программой");
            if self.related_loading {
                ui.spinner();
            }
        });

        let checked: Vec<PathBuf> = self.related.iter().filter(|r| r.checked).map(|r| r.item.path.clone()).collect();
        let checked_size: u64 = self.related.iter().filter(|r| r.checked).map(|r| r.item.size).sum();
        let idle = self.busy.is_none();
        ui.horizontal(|ui| {
            let label = format!(
                "🗑 Удалить программу{}",
                if checked.is_empty() { String::new() } else { format!(" и {} объект(ов) данных", checked.len()) }
            );
            if ui.add_enabled(idle && app.can_uninstall(), egui::Button::new(label)).clicked() {
                self.confirm = Some(Confirm::Uninstall { app: app.clone(), paths: checked.clone() });
            }
            if ui
                .add_enabled(idle && !checked.is_empty(), egui::Button::new("Удалить только данные"))
                .clicked()
            {
                self.confirm = Some(Confirm::DeleteData(checked.clone()));
            }
            ui.label(format!("Выбрано: {}", format_size(checked_size)));
        });
        ui.add_space(4.0);
        if !self.related_loading && self.related.is_empty() {
            ui.label(egui::RichText::new("Связанных данных не найдено").weak());
        }
        found_table(ui, "related", &mut self.related, None);
    }

    fn leftovers_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Остатки удалённых программ");
        ui.label(
            egui::RichText::new(
                "Папки и файлы в каталогах данных, которым не соответствует ни одна установленная программа. \
                 Поиск эвристический — проверьте список перед удалением.",
            )
            .weak(),
        );
        let idle = self.busy.is_none();
        let checked: Vec<PathBuf> = self.orphans.iter().filter(|r| r.checked).map(|r| r.item.path.clone()).collect();
        let checked_size: u64 = self.orphans.iter().filter(|r| r.checked).map(|r| r.item.size).sum();
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.orphans_loading, egui::Button::new("🔍 Сканировать")).clicked() {
                self.scan_orphans();
            }
            if self.orphans_loading {
                ui.spinner();
            }
            if ui.button("Выбрать все").clicked() {
                self.orphans.iter_mut().for_each(|r| r.checked = true);
            }
            if ui.button("Снять выбор").clicked() {
                self.orphans.iter_mut().for_each(|r| r.checked = false);
            }
            if ui
                .add_enabled(!checked.is_empty(), egui::Button::new("Это не остаток"))
                .on_hover_text("Больше не показывать выбранные объекты")
                .clicked()
            {
                for p in &checked {
                    self.policy.ignore_leftovers.insert(p.clone());
                }
                self.orphans.retain(|r| !r.checked);
                self.save_policy();
            }
            if ui
                .add_enabled(idle && !checked.is_empty(), egui::Button::new(format!("🗑 Удалить выбранные ({})", format_size(checked_size))))
                .clicked()
            {
                self.confirm = Some(Confirm::DeleteData(checked.clone()));
            }
        });
        ui.separator();
        if self.orphans_scanned && !self.orphans_loading && self.orphans.is_empty() {
            ui.label("Ничего не найдено 🎉");
        }
        let total: u64 = self.orphans.iter().map(|r| r.item.size).sum();
        if !self.orphans.is_empty() {
            ui.label(format!("Найдено: {} объектов, {}", self.orphans.len(), format_size(total)));
        }
        found_table(ui, "orphans", &mut self.orphans, Some(()));
    }

    fn save_policy(&mut self) {
        if let Err(e) = self.policy.save() {
            self.push_log(format!("✖ не удалось сохранить настройки: {e}"));
        }
    }

    fn cache_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Очистка кэша");
        ui.label(
            egui::RichText::new(
                "«Оставить» — защитить и запомнить; «Удалить» — очищать и запомнить выбор на будущее. \
                 Очищается содержимое папки, сама папка остаётся.",
            )
            .weak(),
        );
        let idle = self.busy.is_none();
        let to_delete: Vec<PathBuf> = self
            .caches
            .iter()
            .filter(|(_, c)| *c == Choice::Delete)
            .map(|(i, _)| i.path.clone())
            .collect();
        let delete_size: u64 = self.caches.iter().filter(|(_, c)| *c == Choice::Delete).map(|(i, _)| i.size).sum();
        let total: u64 = self.caches.iter().map(|(i, _)| i.size).sum();
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.caches_loading, egui::Button::new("⟳ Сканировать")).clicked() {
                self.scan_caches();
            }
            if self.caches_loading {
                ui.spinner();
            }
            if ui.button("Отметить все (кроме «Оставить»)").clicked() {
                for (item, c) in self.caches.iter_mut() {
                    if *c != Choice::Keep {
                        *c = Choice::Delete;
                        self.policy.set(&item.path, Choice::Delete);
                    }
                }
                self.save_policy();
            }
            if ui.button("Сбросить отметки").clicked() {
                for (item, c) in self.caches.iter_mut() {
                    if *c == Choice::Delete {
                        *c = Choice::Skip;
                        self.policy.set(&item.path, Choice::Skip);
                    }
                }
                self.save_policy();
            }
            ui.checkbox(&mut self.cache_permanent, "Безвозвратно");
            if ui
                .add_enabled(
                    idle && !to_delete.is_empty(),
                    egui::Button::new(format!("🧹 Очистить отмеченные ({})", format_size(delete_size))),
                )
                .clicked()
            {
                self.confirm = Some(Confirm::CleanCache(to_delete.clone()));
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.cache_filter).hint_text("Фильтр…").desired_width(260.0));
            ui.label(format!("Всего кэша: {}", format_size(total)));
        });
        ui.separator();

        let filter = self.cache_filter.to_lowercase();
        let mut changed = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut groups: Vec<String> = self.caches.iter().map(|(i, _)| i.group.clone()).collect();
            groups.dedup();
            for group in groups {
                let group_size: u64 = self.caches.iter().filter(|(i, _)| i.group == group).map(|(i, _)| i.size).sum();
                egui::CollapsingHeader::new(format!("{group} — {}", format_size(group_size)))
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::Grid::new(("cache", group.as_str())).striped(true).num_columns(4).show(ui, |ui| {
                            for (item, choice) in self.caches.iter_mut().filter(|(i, _)| i.group == group) {
                                if !filter.is_empty() && !item.name.to_lowercase().contains(&filter) {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    let before = *choice;
                                    ui.selectable_value(choice, Choice::Keep, "🔒 Оставить");
                                    ui.selectable_value(choice, Choice::Skip, "—");
                                    ui.selectable_value(choice, Choice::Delete, "🗑 Удалить");
                                    if *choice != before {
                                        self.policy.set(&item.path, *choice);
                                        changed = true;
                                    }
                                });
                                ui.label(egui::RichText::new(format_size(item.size)).monospace());
                                let name = ui.label(&item.name).on_hover_text(item.path.display().to_string());
                                let _ = name;
                                if ui.small_button("📂").on_hover_text("Показать в файловом менеджере").clicked() {
                                    reveal(&item.path);
                                }
                                ui.end_row();
                            }
                        });
                    });
            }
        });
        if changed {
            self.save_policy();
        }
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else { return };
        let (title, paths, extra) = match confirm {
            Confirm::Uninstall { app, paths } => (
                format!("Удалить «{}»?", app.name),
                paths.clone(),
                match &app.uninstaller {
                    Uninstaller::Command(_) => "Будет запущен штатный деинсталлятор программы.",
                    Uninstaller::Package(_) | Uninstaller::Snap(_) => "Потребуется пароль администратора.",
                    _ => "",
                },
            ),
            Confirm::DeleteData(paths) => (format!("Удалить {} объект(ов)?", paths.len()), paths.clone(), ""),
            Confirm::CleanCache(paths) => (format!("Очистить {} папок кэша?", paths.len()), paths.clone(), ""),
        };
        let is_cache = matches!(confirm, Confirm::CleanCache(_));
        let mode = if is_cache {
            if self.cache_permanent { "Файлы будут удалены безвозвратно." } else { "Файлы будут перемещены в корзину." }
        } else if self.use_trash {
            "Файлы будут перемещены в корзину."
        } else {
            "Файлы будут удалены безвозвратно!"
        };

        let mut decision = None;
        let resp = egui::Modal::new(egui::Id::unique("ccl_confirm")).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.heading(title);
            if !extra.is_empty() {
                ui.label(extra);
            }
            ui.label(mode);
            if !paths.is_empty() {
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for p in &paths {
                        ui.label(egui::RichText::new(display_path(p)).monospace().small());
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Да, удалить").clicked() {
                    decision = Some(true);
                }
                if ui.button("Отмена").clicked() {
                    decision = Some(false);
                }
            });
        });
        if resp.should_close() && decision.is_none() {
            decision = Some(false);
        }
        match decision {
            Some(true) => {
                let c = self.confirm.take().unwrap();
                self.run_confirmed(c);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

fn delete_all(r: &Reporter, paths: &[PathBuf], permanent: bool) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for p in paths {
        match crate::ops::remove(p, permanent) {
            Ok(()) => {
                r.log(format!("✔ удалено: {}", display_path(p)));
                removed.push(p.clone());
            }
            Err(e) => r.log(format!("✖ {}: {e}", display_path(p))),
        }
    }
    removed
}

fn found_table(ui: &mut egui::Ui, id: &str, rows: &mut [Row<Found>], show_location: Option<()>) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new(id).striped(true).num_columns(4).show(ui, |ui| {
            for row in rows.iter_mut() {
                ui.checkbox(&mut row.checked, "");
                ui.label(egui::RichText::new(format_size(row.item.size)).monospace());
                let name = row.item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.vertical(|ui| {
                    ui.label(name);
                    let loc = if show_location.is_some() { row.item.location.clone() } else { display_path(&row.item.path) };
                    ui.label(egui::RichText::new(loc).weak().small());
                })
                .response
                .on_hover_text(row.item.path.display().to_string());
                if ui.small_button("📂").on_hover_text("Показать в файловом менеджере").clicked() {
                    reveal(&row.item.path);
                }
                ui.end_row();
            }
        });
    });
}

impl eframe::App for CclApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("CCL");
                ui.separator();
                let prev = self.tab;
                ui.selectable_value(&mut self.tab, Tab::Apps, "📦 Программы");
                ui.selectable_value(&mut self.tab, Tab::Leftovers, "🔍 Остатки");
                ui.selectable_value(&mut self.tab, Tab::Cache, "🧹 Кэш");
                if prev != self.tab {
                    if self.tab == Tab::Leftovers && !self.orphans_scanned {
                        self.scan_orphans();
                    }
                    if self.tab == Tab::Cache && !self.caches_scanned {
                        self.scan_caches();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.use_trash, "Удалять в корзину");
                    if let Some(b) = &self.busy {
                        ui.label(b);
                        ui.spinner();
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("log").resizable(true).default_size(110.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Журнал");
                if ui.small_button("Очистить").clicked() {
                    self.log.clear();
                }
                #[cfg(target_os = "macos")]
                if self.need_disk_access {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(220, 150, 40), "⚠ macOS не дал доступ к части файлов");
                    if ui
                        .button("Открыть «Полный доступ к диску»")
                        .on_hover_text("Добавьте CCL или Терминал, из которого он запущен, затем перезапустите CCL")
                        .clicked()
                    {
                        crate::ops::open_full_disk_access_settings();
                    }
                }
            });
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in &self.log {
                    ui.label(egui::RichText::new(line).small().monospace());
                }
            });
        });

        if self.tab == Tab::Apps {
            egui::Panel::left("apps_list").resizable(true).default_size(320.0).show(ui, |ui| {
                self.apps_list(ui);
            });
        }

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Apps => self.app_details(ui),
            Tab::Leftovers => self.leftovers_tab(ui),
            Tab::Cache => self.cache_tab(ui),
        });

        self.confirm_modal(&ctx);
    }
}
