//! The application shell: per-frame session tick, sidebar, banner, toasts,
//! modals, page routing and safe close handling.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use egui::{
    Align, Align2, CentralPanel, Frame, Key, Layout, Margin, Modifiers, Panel, Popup, RichText, ScrollArea, Sense,
    ViewportCommand,
};
use egui_phosphor::regular as icon;

use crate::dates::now_ts;
use crate::lock::Acquire;
use crate::model::{Id, Ts};
use crate::prefs::{save_prefs_to, Prefs, Theme};
use crate::query::{search, SearchHit};
use crate::session::{LeaveReason, Mode, Session, SessionEvent};
use crate::ui::audit::{self, AuditState};
use crate::ui::detail::{self, DetailState};
use crate::ui::personnel::{self, PersonnelState};
use crate::ui::sections::{self, SectionsState};
use crate::ui::settings::{self, SettingsState};
use crate::ui::summary::{self, SummaryState};
use crate::ui::theme::{self, serif, Palette};
use crate::ui::widgets::{icon_button, nav_item, search_box};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Summary,
    Sections,
    Personnel,
    Audit,
    Settings,
}

impl Page {
    const ALL: [Page; 5] = [Page::Summary, Page::Sections, Page::Personnel, Page::Audit, Page::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Page::Summary => "Summary",
            Page::Sections => "Sections",
            Page::Personnel => "Personnel",
            Page::Audit => "Audit log",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::Summary => icon::HOUSE,
            Page::Sections => icon::SQUARES_FOUR,
            Page::Personnel => icon::USERS,
            Page::Audit => icon::CLOCK_COUNTER_CLOCKWISE,
            Page::Settings => icon::GEAR,
        }
    }
}

/// A short confirmation shown bottom-centre.
pub struct Toast {
    text: String,
    until: Instant,
}

impl Toast {
    pub fn new(text: impl Into<String>) -> Toast {
        Toast::lasting(text, Duration::from_secs(3))
    }

    pub fn lasting(text: impl Into<String>, d: Duration) -> Toast {
        Toast { text: text.into(), until: Instant::now() + d }
    }
}

/// An action that runs once the user confirms it. Pages add variants as they
/// need confirmations (Tasks 12–16).
pub enum PendingAction {}

pub enum Modal {
    Confirm { title: String, body: String, action: PendingAction },
    InactivityCountdown,
    /// Saving on close failed: offer Retry or Quit anyway.
    ExitFailed { error: String },
    /// "Quit anyway" ran; `rescued` is the rescue copy, if one was written.
    ExitRescued { rescued: Option<PathBuf> },
}

/// The per-frame context passed to page functions.
#[allow(dead_code)] // Fields are consumed by the pages as Tasks 12–16 fill them in.
pub(crate) struct Ctx<'a> {
    pub session: &'a mut Session,
    pub pal: &'a Palette,
    pub today: NaiveDate,
    pub now: Ts,
    pub toasts: &'a mut Vec<Toast>,
    pub open_detail: &'a mut Option<(Id, Id)>,
}

pub struct TrackerApp {
    session: Session,
    prefs: Prefs,
    prefs_path: Option<PathBuf>,
    page: Page,
    styled: bool,
    toasts: Vec<Toast>,
    modal: Option<Modal>,
    /// When the inactivity countdown ends.
    warn_deadline: Option<Instant>,
    search: String,
    open_detail: Option<(Id, Id)>,
    /// The close was cleared (saved, or the user chose to quit anyway).
    allow_close: bool,
    summary: SummaryState,
    sections: SectionsState,
    personnel: PersonnelState,
    audit: AuditState,
    settings: SettingsState,
    detail: DetailState,
}

enum Tone {
    Error,
    Warn,
    Info,
}

impl TrackerApp {
    pub fn new(cc: &eframe::CreationContext, session: Session, prefs: Prefs, prefs_path: Option<PathBuf>) -> Self {
        let mut app = Self::build(session, prefs, prefs_path);
        app.style(&cc.egui_ctx);
        app
    }

