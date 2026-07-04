//! Application shell: panels, tabs, global search, import wiring.

use crate::settings::{self, Settings};
use crate::tabs::{BoardTab, CenterRequest};
use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;
use egui::{Color32, RichText};
use fbv_data::{cache::BlobCache, BoardRow, Database, QuarantineRow};
use fbv_index::{ImportEvent, Keys};
use fbv_search::{HitKind, SearchHit};
use std::path::PathBuf;
use std::sync::Arc;

const CONDITIONS: &[&str] = &["unknown", "working", "donor", "stripped"];

#[derive(PartialEq, Clone, Copy)]
enum LeftTab {
    Library,
    Search,
    Problems,
}

pub struct App {
    settings: Settings,
    db: Database,
    cache: Arc<BlobCache>,

    boards: Vec<BoardRow>,
    boards_dirty: bool,
    lib_filter: String,
    quarantine: Vec<QuarantineRow>,

    left_tab: LeftTab,
    global_query: String,
    global_hits: Vec<SearchHit>,
    focus_global: bool,

    tabs: Vec<BoardTab>,
    active: usize,

    import_rx: Option<Receiver<ImportEvent>>,
    import_cancel: Option<Sender<()>>,
    import_total: usize,
    import_done: usize,
    import_last: String,

    settings_open: bool,
    error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let data_dir = settings::data_dir();
        let db = Database::open(&data_dir.join("library.db"))?;
        let cache = Arc::new(BlobCache::new(data_dir.join("cache"))?);
        let settings = Settings::load();

        let _ = fbv_index::reconcile_missing(&db);

