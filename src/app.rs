//! Application shell: tabs, welcome page, clone/init, appearance settings.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Align, Align2, Color32, Context, CursorIcon, Layout, Rect, ScrollArea, Sense, Stroke, StrokeKind, TextureHandle, Ui, pos2, vec2};

use crate::git::{self, OpResult};
use crate::icon;
use crate::prefs::Prefs;
use crate::repo::RepoView;
use crate::theme::{self, Colors, Mode, PaletteStyle, SEEDS, ThemePrefs, shape};
use crate::widgets::{self as w, Kind, ic};

struct CloneForm {
    url: String,
    parent: String,
    name: String,
}

impl CloneForm {
    /// Folder git will create: the explicit name, or the last URL segment.
    fn target(&self) -> PathBuf {
        let from_url = self.url.trim().trim_end_matches('/').rsplit(['/', ':', '\\']).next().unwrap_or("").trim_end_matches(".git");
        let name = if self.name.trim().is_empty() { from_url } else { self.name.trim() };
        PathBuf::from(self.parent.trim()).join(name)
    }
}

pub struct App {
    prefs: Prefs,
    colors: Colors,
    system_dark: bool,
    repos: Vec<RepoView>,
    active: Option<usize>,
    logo: Option<(Color32, TextureHandle)>,
    settings: bool,
    previews: Option<(ThemePrefs, bool, Vec<Colors>)>,
    clone_form: Option<CloneForm>,
    cloning: bool,
    clone_tx: Sender<(PathBuf, OpResult)>,
    clone_rx: Receiver<(PathBuf, OpResult)>,
    notice: Option<(String, bool)>,
    focused: bool,
}