    /// No prefs file, light theme.
    pub fn new_for_test(session: Session) -> Self {
        Self::build(session, Prefs::default(), None)
    }

    fn build(session: Session, prefs: Prefs, prefs_path: Option<PathBuf>) -> Self {
        TrackerApp {
            session,
            prefs,
            prefs_path,
            page: Page::Summary,
            styled: false,
            toasts: Vec::new(),
            modal: None,
            warn_deadline: None,
            search: String::new(),
            open_detail: None,
            allow_close: false,
            summary: SummaryState::default(),
            sections: SectionsState::default(),
            personnel: PersonnelState::default(),
            audit: AuditState::default(),
            settings: SettingsState::default(),
            detail: DetailState::default(),
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    fn style(&mut self, ctx: &egui::Context) {
        theme::install_fonts(ctx);
        theme::apply(ctx, self.prefs.theme);
        self.styled = true;
    }

    /// Draws one frame into the root `ui` (used by eframe and by tests).
    pub fn show_root(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            // New fonts only take effect from the next pass: draw nothing yet.
            self.style(&ctx);
            ctx.request_repaint();
            return;
        }
        let (now, inst) = (now_ts(), Instant::now());
        if ctx.input(|i| !i.events.is_empty()) {
            self.session.note_activity(inst);
        }
        for ev in self.session.tick(now, inst) {
            self.on_event(ev, inst);
        }
        self.handle_close(&ctx, now);
        self.toasts.retain(|t| t.until > inst);
        let focus_search = self.modal.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K));
        if focus_search && self.prefs.sidebar_collapsed {
            self.set_sidebar_collapsed(false);
        }
        let pal = theme::palette(self.prefs.theme);

