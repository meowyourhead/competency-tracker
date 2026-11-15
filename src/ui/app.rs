//! The application shell: per-frame session tick, sidebar, page routing and
//! safe close handling. The banner, modals and toasts are in `overlays.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use egui::{
    Align, CentralPanel, Frame, Key, Layout, Margin, Modifiers, Panel, Popup, RichText, ScrollArea,
    Sense, ViewportCommand,
};
use egui_phosphor::regular as icon;

use crate::dates::now_ts;
use crate::lock::Acquire;
use crate::model::{Id, Ts};
use crate::prefs::{save_prefs_to, Prefs, Theme};
use crate::query::{search, SearchHit};
use crate::session::{LeaveReason, Session, SessionEvent};
use crate::ui::audit::{self, AuditState};
use crate::ui::detail::{self, DetailState};
use crate::ui::overlays::{self, ModalChoice};
pub use crate::ui::overlays::{Modal, PendingAction, Toast};
use crate::ui::personnel::{self, PersonnelState};
use crate::ui::sections::{self, SectionsState};
use crate::ui::settings::{self, SettingsState};
use crate::ui::summary::{self, SummaryState};
use crate::ui::theme::{self, serif, Palette};
use crate::ui::widgets::{icon_button, nav_item, search_box};

const RELOAD_TOAST_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Summary,
    Sections,
    Personnel,
    Audit,
    Settings,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::Summary,
        Page::Sections,
        Page::Personnel,
        Page::Audit,
        Page::Settings,
    ];

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
    /// When "Updated with the latest changes" was last shown.
    last_reload_toast: Option<Instant>,
    summary: SummaryState,
    sections: SectionsState,
    personnel: PersonnelState,
    audit: AuditState,
    settings: SettingsState,
    detail: DetailState,
}