fn home_dir() -> String {
    std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default()
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install_fonts(ctx);
        let prefs = Prefs::load();
        let system_dark = ctx.system_theme() != Some(egui::Theme::Light);
        let colors = Colors::build(&prefs.theme, system_dark);
        theme::apply(ctx, &colors);
        ctx.set_zoom_factor(prefs.zoom.clamp(0.6, 2.0));
        // Finding installed editors takes a moment; have it done before the menu is opened.
        std::thread::spawn(|| {
            crate::open_in::targets();
        });
        let (clone_tx, clone_rx) = channel();
        let mut app = Self {
            prefs,
            colors,
            system_dark,
            repos: Vec::new(),
            active: None,
            logo: None,
            settings: false,
            previews: None,
            clone_form: None,
            cloning: false,
            clone_tx,
            clone_rx,
            notice: None,
            focused: true,
        };
        for path in app.prefs.open.clone().into_iter().chain(open) {
            app.open_repo(ctx, &path);
        }
        app
    }

    // ------------------------------------------------------------ state

    fn save_prefs(&mut self) {
        self.prefs.open = self.repos.iter().map(|r| r.path.clone()).collect();
        if let Err(reason) = self.prefs.save() {
            self.notice = Some((reason, true));
        }
    }

    fn retheme(&mut self, ctx: &Context) {
        self.colors = Colors::build(&self.prefs.theme, self.system_dark);
        theme::apply(ctx, &self.colors);
    }

    fn open_repo(&mut self, ctx: &Context, path: &Path) {
        let Some(root) = git::root(path) else {
            self.notice = Some((format!("{} is not a git repository", path.display()), true));
            return;
        };
        self.active = Some(match self.repos.iter().position(|r| r.path == root) {
            Some(index) => index,
            None => {
                let folders = self.prefs.open_folders.get(&root.display().to_string()).cloned().unwrap_or_default();
                self.repos.push(RepoView::new(root.clone(), self.prefs.history_limit, folders, ctx));
                self.repos.len() - 1
            }
        });
        self.prefs.remember(&root);
        self.save_prefs();
    }

    fn close_repo(&mut self, index: usize) {
        self.repos.remove(index);
        self.active = match self.active {
            Some(active) if active > index => Some(active - 1),
            Some(active) if active == index => index.checked_sub(1).or((!self.repos.is_empty()).then_some(0)),
            other => other,
        };
        self.save_prefs();
    }

    // ponytail: native dialogs block the UI thread while open; move to rfd::AsyncFileDialog if that bites.
    fn browse_and_open(&mut self, ctx: &Context) {
        if let Some(folder) = rfd::FileDialog::new().set_title("Open repository").pick_folder() {
            self.open_repo(ctx, &folder);
        }
    }

    fn init_repo(&mut self, ctx: &Context) {
        let Some(folder) = rfd::FileDialog::new().set_title("Create repository in folder").pick_folder() else { return };
        if git::root(&folder).is_none() {
            let result = git::init(&folder);
            if !result.ok {
                self.notice = Some((format!("Init failed: {}", result.error), true));
                return;
            }
        }
        self.open_repo(ctx, &folder);
    }

    fn start_clone(&mut self, ctx: &Context, form: &CloneForm) {
        let (tx, ctx, target) = (self.clone_tx.clone(), ctx.clone(), form.target());
        let (url, parent, name) = (form.url.trim().to_string(), PathBuf::from(form.parent.trim()), form.name.trim().to_string());
        self.cloning = true;
        std::thread::spawn(move || {
            let _ = tx.send((target, git::clone(&url, &parent, &name)));
            ctx.request_repaint();
        });
    }

    fn poll(&mut self, ctx: &Context) {
        let system_dark = ctx.system_theme().map_or(self.system_dark, |t| t == egui::Theme::Dark);
        if system_dark != self.system_dark {
            self.system_dark = system_dark;
            self.retheme(ctx);
        }
        while let Ok((target, result)) = self.clone_rx.try_recv() {
            self.cloning = false;
            if result.ok {
                self.open_repo(ctx, &target);
            } else {
                self.notice = Some((format!("Clone failed: {}", result.error.trim()), true));
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for path in dropped {
            self.open_repo(ctx, &path);
        }
        let mut requests = Vec::new();
        for repo in &mut self.repos {
            repo.poll(ctx);
            requests.extend(repo.open_request.take());
        }
        for path in requests {
            self.open_repo(ctx, &path);
        }
        // Remember the zoom (Ctrl with plus or minus) and which branch folders are open.
        let mut changed = ctx.zoom_factor() != self.prefs.zoom;
        self.prefs.zoom = ctx.zoom_factor();
        for repo in &self.repos {
            let stored = self.prefs.open_folders.entry(repo.path.display().to_string()).or_default();
            if *stored != repo.opened {
                stored.clone_from(&repo.opened);
                changed = true;
            }
        }
        if changed {
            self.save_prefs();
        }
        // Files may have changed while another window had focus.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.focused && let Some(repo) = self.active.and_then(|i| self.repos.get_mut(i)).filter(|r| !r.is_busy()) {
            repo.reload(ctx);
        }
        self.focused = focused;
    }

    fn logo(&mut self, ctx: &Context) -> TextureHandle {
        let key = self.colors.primary;
        if let Some((cached, texture)) = &self.logo
            && *cached == key
        {
            return texture.clone();
        }
        // Themed like an Android adaptive icon: the mark takes the primary color.
        let rgb = |c: Color32| [c.r(), c.g(), c.b()];
        let size = 192;
        let pixels = icon::render(size, rgb(self.colors.primary), rgb(self.colors.on_primary));
        let image = egui::ColorImage::from_rgba_unmultiplied([size, size], &pixels);
        let texture = ctx.load_texture("spoon-logo", image, egui::TextureOptions::LINEAR);
        self.logo = Some((key, texture.clone()));
        texture
    }

    fn paint_logo(&mut self, ui: &Ui, rect: Rect) {
        let texture = self.logo(ui.ctx());
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
    }

    // ------------------------------------------------------------ top bar

    fn top_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let c = self.colors;
        let ctx = ui.ctx().clone();
        self.paint_logo(ui, Rect::from_center_size(pos2(rect.left() + 40.0, rect.center().y), vec2(34.0, 34.0)));

        let tabs = Rect::from_min_max(pos2(rect.left() + 80.0, rect.top()), pos2(rect.right() - 108.0, rect.bottom()));
        let mut close = None;
        w::hchild(ui, "tabs", tabs, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ScrollArea::horizontal().id_salt("tabs-scroll").show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if tab(ui, &c, ic::HOME, "Home", self.active.is_none(), false).0 {
                        self.active = None;
                    }
                    for (i, repo) in self.repos.iter().enumerate() {
                        let (clicked, closed) = tab(ui, &c, ic::FOLDER, &repo.name, self.active == Some(i), true);
                        if closed {
                            close = Some(i);
                        } else if clicked {
                            self.active = Some(i);
                        }
                    }
                    if w::icon_btn(ic::ADD, "Open repository").size(36.0).show(ui).clicked() {
                        self.browse_and_open(&ctx);
                    }
                    if self.cloning {
                        let (spot, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                        w::loading_indicator(ui, spot.center(), 11.0);
                        ui.label(egui::RichText::new("Cloning\u{2026}").font(theme::emph(13.0)).color(c.on_surface_variant));
                    }
                });
            });
        });
        if let Some(index) = close {
            self.close_repo(index);
        }

        let actions = Rect::from_min_max(pos2(rect.right() - 104.0, rect.top()), pos2(rect.right() - 12.0, rect.bottom()));
        w::hchild(ui, "bar-actions", actions, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (icon, next) = match self.prefs.theme.mode {
                Mode::System => (ic::AUTO, Mode::Light),
                Mode::Light => (ic::LIGHT, Mode::Dark),
                Mode::Dark => (ic::DARK, Mode::System),
            };
            if w::icon_btn(icon, "Theme: system, light or dark").show(ui).clicked() {
                self.prefs.theme.mode = next;
                self.retheme(&ctx);
                self.save_prefs();
            }
            if w::icon_btn(ic::PALETTE, "Appearance").selected(self.settings).show(ui).clicked() {
                self.settings = true;
            }
        });
    }

    // ------------------------------------------------------------ welcome

    fn welcome(&mut self, ui: &mut Ui, rect: Rect) {
        let c = self.colors;
        let ctx = ui.ctx().clone();
        let width = (rect.width() - 48.0).min(760.0);
        let column = Rect::from_min_size(pos2(rect.center().x - width / 2.0, rect.top() + 12.0), vec2(width, rect.height() - 24.0));

        let hero = Rect::from_min_size(column.min, vec2(width, 208.0));
        w::card(ui, hero, c.surface, shape::EXTRA_LARGE);
        self.paint_logo(ui, Rect::from_center_size(pos2(hero.left() + 96.0, hero.center().y), vec2(128.0, 128.0)));
        let text_x = hero.left() + 190.0;
        let p = ui.painter();
        p.text(pos2(text_x, hero.top() + 62.0), Align2::LEFT_CENTER, "Spoon", theme::emph(44.0), c.on_surface);
        p.text(pos2(text_x + 2.0, hero.top() + 104.0), Align2::LEFT_CENTER, "A Git client with a commit graph, written in Rust.", theme::body(15.0), c.on_surface_variant);
        let buttons = Rect::from_min_size(pos2(text_x, hero.top() + 132.0), vec2(hero.right() - text_x - 16.0, 48.0));
        w::hchild(ui, "hero-actions", buttons, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if w::btn("Open").icon(ic::FOLDER_OPEN).height(48.0).show(ui).clicked() {
                self.browse_and_open(&ctx);
            }
            if w::btn("Clone").icon(ic::CLOUD).kind(Kind::Tonal).height(48.0).enabled(!self.cloning).show(ui).clicked() {
                self.clone_form = Some(CloneForm { url: String::new(), parent: home_dir(), name: String::new() });
            }
            if w::btn("New").icon(ic::NEW_FOLDER).kind(Kind::Tonal).height(48.0).show(ui).clicked() {
                self.init_repo(&ctx);
            }
        });

        let title = pos2(column.left() + 12.0, hero.bottom() + 30.0);
        ui.painter().text(title, Align2::LEFT_CENTER, "Repositories", theme::emph(16.0), c.on_surface);
        let list = Rect::from_min_max(pos2(column.left(), hero.bottom() + 52.0), column.max);
        w::card(ui, list, c.surface, shape::EXTRA_LARGE);
        if self.prefs.repos.is_empty() {
            w::empty_state(ui, list, ic::FOLDER_OPEN, "No repositories yet", "Open one, or drop a folder onto this window");
            return;
        }
        let repos = self.prefs.repos.clone();
        let (mut open, mut forget) = (None, None);
        w::rows(ui, "known-repos", list.shrink(10.0), 56.0, repos.len(), None, |ui, range| {
            for i in range {
                let path = &repos[i];
                let (row, resp) = w::row(ui, 56.0);
                w::row_bg(ui, row, &resp, false);
                let p = ui.painter();
                let spot = pos2(row.left() + 30.0, row.center().y);
                p.circle_filled(spot, 18.0, c.secondary_container);
                w::icon_at(p, spot, ic::FOLDER, 18.0, c.on_secondary_container);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let line = Rect::from_min_max(pos2(row.left() + 60.0, row.top() + 8.0), pos2(row.right() - 48.0, row.top() + 28.0));
                w::label_in(p, line, &name, theme::emph(14.5), c.on_surface);
                w::label_in(p, line.translate(vec2(0.0, 20.0)), &path.display().to_string(), theme::body(12.0), c.on_surface_variant);
                if ui.rect_contains_pointer(row) {
                    let button = Rect::from_center_size(pos2(row.right() - 26.0, row.center().y), vec2(30.0, 30.0));
                    if w::icon_btn(ic::CLOSE, "Remove from list").size(30.0).show_at(ui, resp.id.with("forget"), button).clicked() {
                        forget = Some(i);
                    }
                }
                if resp.clicked() {
                    open = Some(path.clone());
                }
            }
        });
        if let Some(index) = forget {
            self.prefs.repos.remove(index);
            self.save_prefs();
        } else if let Some(path) = open {
            self.open_repo(&ctx, &path);
        }
    }

    // ------------------------------------------------------------ dialogs

    fn clone_dialog(&mut self, ctx: &Context) {
        let Some(form) = &mut self.clone_form else { return };
        let (action, dismissed) = w::dialog(ctx, "clone-dialog", ic::CLOUD, "Clone repository", 500.0, |ui| {
            w::field_label(ui, "URL");
            let width = ui.available_width();
            w::text_field(ui, &mut form.url, "https://example.com/team/repo.git", Some(ic::LINK), width);
            w::field_label(ui, "Parent folder");
            ui.horizontal(|ui| {
                w::text_field(ui, &mut form.parent, "Where to put the clone", Some(ic::FOLDER), width - 104.0);
                if w::btn("Browse").kind(Kind::Tonal).show(ui).clicked()
                    && let Some(folder) = rfd::FileDialog::new().pick_folder()
                {
                    form.parent = folder.display().to_string();
                }
            });
            w::field_label(ui, "Folder name");
            w::text_field(ui, &mut form.name, "Optional: defaults to the repository name", None, width);
            let valid = !form.url.trim().is_empty() && Path::new(form.parent.trim()).is_dir();
            w::dialog_actions(ui, "Clone", Kind::Filled, valid)
        });
        match action {
            Some(true) => {
                let form = self.clone_form.take();
                if let Some(form) = form {
                    self.start_clone(ctx, &form);
                }
            }
            Some(false) => self.clone_form = None,
            None if dismissed => self.clone_form = None,
            None => {}
        }
    }

    fn settings_dialog(&mut self, ctx: &Context) {
        let c = self.colors;
        let key = (self.prefs.theme, self.system_dark);
        if self.previews.as_ref().is_none_or(|(prefs, dark, _)| (*prefs, *dark) != key) {
            let build = |style| Colors::build(&ThemePrefs { style, ..key.0 }, key.1);
            self.previews = Some((key.0, key.1, PaletteStyle::ALL.map(build).to_vec()));
        }
        let previews = self.previews.as_ref().map(|(.., list)| list.clone()).unwrap_or_default();
        let mut prefs = self.prefs.theme;

        let (close, dismissed) = w::dialog(ctx, "appearance-dialog", ic::PALETTE, "Appearance", 560.0, |ui| {
            w::body_text(ui, "Colors are generated from one source color, like Material You on Android.");
            w::field_label(ui, "Theme");
            let modes = [(Mode::System, Some(ic::AUTO), "System"), (Mode::Light, Some(ic::LIGHT), "Light"), (Mode::Dark, Some(ic::DARK), "Dark")];
            w::segmented(ui, "mode", &mut prefs.mode, &modes, 40.0);

            w::field_label(ui, "Source color");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for seed in SEEDS {
                    if swatch(ui, &c, seed, prefs.seed == seed) {
                        prefs.seed = seed;
                    }
                }
                ui.add_space(6.0);
                ui.color_edit_button_srgb(&mut prefs.seed).on_hover_text("Custom color");
            });

            w::field_label(ui, "Palette style");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (style, colors) in PaletteStyle::ALL.into_iter().zip(&previews) {
                    if style_preview(ui, &c, colors, style.label(), prefs.style == style) {
                        prefs.style = style;
                    }
                }
            });

            w::field_label(ui, "Contrast");
            let mut level = (prefs.contrast * 2.0).round() as i32;
            let levels = [(0, Some(ic::CONTRAST), "Standard"), (1, None, "Medium"), (2, None, "High")];
            if w::segmented(ui, "contrast", &mut level, &levels, 40.0) {
                prefs.contrast = level as f32 / 2.0;
            }

            ui.add_space(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let done = w::btn("Done").show(ui).clicked();
                if w::btn("Reset").kind(Kind::Text).show(ui).clicked() {
                    prefs = ThemePrefs::default();
                }
                done
            })
            .inner
        });
        if prefs != self.prefs.theme {
            self.prefs.theme = prefs;
            self.retheme(ctx);
            self.save_prefs();
        }
        if close || dismissed {
            self.settings = false;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        #[cfg(all(windows, target_pointer_width = "64"))]
        crate::dpi_drag::guard_windows();
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, self.colors.surface_container);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), 60.0));
        let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
        self.top_bar(ui, bar);
        match self.active.filter(|i| *i < self.repos.len()) {
            Some(index) => self.repos[index].ui(ui, body),
            None => self.welcome(ui, body),
        }
        for repo in &mut self.repos {
            repo.merge_window(&ctx);
        }
        self.clone_dialog(&ctx);
        if self.settings {
            self.settings_dialog(&ctx);
        }
        if let Some((text, error)) = self.notice.clone() {
            let (_, dismissed) = w::snackbar(&ctx, "app-snackbar", &text, error, "");
            if dismissed {
                self.notice = None;
            }
        }
    }
}