        Ok(Self {
            settings,
            db,
            cache,
            boards: Vec::new(),
            boards_dirty: true,
            lib_filter: String::new(),
            quarantine: Vec::new(),
            left_tab: LeftTab::Library,
            global_query: String::new(),
            global_hits: Vec::new(),
            focus_global: false,
            tabs: Vec::new(),
            active: 0,
            import_rx: None,
            import_cancel: None,
            import_total: 0,
            import_done: 0,
            import_last: String::new(),
            settings_open: false,
            error: None,
        })
    }

    fn refresh_boards(&mut self) {
        if let Ok(rows) = self.db.list_boards() {
            self.boards = rows;
        }
        if let Ok(rows) = self.db.list_quarantine() {
            self.quarantine = rows;
        }
        self.boards_dirty = false;
    }

    fn run_global_search(&mut self) {
        match fbv_search::search_library(&self.db, &self.global_query, 500) {
            Ok(hits) => self.global_hits = hits,
            Err(e) => self.error = Some(format!("search failed: {e}")),
        }
    }

    fn start_import(&mut self, roots: Vec<PathBuf>) {
        if roots.is_empty() || self.import_rx.is_some() {
            return;
        }
        let (ev_tx, ev_rx) = unbounded();
        let (cancel_tx, cancel_rx) = unbounded();
        let keys: Keys = self.settings.keys();
        let cache = Arc::clone(&self.cache);
        let db_path = settings::data_dir().join("library.db");
        std::thread::spawn(move || {
            let mut db = match Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    let _ = ev_tx.send(ImportEvent::Failed {
                        path: db_path,
                        reason: format!("cannot open library db: {e}"),
                    });
                    let _ = ev_tx.send(ImportEvent::Finished {
                        imported: 0,
                        failed: 1,
                        known: 0,
                        skipped: 0,
                    });
                    return;
                }
            };
            let _ = fbv_index::run_import(&mut db, &cache, &roots, keys, &ev_tx, &cancel_rx);
        });
        self.import_rx = Some(ev_rx);
        self.import_cancel = Some(cancel_tx);
        self.import_total = 0;
        self.import_done = 0;
    }

    fn poll_import(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.import_rx else {
            return;
        };
        let mut finished = false;
        for ev in rx.try_iter() {
            match ev {
                ImportEvent::Discovered(n) => self.import_total = n,
                ImportEvent::Imported { path, .. } => {
                    self.import_done += 1;
                    self.import_last = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    self.boards_dirty = true;
                }
                ImportEvent::AlreadyKnown { .. } | ImportEvent::Skipped { .. } => {
                    self.import_done += 1;
                }
                ImportEvent::Failed { path, .. } => {
                    self.import_done += 1;
                    self.import_last = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    self.boards_dirty = true;
                }
                ImportEvent::Finished { .. } => {
                    finished = true;
                    self.boards_dirty = true;
                }
            }
        }
        if finished {
            self.import_rx = None;
            self.import_cancel = None;
            if !self.global_query.is_empty() {
                self.run_global_search();
            }
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Opens (or activates) a board tab; returns its index.
    fn open_board(&mut self, board_id: i64) -> Option<usize> {
        if let Some(i) = self.tabs.iter().position(|t| t.board_id == board_id) {
            self.active = i;
            return Some(i);
        }
        let row = match self.db.board_row(board_id) {
            Ok(Some(r)) => r,
            _ => {
                self.error = Some("board not found in library".into());
                return None;
            }
        };
        let model = match self.load_model(&row) {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(format!("cannot open {}: {e}", row.display_name));
                return None;
            }
        };
        let title = row
            .oem_code
            .clone()
            .unwrap_or_else(|| row.display_name.clone());
        let mut tab = BoardTab::new(board_id, title, Arc::new(model));
        if let Ok(list) = self.db.harvested_parts(board_id) {
            tab.harvested = list.into_iter().collect();
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        Some(self.active)
    }

    fn load_model(&self, row: &BoardRow) -> anyhow::Result<fbv_core::BoardModel> {
        if let Some(model) = self.cache.load(&row.sha256)? {
            return Ok(model);
        }
        // Cache miss: re-parse from the source file.
        let path = row
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no file path recorded"))?;
        let path = PathBuf::from(path);
        let bytes = std::fs::read(&path)?;
        let dir = path.parent().map(|p| p.to_path_buf());
        let companion = move |name: &str| -> Option<Vec<u8>> {
            let dir = dir.as_ref()?;
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                if e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
                    return std::fs::read(e.path()).ok();
                }
            }
            None
        };
        let ctx = fbv_parsers::ParseContext {
            fz_key: self.settings.fz_key(),
            xzz_key: self.settings.xzz_key(),
            companion: Some(&companion),
        };
        let (_fmt, model) = fbv_parsers::detect_and_parse(&bytes, Some(&path), &ctx)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let _ = self.cache.store(&row.sha256, &model);
        Ok(model)
    }

    fn open_search_hit(&mut self, hit: &SearchHit) {
        if self.open_board(hit.board_id).is_none() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        tab.center_request = Some(match hit.kind {
            HitKind::Part => CenterRequest::Part(hit.entity_idx as usize),
            HitKind::Net => CenterRequest::Net(hit.entity_idx as usize),
        });
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (space, m, r, f, x, esc, ctrl_f, ctrl_shift_f) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::M),
                i.key_pressed(egui::Key::R),
                i.key_pressed(egui::Key::F) && !i.modifiers.ctrl,
                i.key_pressed(egui::Key::X),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::F),
                i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::F),
            )
        });
        if ctrl_shift_f {
            self.left_tab = LeftTab::Search;
            self.focus_global = true;
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if space {
                tab.view.flip_side();
            }
            if m {
                tab.view.toggle_mirror();
            }
            if r {
                tab.view.rotate_ccw();
            }
            if f {
                tab.view.fit_pending = true;
            }
            if x {
                tab.expansion = (tab.expansion + 1) % 4;
            }
            if esc {
                tab.selected_part = None;
                tab.selected_pin = None;
                tab.selected_net = None;
            }
            if ctrl_f {
                tab.focus_search = true;
            }
        }
    }

    // ---------- panels ----------

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("FlexibleBoardViewer").size(16.0));
                ui.separator();
                if ui.button("Add folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        if !self.settings.library_roots.contains(&dir) {
                            self.settings.library_roots.push(dir.clone());
                            self.settings.save();
                        }
                        self.start_import(vec![dir]);
                    }
                }
                if ui.button("Open file…").clicked() {
                    if let Some(file) = rfd::FileDialog::new().pick_file() {
                        self.start_import(vec![file]);
                    }
                }
                let rescan = ui.add_enabled(
                    self.import_rx.is_none() && !self.settings.library_roots.is_empty(),
                    egui::Button::new("Rescan library"),
                );
                if rescan.clicked() {
                    self.start_import(self.settings.library_roots.clone());
                }
                if ui.button("Settings").clicked() {
                    self.settings_open = !self.settings_open;
                }

                ui.separator();
                ui.label("Search all boards:");
                let resp = ui.add_sized(
                    [280.0, 20.0],
                    egui::TextEdit::singleline(&mut self.global_query)
                        .hint_text("U5300, PPBUS_G3H, pn:TPS51225, net:PP3V3*"),
                );
                if self.focus_global {
                    resp.request_focus();
                    self.focus_global = false;
                }
                if resp.changed() {
                    self.left_tab = LeftTab::Search;
                    self.run_global_search();
                }
            });
        });
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left")
            .resizable(true)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.left_tab, LeftTab::Library, "Library");
                    ui.selectable_value(&mut self.left_tab, LeftTab::Search, "Search results");
                    let label = if self.quarantine.is_empty() {
                        "Problems".to_string()
                    } else {
                        format!("Problems ({})", self.quarantine.len())
                    };
                    ui.selectable_value(&mut self.left_tab, LeftTab::Problems, label);
                });
                ui.separator();
                match self.left_tab {
                    LeftTab::Library => self.library_ui(ui),
                    LeftTab::Search => self.search_ui(ui),
                    LeftTab::Problems => self.problems_ui(ui),
                }
            });
    }

    fn library_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.text_edit_singleline(&mut self.lib_filter);
        });
        ui.add_space(4.0);
        let filter = self.lib_filter.to_lowercase();
        let mut open_request = None;
        let mut condition_change: Option<(i64, String)> = None;
        let mut favorite_change: Option<(i64, bool)> = None;

        egui::ScrollArea::vertical()
            .id_salt("library_scroll")
            .show(ui, |ui| {
                for row in &self.boards {
                    if !filter.is_empty() {
                        let hay = format!(
                            "{} {} {}",
                            row.display_name.to_lowercase(),
                            row.oem_code.clone().unwrap_or_default().to_lowercase(),
                            row.format.to_lowercase()
                        );
                        if !hay.contains(&filter) {
                            continue;
                        }
                    }
                    ui.push_id(row.board_id, |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let star = if row.favorite { "★" } else { "☆" };
                                if ui.button(star).on_hover_text("favorite").clicked() {
                                    favorite_change = Some((row.board_id, !row.favorite));
                                }
                                let title = ui.selectable_label(
                                    false,
                                    RichText::new(&row.display_name).strong(),
                                );
                                if title.clicked() {
                                    open_request = Some(row.board_id);
                                }
                                if row.missing {
                                    ui.label(
                                        RichText::new("missing").color(Color32::from_rgb(220, 90, 60)),
                                    );
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} parts · {} nets",
                                        row.format, row.part_count, row.net_count
                                    ))
                                    .weak()
                                    .size(11.0),
                                );
                                let mut cond = row.condition.clone();
                                egui::ComboBox::from_id_salt(("cond", row.board_id))
                                    .selected_text(&cond)
                                    .width(90.0)
                                    .show_ui(ui, |ui| {
                                        for c in CONDITIONS {
                                            ui.selectable_value(&mut cond, c.to_string(), *c);
                                        }
                                    });
                                if cond != row.condition {
                                    condition_change = Some((row.board_id, cond));
                                }
                            });
                        });
                    });
                }
                if self.boards.is_empty() {
                    ui.add_space(20.0);
                    ui.label("No boards yet. Use 'Add folder…' to import your boardview files.");
                }
            });

        if let Some((id, cond)) = condition_change {
            let _ = self.db.set_condition(id, &cond);
            self.boards_dirty = true;
        }
        if let Some((id, fav)) = favorite_change {
            let _ = self.db.set_favorite(id, fav);
            self.boards_dirty = true;
        }
        if let Some(id) = open_request {
            self.open_board(id);
        }
    }

    fn search_ui(&mut self, ui: &mut egui::Ui) {
        if self.global_query.trim().is_empty() {
            ui.label("Type in the global search box above.");
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Syntax: bare terms match everything; ref:U5300, net:PPBUS*, \
                     pn:TPS51225, val:1uF, pkg:0402, board:820-00281 narrow by field.",
                )
                .weak(),
            );
            return;
        }
        ui.label(format!("{} hits", self.global_hits.len()));
        ui.separator();
        let mut clicked: Option<SearchHit> = None;
        egui::ScrollArea::vertical()
            .id_salt("search_scroll")
            .show(ui, |ui| {
                for (i, hit) in self.global_hits.iter().enumerate() {
                    let kind = match hit.kind {
                        HitKind::Part => "part",
                        HitKind::Net => "net",
                    };
                    let mut line1 = format!("{} · {}", hit.label, kind);
                    if let Some(pn) = &hit.part_number {
                        if !pn.is_empty() {
                            line1.push_str(&format!(" · {pn}"));
                        }
                    }
                    if let Some(v) = &hit.value {
                        if !v.is_empty() {
                            line1.push_str(&format!(" · {v}"));
                        }
                    }
                    if hit.kind == HitKind::Net {
                        line1.push_str(&format!(" · {} pins", hit.pin_count));
                    }
                    if hit.harvested {
                        line1.push_str(" · HARVESTED");
                    }
                    let mut line2 = hit.board_name.clone();
                    if let Some(code) = &hit.oem_code {
                        if !code.is_empty() && !hit.board_name.contains(code.as_str()) {
                            line2.push_str(&format!(" [{code}]"));
                        }
                    }
                    if hit.condition != "unknown" {
                        line2.push_str(&format!(" · {}", hit.condition));
                    }
                    ui.push_id(i, |ui| {
                        let resp = ui.selectable_label(
                            false,
                            format!("{line1}\n    {line2}"),
                        );
                        if resp.clicked() {
                            clicked = Some(hit.clone());
                        }
                    });
                }
            });
        if let Some(hit) = clicked {
            self.open_search_hit(&hit);
        }
    }

    fn problems_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Files that could not be indexed:");
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("quarantine_scroll")
            .show(ui, |ui| {
                for q in &self.quarantine {
                    ui.label(RichText::new(&q.path).size(11.0));
                    ui.label(
                        RichText::new(format!("  {}", q.reason))
                            .weak()
                            .color(Color32::from_rgb(220, 140, 60)),
                    );
                    ui.add_space(4.0);
                }
                if self.quarantine.is_empty() {
                    ui.label(RichText::new("none — all files parsed").weak());
                }
            });
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            return;
        }
        let mut harvested_change: Option<(i64, i64, bool)> = None;
        egui::SidePanel::right("right")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                ui.heading("Component");
                ui.separator();
                if let Some(idx) = tab.selected_part {
                    let part = tab.model.parts[idx].clone();
                    ui.label(RichText::new(&part.refdes).strong().size(15.0));
                    ui.label(format!("side: {}", part.side.label()));
                    if let Some(pn) = &part.mfg_code {
                        ui.label(format!("part number: {pn}"));
                    }
                    if let Some(v) = &part.value {
                        ui.label(format!("value: {v}"));
                    }
                    if let Some(p) = &part.package {
                        ui.label(format!("package: {p}"));
                    }
                    if let Some((min, max)) = tab.model.part_bounds(idx) {
                        ui.label(format!(
                            "position: {:.0}, {:.0} mil",
                            (min.x + max.x) / 2.0,
                            (min.y + max.y) / 2.0
                        ));
                    }
                    ui.label(format!("pins: {}", part.pins.len()));
                    if !part.is_dummy {
                        let mut h = tab.harvested.contains(&(idx as i64));
                        if ui.checkbox(&mut h, "harvested from this board").changed() {
                            harvested_change = Some((tab.board_id, idx as i64, h));
                            if h {
                                tab.harvested.insert(idx as i64);
                            } else {
                                tab.harvested.remove(&(idx as i64));
                            }
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("Pins").strong());
                    let mut select_pin_net: Option<(usize, fbv_core::NetId)> = None;
                    egui::ScrollArea::vertical()
                        .id_salt("pins_scroll")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for &pi in &part.pins {
                                let pin = &tab.model.pins[pi as usize];
                                let net_name = if pin.net == fbv_core::NO_NET {
                                    "(nc)".to_string()
                                } else {
                                    tab.model.nets[pin.net as usize].name.clone()
                                };
                                let label = if pin.number.is_empty() {
                                    net_name.clone()
                                } else {
                                    format!("{} → {}", pin.number, net_name)
                                };
                                let selected = tab.selected_net.is_some()
                                    && tab.selected_net == Some(pin.net);
                                if ui.selectable_label(selected, label).clicked()
                                    && pin.net != fbv_core::NO_NET
                                {
                                    select_pin_net = Some((pi as usize, pin.net));
                                }
                            }
                        });
                    if let Some((pi, n)) = select_pin_net {
                        // The clicked pin becomes the netweb origin.
                        tab.selected_pin = Some(pi);
                        tab.select_net(n);
                    }
                } else {
                    ui.label(RichText::new("click a pin on the board").weak());
                }

                ui.add_space(8.0);
                ui.heading("Net");
                ui.separator();
                if let Some(net) = tab.selected_net {
                    let name = tab.model.nets[net as usize].name.clone();
                    let count = tab.model.nets[net as usize].pins.len();
                    ui.label(RichText::new(&name).strong().color(Color32::from_rgb(255, 220, 60)));
                    ui.label(format!("{count} pins on this board"));
                    ui.horizontal(|ui| {
                        ui.label("expand through jumpers:");
                        ui.add(egui::Slider::new(&mut tab.expansion, 0..=3).text("levels"));
                    });
                    if tab.expansion > 0 {
                        let map = tab.highlight_map();
                        let mut names: Vec<(u8, String)> = map
                            .iter()
                            .filter(|(_, &l)| l > 0)
                            .map(|(&n, &l)| (l, tab.model.nets[n as usize].name.clone()))
                            .collect();
                        names.sort();
                        for (l, n) in names {
                            ui.label(RichText::new(format!("  L{l} · {n}")).weak().size(11.0));
                        }
                    }
                    if ui.button("copy net name").clicked() {
                        ctx.copy_text(name);
                    }
                } else {
                    ui.label(RichText::new("no net selected").weak());
                }

                ui.add_space(8.0);
                ui.heading("Nets on board");
                ui.separator();
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut tab.inboard_query)
                            .hint_text("find on this board (Ctrl+F)"),
                    );
                    if tab.focus_search {
                        resp.request_focus();
                        tab.focus_search = false;
                    }
                });
                let query = tab.inboard_query.trim().to_string();
                let mut select_net = None;
                let mut center_part = None;
                egui::ScrollArea::vertical()
                    .id_salt("nets_scroll")
                    .show(ui, |ui| {
                        if query.is_empty() {
                            // Busiest nets first: that's where the rails are.
                            let mut order: Vec<usize> = (0..tab.model.nets.len()).collect();
                            order.sort_by_key(|&i| std::cmp::Reverse(tab.model.nets[i].pins.len()));
                            for i in order.into_iter().take(400) {
                                let n = &tab.model.nets[i];
                                let selected = tab.selected_net == Some(i as u32);
                                if ui
                                    .selectable_label(
                                        selected,
                                        format!("{} ({})", n.name, n.pins.len()),
                                    )
                                    .clicked()
                                {
                                    select_net = Some(i as u32);
                                }
                            }
                        } else {
                            for hit in fbv_search::search_board(&tab.model, &query) {
                                match hit {
                                    fbv_search::InBoardHit::Part(i) => {
                                        let p = &tab.model.parts[i];
                                        if ui
                                            .selectable_label(
                                                tab.selected_part == Some(i),
                                                format!("part {}", p.refdes),
                                            )
                                            .clicked()
                                        {
                                            center_part = Some(i);
                                        }
                                    }
                                    fbv_search::InBoardHit::Net(i) => {
                                        let n = &tab.model.nets[i];
                                        if ui
                                            .selectable_label(
                                                tab.selected_net == Some(i as u32),
                                                format!("net {} ({})", n.name, n.pins.len()),
                                            )
                                            .clicked()
                                        {
                                            select_net = Some(i as u32);
                                        }
                                    }
                                }
                            }
                        }
                    });
                if let Some(n) = select_net {
                    // Selected from the net list: no specific pin, the
                    // netweb origin falls back to a facing-side member.
                    tab.selected_pin = None;
                    tab.select_net(n);
                }
                if let Some(i) = center_part {
                    tab.center_request = Some(CenterRequest::Part(i));
                }
            });
        if let Some((board, part, h)) = harvested_change {
            let _ = self.db.set_harvested(board, part, h);
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Tab strip
            let mut close: Option<usize> = None;
            if !self.tabs.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let selected = i == self.active;
                        if ui.selectable_label(selected, &tab.title).clicked() {
                            self.active = i;
                        }
                        if ui.small_button("×").on_hover_text("close").clicked() {
                            close = Some(i);
                        }
                        ui.separator();
                    }
                });
                ui.separator();
            }
            if let Some(i) = close {
                self.tabs.remove(i);
                if self.active >= self.tabs.len() && self.active > 0 {
                    self.active = self.tabs.len() - 1;
                }
            }

            if let Some(tab) = self.tabs.get_mut(self.active) {
                // Canvas toolbar: side switching and view controls, with the
                // current side always visible at a glance.
                ui.horizontal(|ui| {
                    let (side_text, side_color) = if tab.view.bottom {
                        ("BOTTOM", Color32::from_rgb(255, 160, 70))
                    } else {
                        ("TOP", Color32::from_rgb(110, 190, 255))
                    };
                    ui.label(
                        RichText::new(side_text)
                            .color(side_color)
                            .strong()
                            .size(15.0),
                    );
                    if ui
                        .button("⇅ Flip side")
                        .on_hover_text("Space — view the other side of the board")
                        .clicked()
                    {
                        tab.view.flip_side();
                    }
                    if ui
                        .button("Mirror")
                        .on_hover_text("M — mirror the view (match the board under your scope)")
                        .clicked()
                    {
                        tab.view.toggle_mirror();
                    }
                    if ui.button("Rotate").on_hover_text("R — rotate 90°").clicked() {
                        tab.view.rotate_ccw();
                    }
                    if ui.button("Fit").on_hover_text("F — fit board to window").clicked() {
                        tab.view.fit_pending = true;
                    }
                    ui.separator();
                    ui.checkbox(&mut tab.ghost_back, "ghost far side")
                        .on_hover_text("show the other side's parts faintly under this side");
                    ui.checkbox(&mut tab.show_netweb, "net lines")
                        .on_hover_text("fan lines from the clicked pin to every pad on its net");
                    if tab.view.mirror {
                        ui.separator();
                        ui.label(
                            RichText::new("mirrored")
                                .color(Color32::from_rgb(255, 200, 120))
                                .size(11.0),
                        );
                    }
                });
                ui.separator();
                tab.canvas(ui);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new(
                            "Open a board from the Library, or search all boards above.\n\n\
                             Space = flip side · M = mirror · R = rotate · F = fit · \
                             X = net expansion · Ctrl+Shift+F = global search",
                        )
                        .weak(),
                    );
                });
            }
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tab) = self.tabs.get(self.active) {
                    let side = if tab.view.bottom { "BOTTOM" } else { "TOP" };
                    let mirror = if tab.view.mirror { " · mirrored" } else { "" };
                    ui.label(format!(
                        "{side} · rot {}°{mirror}",
                        (tab.view.rot as u32) * 90
                    ));
                    ui.separator();
                    if let Some(w) = tab.hover_world {
                        ui.label(format!("{:.0}, {:.0} mil", w.x, w.y));
                        ui.separator();
                    }
                    if let Some(net) = tab.selected_net {
                        let n = &tab.model.nets[net as usize];
                        ui.label(format!("net {} · {} pins", n.name, n.pins.len()));
                        ui.separator();
                    }
                }
                if self.import_rx.is_some() {
                    ui.spinner();
                    ui.label(format!(
                        "indexing {} / {} — {}",
                        self.import_done, self.import_total, self.import_last
                    ));
                    ui.separator();
                }
                if let Ok((boards, parts, _nets)) = self.db.stats() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("library: {boards} boards · {parts} parts"));
                    });
                }
            });
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut save = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("Watched folders").strong());
                let mut remove = None;
                for (i, root) in self.settings.library_roots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(root.to_string_lossy());
                        if ui.small_button("remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.library_roots.remove(i);
                    save = true;
                }

                ui.add_space(10.0);
                ui.label(RichText::new("FZ key (ASUS .fz files)").strong());
                ui.label(
                    RichText::new(
                        "44 hex words, the same value OpenBoardView users configure as FZKey. \
                         Without it, encrypted .fz files land in Problems.",
                    )
                    .weak()
                    .size(11.0),
                );
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut self.settings.fz_key_text)
                            .desired_rows(3)
                            .hint_text("0x12345678 0x9abcdef0 …  (44 words)"),
                    )
                    .changed()
                {
                    save = true;
                }
                match (
                    self.settings.fz_key_text.trim().is_empty(),
                    self.settings.fz_key(),
                ) {
                    (true, _) => {}
                    (false, Some(_)) => {
                        ui.label(RichText::new("✓ 44 words parsed").color(Color32::from_rgb(90, 200, 90)));
                    }
                    (false, None) => {
                        ui.label(
                            RichText::new("not valid: expected exactly 44 hex words")
                                .color(Color32::from_rgb(220, 90, 60)),
                        );
                    }
                }

                ui.add_space(10.0);
                ui.label(RichText::new("XZZ key (.pcb files)").strong());
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.settings.xzz_key_text)
                            .hint_text("0x…  (64-bit hex)"),
                    )
                    .changed()
                {
                    save = true;
                }

                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!(
                        "Library data: {}",
                        settings::data_dir().to_string_lossy()
                    ))
                    .weak()
                    .size(11.0),
                );
            });
        if save {
            self.settings.save();
        }
        self.settings_open = open;
    }

    fn error_toast(&mut self, ctx: &egui::Context) {
        if let Some(msg) = self.error.clone() {
            let mut open = true;
            egui::Window::new("Error")
                .open(&mut open)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 40.0])
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(&msg);
                    if ui.button("dismiss").clicked() {
                        self.error = None;
                    }
                });
            if !open {
                self.error = None;
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_import(ctx);
        if self.boards_dirty {
            self.refresh_boards();
        }
        self.keyboard(ctx);
        self.top_bar(ctx);
        self.left_panel(ctx);
        self.right_panel(ctx);
        self.status_bar(ctx);
        self.central(ctx);
        self.settings_window(ctx);
        self.error_toast(ctx);
    }
}