impl TrackerApp {
    pub fn new(
        cc: &eframe::CreationContext,
        session: Session,
        prefs: Prefs,
        prefs_path: Option<PathBuf>,
    ) -> Self {
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
            last_reload_toast: None,
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

    /// One full frame, logic then drawing, as eframe runs it when the window
    /// is visible (used by tests).
    pub fn show_root(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.logic_pass(&ctx);
        self.draw(ui);
    }

    /// Ticks the session (once per frame) and handles a close request. eframe
    /// runs this before every ui pass and also, without one, while the window
    /// is minimised or hidden; the repaint request keeps it running then.
    /// Paints nothing: events only queue toasts and modals for the next pass.
    fn logic_pass(&mut self, ctx: &egui::Context) {
        let (now, inst) = (now_ts(), Instant::now());
        for ev in self.session.tick(now, inst) {
            self.on_event(ev, inst);
        }
        self.handle_close(ctx, now, inst);
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// Draws one frame into the root `ui`.
    fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            // New fonts only take effect from the next pass: draw nothing yet.
            self.style(&ctx);
            ctx.request_repaint();
            return;
        }
        let (now, inst) = (now_ts(), Instant::now());
        // Input events are only fresh in a ui pass (a logic-only pass while
        // hidden sees the last shown frame's), so activity is noted here.
        if ctx.input(|i| !i.events.is_empty()) {
            self.session.note_activity(inst);
            // Activity ends any countdown at once, so it never goes stale.
            self.warn_deadline = None;
            if matches!(self.modal, Some(Modal::InactivityCountdown)) {
                self.modal = None;
            }
        }
        self.toasts.retain(|t| t.until > inst);
        let focus_search =
            self.modal.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K));
        if focus_search && self.prefs.sidebar_collapsed {
            self.set_sidebar_collapsed(false);
        }
        let pal = theme::palette(self.prefs.theme);

        let width = if self.prefs.sidebar_collapsed {
            60.0
        } else {
            264.0
        };
        Panel::left("sidebar")
            .exact_size(width)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::same(12)),
            )
            .show(ui, |ui| self.sidebar(ui, &pal, now, inst, focus_search));
        if overlays::banner(ui, &pal, &self.session) {
            self.start_editing(now, inst);
        }
        let today = now.date_naive();
        // Drawn on every page whenever a detail target is open; slides in and out.
        let mut detail_open = self.open_detail.is_some();
        Panel::right("detail")
            .exact_size(420.0)
            .resizable(false)
            .frame(Frame::new().fill(pal.card).inner_margin(Margin::same(20)))
            .show_collapsible(ui, &mut detail_open, |ui| {
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
        CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(pal.bg)
                    .inner_margin(Margin::symmetric(40, 28)),
            )
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
        if self.open_detail.is_none() {
            // Reopening the same pair later starts fresh.
            self.detail = DetailState::default();
        }
        self.show_modal(&ctx, now, inst);
        overlays::show_toasts(&ctx, &pal, &self.toasts);
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
            SessionEvent::Reloaded => {
                // A busy editor saves every few seconds: say so at most once a minute.
                if self
                    .last_reload_toast
                    .is_none_or(|t| inst.saturating_duration_since(t) >= RELOAD_TOAST_EVERY)
                {
                    self.last_reload_toast = Some(inst);
                    self.toasts
                        .push(Toast::new("Updated with the latest changes"));
                }
            }
            SessionEvent::Conflict(path) => self.toasts.push(Toast::lasting(
                format!("Your unsaved changes were kept in {}", path.display()),
                Duration::from_secs(10),
            )),
            SessionEvent::TookOverStale(prev) => self.toasts.push(Toast::new(format!(
                "Took over editing from {} (their session had stopped)",
                prev.user
            ))),
        }
    }

    /// eframe cannot cancel a close from `on_exit`, so save here: a failed
    /// save cancels the close and asks Retry / Quit anyway.
    fn handle_close(&mut self, ctx: &egui::Context, now: Ts, inst: Instant) {
        if self.allow_close || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.finish_exit(now, inst) {
            self.allow_close = true;
        } else {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
    }

    /// Saves and releases for closing. Returns false (with a modal set) when
    /// the close must wait: the save failed, or the lock was lost and the
    /// edits went to the conflicts folder (the user is told where first).
    fn finish_exit(&mut self, now: Ts, inst: Instant) -> bool {
        if let Err(error) = self.session.on_exit(now) {
            self.modal = Some(Modal::ExitFailed { error });
            return false;
        }
        let mut conflict = None;
        for ev in self.session.take_events() {
            match ev {
                SessionEvent::Conflict(path) => conflict = Some(path),
                ev => self.on_event(ev, inst),
            }
        }
        match conflict {
            Some(path) => {
                self.modal = Some(Modal::ExitConflict { path });
                false
            }
            None => true,
        }
    }

    fn show_modal(&mut self, ctx: &egui::Context, now: Ts, inst: Instant) {
        let Some(modal) = &self.modal else { return };
        let Some(choice) = overlays::show_modal(ctx, modal, self.warn_deadline, inst) else {
            return;
        };
        match choice {
            ModalChoice::Close => self.modal = None,
            ModalChoice::Confirm => {
                if let Some(Modal::Confirm { action, .. }) = self.modal.take() {
                    match action {}
                }
            }
            ModalChoice::Stay => {
                self.session.note_activity(inst);
                self.warn_deadline = None;
                self.modal = None;
            }
            ModalChoice::RetryExit => {
                if self.finish_exit(now, inst) {
                    self.modal = None;
                    self.quit(ctx);
                }
            }
            ModalChoice::QuitAnyway => {
                self.modal = Some(Modal::ExitRescued {
                    rescued: self.session.abandon(now),
                })
            }
            ModalChoice::Quit => {
                self.modal = None;
                self.quit(ctx);
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
                self.toasts
                    .push(Toast::new(format!("Couldn't save preferences: {e}")));
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
            SearchHit::Test {
                section_id,
                test_id,
            } => {
                self.sections.section = Some(section_id);
                self.sections.test = Some(test_id);
                self.page = Page::Sections;
            }
        }
    }

    fn sidebar(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        now: Ts,
        inst: Instant,
        focus_search: bool,
    ) {
        let compact = self.prefs.sidebar_collapsed;
        ui.horizontal(|ui| {
            if !compact {
                ui.label(
                    RichText::new("Competency Tracker")
                        .family(serif())
                        .size(19.0)
                        .color(pal.text),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if compact {
                    "Expand sidebar"
                } else {
                    "Collapse sidebar"
                };
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
            if nav_item(
                ui,
                pal,
                page.icon(),
                page.label(),
                self.page == page,
                compact,
            )
            .clicked()
            {
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
            let edit_label = if editing {
                "Stop editing"
            } else {
                "Start editing"
            };
            let can_toggle = editing || self.can_start_editing();
            if compact {
                if icon_button(ui, pal, theme_icon, theme_label).clicked() {
                    self.toggle_theme(&ctx);
                }
                let glyph = if editing {
                    icon::PENCIL_SIMPLE
                } else {
                    icon::EYE
                };
                let resp = ui
                    .add_enabled_ui(can_toggle, |ui| icon_button(ui, pal, glyph, edit_label))
                    .inner;
                if resp.clicked() {
                    self.toggle_editing(now, inst);
                }
                return;
            }
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(
                    dot.center(),
                    4.0,
                    if editing { pal.accent } else { pal.muted },
                );
                let status = if editing { "Editing" } else { "Viewing" };
                ui.label(
                    RichText::new(format!("{} \u{b7} {status}", self.session.user()))
                        .color(pal.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, pal, theme_icon, theme_label).clicked() {
                        self.toggle_theme(&ctx);
                    }
                });
            });
            let button =
                egui::Button::new(edit_label).min_size(egui::vec2(ui.available_width(), 30.0));
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
        Popup::from_response(&resp)
            .open(true)
            .width(resp.rect.width())
            .show(|ui| {
                if hits.is_empty() {
                    ui.label(RichText::new("No matches").weak());
                }
                for (hit, label) in hits {
                    let glyph = match hit {
                        SearchHit::Staff(_) => icon::USER,
                        SearchHit::Section(_) => icon::SQUARES_FOUR,
                        SearchHit::Test { .. } => icon::FLASK,
                    };
                    if ui
                        .selectable_label(false, format!("{glyph}  {label}"))
                        .clicked()
                    {
                        picked = Some(hit);
                    }
                }
            });
        if let Some(hit) = picked {
            self.navigate(hit);
            self.search.clear();
        }
    }
}

impl eframe::App for TrackerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.logic_pass(ctx);
    }

    /// `logic` already ran for this frame.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::palette(self.prefs.theme)
            .bg
            .to_normalized_gamma_f32()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::DataDir;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    #[test]
    fn countdown_closes_on_activity() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = DataDir {
            root: dir.path().join("CompetencyData"),
        };
        let session = Session::open(data_dir, "tester", "pc", 1, now_ts(), Instant::now()).unwrap();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1280.0, 800.0))
            .build_ui_state(
                |ui, app: &mut TrackerApp| app.show_root(ui),
                TrackerApp::new_for_test(session),
            );
        h.state_mut().warn_deadline = Some(Instant::now() + Duration::from_secs(40));
        h.state_mut().modal = Some(Modal::InactivityCountdown);
        h.run();
        assert!(h.query_by_label("Still there?").is_some());

        h.event(egui::Event::Text("x".into()));
        h.run();
        assert!(
            h.query_by_label("Still there?").is_none(),
            "typing ends the countdown"
        );
        assert!(h.state().modal.is_none());
        assert!(h.state().warn_deadline.is_none());
    }

    fn toast_texts(app: &TrackerApp) -> Vec<&str> {
        app.toasts.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn logic_ticks_the_session_without_a_ui_pass() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = DataDir {
            root: dir.path().join("CompetencyData"),
        };
        data_dir.ensure().unwrap();
        // A stale lock: opening takes it over and queues `TookOverStale` for the next tick.
        let old = now_ts() - chrono::Duration::minutes(10);
        let prev = crate::lock::LockInfo {
            user: "agnes".into(),
            machine: "PC2".into(),
            pid: 9,
            acquired_at: old,
            heartbeat_at: old,
        };
        std::fs::write(data_dir.lock_path(), serde_json::to_vec(&prev).unwrap()).unwrap();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(data_dir.lock_path())
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(600))
            .unwrap();
        drop(f);
        let session = Session::open(data_dir, "tester", "pc", 1, now_ts(), Instant::now()).unwrap();
        assert!(session.is_editing());
        let mut app = TrackerApp::new_for_test(session);
        // What eframe calls while the window is minimised: logic only, no ui pass.
        let ctx = egui::Context::default();
        eframe::App::logic(&mut app, &ctx, &mut eframe::Frame::_new_kittest());
        assert!(
            toast_texts(&app)
                .iter()
                .any(|t| t.starts_with("Took over editing from agnes")),
            "{:?}",
            toast_texts(&app)
        );
    }

    #[test]
    fn reload_toast_at_most_once_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = DataDir {
            root: dir.path().join("CompetencyData"),
        };
        let session = Session::open(data_dir, "tester", "pc", 1, now_ts(), Instant::now()).unwrap();
        let mut app = TrackerApp::new_for_test(session);
        let t = Instant::now();
        let count = |app: &TrackerApp| {
            toast_texts(app)
                .iter()
                .filter(|s| **s == "Updated with the latest changes")
                .count()
        };
        app.on_event(SessionEvent::Reloaded, t);
        app.on_event(SessionEvent::Reloaded, t + Duration::from_secs(10));
        app.on_event(SessionEvent::Reloaded, t + Duration::from_secs(59));
        assert_eq!(count(&app), 1);
        app.on_event(SessionEvent::Reloaded, t + Duration::from_secs(60));
        assert_eq!(count(&app), 2);
    }
}