// ---------------------------------------------------------------- pieces

/// Tab pill. Returns (clicked, close clicked).
fn tab(ui: &mut Ui, c: &Colors, icon: &str, label: &str, selected: bool, closable: bool) -> (bool, bool) {
    let font = if selected { theme::emph(13.5) } else { theme::body(13.5) };
    let label_w = w::text_width(ui.painter(), label, font.clone()).min(180.0);
    let close_w = if closable { 26.0 } else { 6.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(14.0 + 18.0 + 8.0 + label_w + close_w + 8.0, 40.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id.with("selected"), selected, 0.2);
    let hot = ui.rect_contains_pointer(rect);
    let p = ui.painter();
    let fill = Colors::state(c.surface_container, c.on_surface, if hot { 0.08 } else { 0.0 }).lerp_to_gamma(c.secondary_container, t);
    p.rect_filled(rect, w::radius(egui::lerp(shape::MEDIUM..=20.0, t)), fill);
    let fg = c.on_surface_variant.lerp_to_gamma(c.on_secondary_container, t);
    w::icon_at(p, pos2(rect.left() + 23.0, rect.center().y), icon, 18.0, fg);
    let text = Rect::from_min_size(pos2(rect.left() + 40.0, rect.top()), vec2(label_w, rect.height()));
    w::label_in(p, text, label, font, fg);
    let mut closed = false;
    if closable && (hot || selected) {
        let button = Rect::from_center_size(pos2(rect.right() - 18.0, rect.center().y), vec2(22.0, 22.0));
        closed = w::icon_btn(ic::CLOSE, "Close tab").size(22.0).show_at(ui, resp.id.with("close"), button).clicked();
    }
    (resp.on_hover_cursor(CursorIcon::PointingHand).clicked(), closed)
}

/// Source color swatch: a circle that morphs into a rounded square when selected.
fn swatch(ui: &mut Ui, c: &Colors, seed: [u8; 3], selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id.with("selected"), selected, 0.2);
    let color = Color32::from_rgb(seed[0], seed[1], seed[2]);
    let p = ui.painter();
    let corner = w::radius(egui::lerp(22.0..=13.0, t));
    p.rect_filled(rect.shrink(if resp.hovered() { 0.0 } else { 2.0 }), corner, color);
    if t > 0.0 {
        p.rect_stroke(rect.expand(3.0), w::radius(16.0), Stroke::new(2.0, c.primary.gamma_multiply(t)), StrokeKind::Inside);
        w::icon_at(p, rect.center(), ic::CHECK, 22.0, Color32::WHITE.gamma_multiply(t));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// Palette style tile: primary on top, secondary and tertiary below.
fn style_preview(ui: &mut Ui, c: &Colors, colors: &Colors, label: &str, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(74.0, 92.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id.with("selected"), selected, 0.2);
    let p = ui.painter();
    let fill = Colors::state(c.surface_high, c.on_surface, if resp.hovered() { 0.08 } else { 0.0 }).lerp_to_gamma(c.secondary_container, t);
    p.rect_filled(rect, w::radius(shape::LARGE), fill);
    let center = pos2(rect.center().x, rect.top() + 36.0);
    let r = 24.0;
    p.circle_filled(center, r, colors.primary);
    let left = Rect::from_min_max(center - vec2(r, 0.0), center + vec2(0.0, r));
    p.with_clip_rect(left).circle_filled(center, r, colors.secondary_container);
    p.with_clip_rect(left.translate(vec2(r, 0.0))).circle_filled(center, r, colors.tertiary);
    let font = if selected { theme::emph(11.5) } else { theme::body(11.5) };
    let fg = c.on_surface_variant.lerp_to_gamma(c.on_secondary_container, t);
    p.text(pos2(rect.center().x, rect.bottom() - 15.0), Align2::CENTER_CENTER, label, font, fg);
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}