        let width = if self.prefs.sidebar_collapsed { 60.0 } else { 264.0 };
        Panel::left("sidebar")
            .exact_size(width)
            .resizable(false)
            .frame(Frame::new().fill(pal.sidebar).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.sidebar(ui, &pal, now, inst, focus_search));
        self.banner(ui, &pal, now, inst);
        let today = now.date_naive();
        if self.open_detail.is_some() {
            Panel::right("detail")
                .default_size(420.0)
                .frame(Frame::new().fill(pal.card).inner_margin(Margin::same(20)))
                .show(ui, |ui| {
                    let mut cx = Ctx {
                        session: &mut self.session,
                        pal: &pal,
                        today,
                        now,
                        toasts: &mut self.toasts,
                        open_detail: &mut self.open_detail,
                    };
                    detail::show(ui, &mut cx, &mut self.detail);
                });
        }
        CentralPanel::default()
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(40, 28)))
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    let mut cx = Ctx {
                        session: &mut self.session,
                        pal: &pal,
                        today,
                        now,
                        toasts: &mut self.toasts,
                        open_detail: &mut self.open_detail,
                    };
                    match self.page {
                        Page::Summary => summary::show(ui, &mut cx, &mut self.summary),
                        Page::Sections => sections::show(ui, &mut cx, &mut self.sections),
                        Page::Personnel => personnel::show(ui, &mut cx, &mut self.personnel),
                        Page::Audit => audit::show(ui, &mut cx, &mut self.audit),
                        Page::Settings => settings::show(ui, &mut cx, &mut self.settings),
                    }
                });
            });
        self.show_modal(&ctx, now, inst);
        self.show_toasts(&ctx, &pal);
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn on_event(&mut self, ev: SessionEvent, inst: Instant) {
        match ev {
            SessionEvent::Saved => self.toasts.push(Toast::new("Saved")),
            // The banner shows `save_error` / the mode; nothing else to do.
            SessionEvent::SaveFailed(_) | SessionEvent::EditingAvailable => {}
            SessionEvent::InactivityWarning { secs_left } => {
                self.warn_deadline = Some(inst + Duration::from_secs(secs_left));
                if self.modal.is_none() {
                    self.modal = Some(Modal::InactivityCountdown);
                }
            }
            SessionEvent::LeftEditMode(reason) => {
                self.warn_deadline = None;
                if matches!(self.modal, Some(Modal::InactivityCountdown)) {
                    self.modal = None;
                }
                let text = match reason {
                    LeaveReason::Inactivity => "Stopped editing after inactivity",
                    LeaveReason::Manual => "Stopped editing",
                    LeaveReason::Conflict => "Another user took over editing",
                };
                self.toasts.push(Toast::new(text));
            }
            SessionEvent::Reloaded => self.toasts.push(Toast::new("Updated with the latest changes")),
            SessionEvent::Conflict(path) => self.toasts.push(Toast::lasting(
                format!("Your unsaved changes were kept in {}", path.display()),
                Duration::from_secs(10),
            )),
            SessionEvent::TookOverStale(prev) => self
                .toasts
                .push(Toast::new(format!("Took over editing from {} (their session had stopped)", prev.user))),
        }
    }

    /// eframe cannot cancel a close from `on_exit`, so save here: a failed
    /// save cancels the close and asks Retry / Quit anyway.
    fn handle_close(&mut self, ctx: &egui::Context, now: Ts) {
        if self.allow_close || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        match self.session.on_exit(now) {
            Ok(()) => self.allow_close = true,
            Err(error) => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.modal = Some(Modal::ExitFailed { error });
            }
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.allow_close = true;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn start_editing(&mut self, now: Ts, inst: Instant) {
        let text = match self.session.start_editing(now, inst) {
            Ok(Acquire::HeldBy(h)) => format!("{} is editing", h.user),
            Ok(_) => "You're editing".to_owned(),
            Err(e) => format!("Couldn't start editing: {e}"),
        };
        self.toasts.push(Toast::new(text));
    }

    fn toggle_editing(&mut self, now: Ts, inst: Instant) {
        if !self.session.is_editing() {
            return self.start_editing(now, inst);
        }
        if let Err(e) = self.session.stop_editing(now) {
            self.toasts.push(Toast::new(format!("Couldn't save: {e}")));
        }
    }

    fn can_start_editing(&self) -> bool {
        self.session.load_error().is_none() && self.session.read_only_reason().is_none()
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.prefs.theme = match self.prefs.theme {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        };
        theme::apply(ctx, self.prefs.theme);
        self.save_prefs();
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool) {
        self.prefs.sidebar_collapsed = collapsed;
        self.save_prefs();
    }

    fn save_prefs(&mut self) {
        if let Some(p) = &self.prefs_path {
            if let Err(e) = save_prefs_to(p, &self.prefs) {
                self.toasts.push(Toast::new(format!("Couldn't save preferences: {e}")));
            }
        }
    }

    fn navigate(&mut self, hit: SearchHit) {
        match hit {
            SearchHit::Staff(id) => {
                self.personnel.profile = Some(id);
                self.page = Page::Personnel;
            }
            SearchHit::Section(id) => {
                self.sections.section = Some(id);
                self.sections.test = None;
                self.page = Page::Sections;
            }
            SearchHit::Test { section_id, test_id } => {
                self.sections.section = Some(section_id);
                self.sections.test = Some(test_id);
                self.page = Page::Sections;
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, pal: &Palette, now: Ts, inst: Instant, focus_search: bool) {
        let compact = self.prefs.sidebar_collapsed;
        ui.horizontal(|ui| {
            if !compact {
                ui.label(RichText::new("Competency Tracker").family(serif()).size(19.0).color(pal.text));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if compact { "Expand sidebar" } else { "Collapse sidebar" };
                if icon_button(ui, pal, icon::SIDEBAR_SIMPLE, label).clicked() {
                    self.set_sidebar_collapsed(!compact);
                }
            });
        });
        ui.add_space(8.0);
        if !compact {
            self.search_field(ui, focus_search);
            ui.add_space(8.0);
        }
        for page in Page::ALL {
            if nav_item(ui, pal, page.icon(), page.label(), self.page == page, compact).clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let editing = self.session.is_editing();
            let ctx = ui.ctx().clone();
            let (theme_icon, theme_label) = match self.prefs.theme {
                Theme::Light => (icon::MOON, "Switch to dark mode"),
                Theme::Dark => (icon::SUN, "Switch to light mode"),
            };
            let edit_label = if editing { "Stop editing" } else { "Start editing" };
            let can_toggle = editing || self.can_start_editing();
            if compact {
                if icon_button(ui, pal, theme_icon, theme_label).clicked() {
                    self.toggle_theme(&ctx);
                }
                let glyph = if editing { icon::PENCIL_SIMPLE } else { icon::EYE };
                let resp = ui.add_enabled_ui(can_toggle, |ui| icon_button(ui, pal, glyph, edit_label)).inner;
                if resp.clicked() {
                    self.toggle_editing(now, inst);
                }
                return;
            }
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, if editing { pal.accent } else { pal.muted });
                let status = if editing { "Editing" } else { "Viewing" };
                ui.label(RichText::new(format!("{} \u{b7} {status}", self.session.user())).color(pal.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, pal, theme_icon, theme_label).clicked() {
                        self.toggle_theme(&ctx);
                    }
                });
            });
            let button = egui::Button::new(edit_label).min_size(egui::vec2(ui.available_width(), 30.0));
            if ui.add_enabled(can_toggle, button).clicked() {
                self.toggle_editing(now, inst);
            }
            ui.separator();
        });
    }

    fn search_field(&mut self, ui: &mut egui::Ui, focus: bool) {
        let resp = search_box(ui, &mut self.search, "Search  (Ctrl+K)");
        if focus {
            resp.request_focus();
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.search.clear();
        }
        let q = self.search.trim().to_owned();
        if q.is_empty() {
            return;
        }
        let hits = search(self.session.data(), &q, 8);
        let mut picked = None;
        Popup::from_response(&resp).open(true).width(resp.rect.width()).show(|ui| {
            if hits.is_empty() {
                ui.label(RichText::new("No matches").weak());
            }
            for (hit, label) in hits {
                let glyph = match hit {
                    SearchHit::Staff(_) => icon::USER,
                    SearchHit::Section(_) => icon::SQUARES_FOUR,
                    SearchHit::Test { .. } => icon::FLASK,
                };
                if ui.selectable_label(false, format!("{glyph}  {label}")).clicked() {
                    picked = Some(hit);
                }
            }
        });
        if let Some(hit) = picked {
            self.navigate(hit);
            self.search.clear();
        }
    }

    /// The single most important banner, if any: load error, read-only,
    /// save error, corrupt files, then viewing / editing available.
    fn banner(&mut self, ui: &mut egui::Ui, pal: &Palette, now: Ts, inst: Instant) {
        let s = &self.session;
        let mut start_button = false;
        let mut hover = None;
        let (tone, text) = if let Some(e) = s.load_error() {
            (Tone::Error, format!("Can't read the data folder: {e}"))
        } else if s.read_only_reason().is_some() {
            (Tone::Warn, "Read-only: data saved by a newer version".to_owned())
        } else if let Some(e) = s.save_error() {
            hover = Some(e.to_owned());
            (Tone::Error, "Not saved \u{2014} retrying".to_owned())
        } else if !s.corrupt_files().is_empty() {
            let names: Vec<String> =
                s.corrupt_files().iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
            (Tone::Warn, format!("Skipped unreadable snapshot files: {}", names.join(", ")))
        } else {
            match s.mode() {
                Mode::Editing => return,
                Mode::Viewing { holder: Some(h) } => (
                    Tone::Info,
                    format!("Viewing \u{2014} {} is editing (since {})", h.user, h.acquired_at.format("%H:%M")),
                ),
                Mode::Viewing { holder: None } => {
                    start_button = true;
                    (Tone::Info, "Editing is available".to_owned())
                }
            }
        };
        let (fill, fg) = match tone {
            Tone::Error => (pal.red_bg, pal.red_fg),
            Tone::Warn => (pal.yellow_bg, pal.yellow_fg),
            Tone::Info => (pal.grey_bg, pal.text),
        };
        let glyph = match tone {
            Tone::Error | Tone::Warn => icon::WARNING,
            Tone::Info => icon::EYE,
        };
        Panel::top("banner").frame(Frame::new().fill(fill).inner_margin(Margin::symmetric(20, 8))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph).color(fg));
                let resp = ui.label(RichText::new(text).color(fg));
                if let Some(h) = hover {
                    resp.on_hover_text(h);
                }
                if start_button && ui.button("Start editing").clicked() {
                    self.start_editing(now, inst);
                }
            });
        });
    }

    fn show_modal(&mut self, ctx: &egui::Context, now: Ts, inst: Instant) {
        let Some(modal) = &self.modal else { return };
        let session = &mut self.session;
        let (mut close, mut confirm, mut quit, mut next) = (false, false, false, None);
        let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
        let resp = egui::Modal::new(egui::Id::new("app_modal")).frame(frame).show(ctx, |ui| {
            ui.set_max_width(440.0);
            let title = |ui: &mut egui::Ui, t: &str| ui.label(RichText::new(t).family(serif()).size(20.0));
            match modal {
                Modal::Confirm { title: t, body, .. } => {
                    title(ui, t);
                    ui.label(body.as_str());
                    ui.horizontal(|ui| {
                        confirm = ui.button("Confirm").clicked();
                        close = ui.button("Cancel").clicked();
                    });
                }
                Modal::InactivityCountdown => {
                    let left = self.warn_deadline.map_or(0, |d| d.saturating_duration_since(inst).as_secs());
                    title(ui, "Still there?");
                    ui.label(format!("Editing stops in {left} s because there has been no activity."));
                    if ui.button("Stay").clicked() {
                        session.note_activity(inst);
                        close = true;
                    }
                }
                Modal::ExitFailed { error } => {
                    title(ui, "Couldn't save your changes");
                    ui.label(format!("Couldn't save your changes: {error}"));
                    ui.horizontal(|ui| {
                        if ui.button("Retry").clicked() {
                            match session.on_exit(now) {
                                Ok(()) => quit = true,
                                Err(error) => next = Some(Modal::ExitFailed { error }),
                            }
                        }
                        if ui.button("Quit anyway").clicked() {
                            next = Some(Modal::ExitRescued { rescued: session.abandon(now) });
                        }
                    });
                }
                Modal::ExitRescued { rescued } => {
                    title(ui, "Quitting without saving");
                    match rescued {
                        Some(p) => ui.label(format!("Your unsaved changes were copied to {}", p.display())),
                        None => ui.label("Your unsaved changes could not be rescued."),
                    };
                    quit = ui.button("Close").clicked();
                }
            }
        });
        let dismissable = matches!(modal, Modal::Confirm { .. } | Modal::InactivityCountdown);
        if dismissable && resp.should_close() {
            close = true;
            if matches!(modal, Modal::InactivityCountdown) {
                session.note_activity(inst);
            }
        }
        if confirm {
            if let Some(Modal::Confirm { action, .. }) = self.modal.take() {
                match action {}
            }
        } else if let Some(m) = next {
            self.modal = Some(m);
        } else if close || quit {
            self.modal = None;
        }
        if quit {
            self.quit(ctx);
        }
    }

    fn show_toasts(&self, ctx: &egui::Context, pal: &Palette) {
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for t in &self.toasts {
                    Frame::new()
                        .fill(pal.text)
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(14, 8))
                        .show(ui, |ui| ui.label(RichText::new(&t.text).color(pal.bg)));
                }
            });
    }
}

impl eframe::App for TrackerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_root(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::palette(self.prefs.theme).bg.to_normalized_gamma_f32()
    }

    /// Last resort: the close was not cleared in `show_root` (e.g. the
    /// process is shutting down). Save if possible, else write a rescue copy.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.allow_close {
            return;
        }
        let now = now_ts();
        if self.session.on_exit(now).is_err() {
            let _ = self.session.abandon(now);
        }
    }
}
