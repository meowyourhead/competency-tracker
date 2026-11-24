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
use crate::prefs::{save_prefs_to, Prefs, ProfileView, Theme};
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
use crate::ui::widgets::nav_button;
use crate::ui::widgets::{choice, icon_button, nav_item, presence, search_box, window_button};
use crate::update::{relaunch, UpdateState, Updater};

const RELOAD_TOAST_EVERY: Duration = Duration::from_secs(60);
/// Height of the strip holding the window buttons (the system title bar is off).
const TITLEBAR_H: f32 = 44.0;
/// How close to the window edge a press starts a resize.
const RESIZE_GRIP: f32 = 5.0;
/// Below this window width (in points) the sidebar folds to icons.
const NARROW_W: f32 = 1000.0;
/// The detail panel takes half the room beside the sidebar, within these.
const DETAIL_MIN_W: f32 = 380.0;
const DETAIL_MAX_W: f32 = 620.0;
/// The page's side margin; the title bar's search starts at the same line.
const PAGE_MARGIN: i8 = 40;
/// The sidebar with its labels: wide enough for the longest, and no wider.
const SIDEBAR_W: f32 = 148.0;
/// The widest the title bar's search gets.
const TITLE_SEARCH_W: f32 = 260.0;

/// What the user clicked in the title bar this frame.
#[derive(Default)]
struct TitleBarClicks {
    toggle_editing: bool,
    toggle_theme: bool,
}

/// The title bar's right-hand controls. From the right: close, maximise /
/// restore, minimise, a hairline, the theme switch, then the presence capsule
/// (who you are, Editing / Viewing, and the button that changes it). Returns
/// the clicks and the left edge of the controls.
fn title_bar_controls(
    ui: &mut egui::Ui,
    pal: &Palette,
    user: &str,
    editing: bool,
    can_toggle: bool,
    theme: Theme,
) -> (TitleBarClicks, f32) {
    let mut clicks = TitleBarClicks::default();
    let rect = ui.max_rect();
    let mut left_edge = rect.right();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let ctx = ui.ctx().clone();
            if window_button(ui, pal, icon::X, "Close window", true).clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            let (glyph, label) = if maximized {
                (icon::COPY_SIMPLE, "Restore down")
            } else {
                (icon::SQUARE, "Maximize")
            };
            if window_button(ui, pal, glyph, label, false).clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
            }
            if window_button(ui, pal, icon::MINUS, "Minimize", false).clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            // A hairline sets the app's own controls apart from the window's.
            ui.add_space(10.0);
            let (line, _) = ui.allocate_exact_size(egui::vec2(1.0, 18.0), Sense::hover());
            ui.painter().vline(
                line.center().x,
                line.y_range(),
                egui::Stroke::new(1.0, pal.border),
            );
            ui.add_space(8.0);
            let (theme_icon, theme_label) = match theme {
                Theme::Light => (icon::MOON, "Switch to dark mode"),
                Theme::Dark => (icon::SUN, "Switch to light mode"),
            };
            clicks.toggle_theme = icon_button(ui, pal, theme_icon, theme_label).clicked();
            ui.add_space(8.0);
            clicks.toggle_editing = presence(ui, pal, user, editing, can_toggle).clicked();
            ui.add_space(4.0);
            left_edge = ui.min_rect().left();
        });
    });
    (clicks, left_edge)
}

/// Moves the window when `resp` (an empty, draggable area) is dragged, and
/// toggles maximised on a double-click.
fn window_drag(ctx: &egui::Context, resp: &egui::Response) {
    if resp.double_clicked() {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// Without the system frame, the window resizes from a thin grip along its
/// edges: show the resize cursor there and hand a press to the OS.
fn resize_edges(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D};
    let (pos, pressed, screen, maximized) = ctx.input(|i| {
        (
            i.pointer.hover_pos(),
            i.pointer.primary_pressed(),
            i.viewport_rect(),
            i.viewport().maximized.unwrap_or(false),
        )
    });
    let Some(pos) = pos else { return };
    if maximized {
        return;
    }
    let g = RESIZE_GRIP;
    let (l, r) = (pos.x - screen.left() < g, screen.right() - pos.x < g);
    let (t, b) = (pos.y - screen.top() < g, screen.bottom() - pos.y < g);
    let (dir, cursor) = match (l, r, t, b) {
        (true, _, true, _) => (D::NorthWest, C::ResizeNorthWest),
        (_, true, true, _) => (D::NorthEast, C::ResizeNorthEast),
        (true, _, _, true) => (D::SouthWest, C::ResizeSouthWest),
        (_, true, _, true) => (D::SouthEast, C::ResizeSouthEast),
        (true, ..) => (D::West, C::ResizeWest),
        (_, true, ..) => (D::East, C::ResizeEast),
        (_, _, true, _) => (D::North, C::ResizeNorth),
        (.., true) => (D::South, C::ResizeSouth),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

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
            Page::Audit => "History",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::Summary => icon::CHART_PIE_SLICE,
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
    /// This computer's display scale (a zoom factor); Settings may change it.
    pub display_scale: &'a mut f32,
    /// Cards or table on a person's profile (remembered on this computer).
    pub profile_view: &'a mut ProfileView,
    /// Set to a person's id to open their profile on the Personnel page.
    pub open_profile: &'a mut Option<Id>,
    /// Set to a section's id to open it on the Sections page.
    pub open_section: &'a mut Option<Id>,
    /// Checks for and installs new versions (Settings → General).
    pub updater: &'a mut Updater,
}

/// Somewhere Back and Forward can return to.
#[derive(Debug, Clone, PartialEq)]
struct Place {
    page: Page,
    all_sections: bool,
    section: Option<Id>,
    profile: Option<Id>,
    settings: Option<settings::SettingsTab>,
    detail: Option<(Id, Id)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Back,
    Forward,
}

/// How many places Back remembers.
const HISTORY_MAX: usize = 100;

/// The places visited, as in a web browser.
#[derive(Default)]
struct History {
    back: Vec<Place>,
    forward: Vec<Place>,
    current: Option<Place>,
}

impl History {
    /// Notes where the app is at the end of a frame.
    fn visit(&mut self, p: Place) {
        match &self.current {
            Some(c) if *c == p => return,
            Some(c) => {
                self.back.push(c.clone());
                if self.back.len() > HISTORY_MAX {
                    self.back.remove(0);
                }
                self.forward.clear();
            }
            None => {}
        }
        self.current = Some(p);
    }

    fn can(&self, s: Step) -> bool {
        match s {
            Step::Back => !self.back.is_empty(),
            Step::Forward => !self.forward.is_empty(),
        }
    }

    /// Moves one step from `here`; returns the place to show.
    fn step(&mut self, s: Step, here: &Place) -> Option<Place> {
        let (from, to) = match s {
            Step::Back => (&mut self.back, &mut self.forward),
            Step::Forward => (&mut self.forward, &mut self.back),
        };
        let p = from.pop()?;
        to.push(here.clone());
        self.current = Some(p.clone());
        Some(p)
    }

    /// The place stepped to settled as `p` (say, what it showed is gone).
    fn settle(&mut self, p: Place) {
        self.current = Some(p);
    }
}

pub struct TrackerApp {
    session: Session,
    prefs: Prefs,
    prefs_path: Option<PathBuf>,
    page: Page,
    styled: bool,
    /// The window is too narrow (in points) for the full sidebar.
    narrow: bool,
    /// While narrow, the sidebar was opened anyway (not saved).
    sidebar_peek: bool,
    toasts: Vec<Toast>,
    modal: Option<Modal>,
    /// Ask the OS to flash the window (the inactivity prompt just opened).
    attention: bool,
    search: String,
    open_detail: Option<(Id, Id)>,
    history: History,
    /// Back or Forward was clicked this frame.
    step: Option<Step>,
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
    updater: Updater,
    /// "A new version is available" was shown.
    update_announced: bool,
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
        app.updater.check(Some(cc.egui_ctx.clone()));
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
            narrow: false,
            sidebar_peek: false,
            toasts: Vec::new(),
            modal: None,
            attention: false,
            search: String::new(),
            open_detail: None,
            history: History::default(),
            step: None,
            allow_close: false,
            last_reload_toast: None,
            summary: SummaryState::default(),
            sections: SectionsState::default(),
            personnel: PersonnelState::default(),
            audit: AuditState::default(),
            settings: SettingsState::default(),
            detail: DetailState::default(),
            updater: Updater::default(),
            update_announced: false,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn prefs(&self) -> &Prefs {
        &self.prefs
    }

    fn style(&mut self, ctx: &egui::Context) {
        theme::install_fonts(ctx);
        theme::apply(ctx, self.prefs.theme);
        ctx.set_zoom_factor(self.prefs.scale());
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
        if std::mem::take(&mut self.attention) {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        if !self.update_announced {
            if let UpdateState::Available(r) = self.updater.state() {
                self.update_announced = true;
                self.toasts.push(Toast::lasting(
                    format!(
                        "Version {} is available. Install it from Settings \u{2192} General.",
                        r.tag
                    ),
                    Duration::from_secs(12),
                ));
            }
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
        // Once the inactivity prompt is open, activity no longer counts: only
        // its "Continue editing" button ends the countdown.
        if ctx.input(|i| !i.events.is_empty()) {
            self.session.note_activity(inst);
        }
        match (
            self.session.warning_left(inst).is_some(),
            matches!(self.modal, Some(Modal::InactivityCountdown)),
        ) {
            // Opened while another dialog was up, or the event was missed.
            (true, false) if self.modal.is_none() => self.modal = Some(Modal::InactivityCountdown),
            // Editing ended some other way: the prompt has nothing to ask.
            (false, true) => self.modal = None,
            _ => {}
        }
        self.toasts.retain(|t| t.until > inst);
        let focus_search =
            self.modal.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K));
        let pal = theme::palette(self.prefs.theme);

        let here = self.history.current.clone();
        self.take_step(&ctx);
        let stepped = self.history.current != here;

        // The window's width in points: at a large display scale on a small
        // screen the sidebar folds to icons (it can still be opened for a
        // while) and the detail panel narrows, so the page keeps its room.
        let total_w = ui.max_rect().width();
        self.narrow = total_w < NARROW_W;
        let width = if self.compact_sidebar() {
            60.0
        } else {
            SIDEBAR_W
        };
        let detail_w = ((total_w - width) * 0.5).clamp(DETAIL_MIN_W, DETAIL_MAX_W);
        let scale_before = self.prefs.display_scale;
        let view_before = self.prefs.profile_view;
        Panel::left("sidebar")
            .exact_size(width)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::same(12)),
            )
            .show(ui, |ui| self.sidebar(ui, &pal));
        let clicks = Panel::top("titlebar")
            .exact_size(TITLEBAR_H)
            .resizable(false)
            .frame(Frame::new().fill(pal.bg))
            .show(ui, |ui| self.title_bar(ui, &pal, focus_search))
            .inner;
        if clicks.toggle_editing {
            self.toggle_editing(now, inst);
        }
        if clicks.toggle_theme {
            self.toggle_theme(&ctx);
        }
        let pal = theme::palette(self.prefs.theme);
        overlays::banner(ui, &pal, &self.session);
        let today = now.date_naive();
        let mut open_profile = None;
        let mut open_section = None;
        // Drawn on every page whenever a detail target is open; slides in and out.
        let mut detail_open = self.open_detail.is_some();
        Panel::right("detail")
            .exact_size(detail_w)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(pal.card)
                    .stroke(egui::Stroke::new(1.0, pal.border))
                    .inner_margin(Margin::symmetric(24, 20)),
            )
            .show_collapsible(ui, &mut detail_open, |ui| {
                let mut cx = Ctx {
                    session: &mut self.session,
                    pal: &pal,
                    today,
                    now,
                    toasts: &mut self.toasts,
                    open_detail: &mut self.open_detail,
                    display_scale: &mut self.prefs.display_scale,
                    profile_view: &mut self.prefs.profile_view,
                    open_profile: &mut open_profile,
                    open_section: &mut open_section,
                    updater: &mut self.updater,
                };
                detail::show(ui, &mut cx, &mut self.detail);
            });
        CentralPanel::default()
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin {
                left: PAGE_MARGIN,
                right: PAGE_MARGIN,
                top: 8,
                bottom: 28,
            }))
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    let mut cx = Ctx {
                        session: &mut self.session,
                        pal: &pal,
                        today,
                        now,
                        toasts: &mut self.toasts,
                        open_detail: &mut self.open_detail,
                        display_scale: &mut self.prefs.display_scale,
                        profile_view: &mut self.prefs.profile_view,
                        open_profile: &mut open_profile,
                        open_section: &mut open_section,
                        updater: &mut self.updater,
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
        if let Some(id) = open_profile {
            self.personnel.profile = Some(id);
            self.page = Page::Personnel;
        }
        if let Some(id) = open_section {
            // The whole section: filters left over from an earlier visit go.
            self.sections.clear_filters();
            self.navigate(SearchHit::Section(id));
        }
        if self.open_detail.is_none() {
            // Reopening the same pair later starts fresh.
            self.detail = DetailState::default();
        }
        let place = self.place();
        if stepped {
            self.history.settle(place);
        } else {
            self.history.visit(place);
        }
        if self.step.is_some() {
            ctx.request_repaint();
        }
        if self.prefs.display_scale != scale_before {
            // Takes effect from the next frame, laid out afresh at the new size.
            ctx.set_zoom_factor(self.prefs.scale());
            self.save_prefs();
        } else if self.prefs.profile_view != view_before {
            self.save_prefs();
        }
        self.show_modal(&ctx, &pal, now, inst);
        overlays::show_toasts(&ctx, &pal, &self.toasts);
        resize_edges(&ctx);
    }

    fn on_event(&mut self, ev: SessionEvent, inst: Instant) {
        match ev {
            SessionEvent::Saved => self.toasts.push(Toast::new("Saved")),
            // The banner shows `save_error` / the mode; nothing else to do.
            SessionEvent::SaveFailed(_) | SessionEvent::EditingAvailable => {}
            SessionEvent::InactivityWarning { .. } => {
                if self.modal.is_none() {
                    self.modal = Some(Modal::InactivityCountdown);
                }
                // Even when minimised: the window flashes in the taskbar.
                self.attention = true;
            }
            SessionEvent::LeftEditMode(reason) => {
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

    fn show_modal(&mut self, ctx: &egui::Context, pal: &Palette, now: Ts, inst: Instant) {
        let Some(modal) = &self.modal else { return };
        let countdown = self.session.warning_left(inst).map(|left| {
            (
                left,
                self.session.inactivity_grace(),
                self.session.inactivity_timeout(),
            )
        });
        let Some(choice) = overlays::show_modal(ctx, pal, modal, countdown) else {
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
                self.session.continue_editing(inst);
                self.modal = None;
            }
            ModalChoice::Relinquish => {
                self.modal = None;
                if let Err(e) = self.session.stop_editing(now) {
                    self.toasts.push(Toast::new(format!("Couldn't save: {e}")));
                }
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

    /// Icons only: as saved, or because the window is narrow (unless opened).
    fn compact_sidebar(&self) -> bool {
        if self.narrow {
            !self.sidebar_peek
        } else {
            self.prefs.sidebar_collapsed
        }
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool) {
        if self.narrow {
            // A narrow window's folding isn't the user's saved choice.
            self.sidebar_peek = !collapsed;
            return;
        }
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
                self.sections.all = false;
                self.sections.section = Some(id);
                self.sections.test = None;
                self.page = Page::Sections;
            }
            SearchHit::Test {
                section_id,
                test_id,
            } => {
                self.sections.all = false;
                self.sections.section = Some(section_id);
                self.sections.test = Some(test_id);
                self.page = Page::Sections;
            }
        }
    }

    /// Where the app is now, for Back and Forward.
    fn place(&self) -> Place {
        let sections = self.page == Page::Sections;
        Place {
            page: self.page,
            all_sections: sections && self.sections.all,
            section: self
                .sections
                .section
                .clone()
                .filter(|_| sections && !self.sections.all),
            profile: self
                .personnel
                .profile
                .clone()
                .filter(|_| self.page == Page::Personnel),
            settings: (self.page == Page::Settings).then_some(self.settings.tab),
            detail: self.open_detail.clone(),
        }
    }

    /// Goes to a place visited before. People and tests deleted since are
    /// skipped over by the pages themselves.
    fn go_to(&mut self, p: Place) {
        self.page = p.page;
        match p.page {
            Page::Sections => {
                self.sections.all = p.all_sections;
                if p.section.is_some() && p.section != self.sections.section {
                    self.sections.section = p.section;
                    self.sections.test = None;
                }
            }
            Page::Personnel => self.personnel.profile = p.profile,
            Page::Settings => {
                if let Some(t) = p.settings {
                    self.settings.tab = t;
                }
            }
            Page::Summary | Page::Audit => {}
        }
        let data = self.session.data();
        self.open_detail = p
            .detail
            .filter(|(s, t)| data.staff(s).is_some() && data.test(t).is_some());
    }

    /// Back or Forward: the buttons, Alt+Left / Alt+Right, or the mouse's
    /// side buttons. Not while a dialog is open.
    fn take_step(&mut self, ctx: &egui::Context) {
        let mut step = self.step.take();
        let dialog = self.modal.is_some() || ctx.memory(|m| m.top_modal_layer().is_some());
        if dialog {
            return;
        }
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::ALT, Key::ArrowLeft)
                || i.pointer.button_pressed(egui::PointerButton::Extra1)
            {
                step = Some(Step::Back);
            } else if i.consume_key(Modifiers::ALT, Key::ArrowRight)
                || i.pointer.button_pressed(egui::PointerButton::Extra2)
            {
                step = Some(Step::Forward);
            }
        });
        if let Some(p) = step.and_then(|s| self.history.step(s, &self.place())) {
            self.go_to(p);
            ctx.request_repaint();
        }
    }

    /// The draggable strip at the top (double-click maximises): back, forward
    /// and the app's name on the left, the search in the middle, and the
    /// presence capsule, theme switch and window buttons on the right.
    fn title_bar(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        focus_search: bool,
    ) -> TitleBarClicks {
        let rect = ui.max_rect();
        let drag = ui.interact(rect, ui.id().with("titlebar_drag"), Sense::click_and_drag());
        window_drag(ui.ctx(), &drag);
        let editing = self.session.is_editing();
        let can_toggle = editing || self.can_start_editing();
        let user = self.session.user().to_owned();
        let (clicks, controls_left) =
            title_bar_controls(ui, pal, &user, editing, can_toggle, self.prefs.theme);

        // Back and forward, then the app's name, lined up with the page below.
        let left = rect.left() + PAGE_MARGIN as f32 - 8.0;
        let nav = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(left + 68.0, rect.bottom()),
        );
        let builder = egui::UiBuilder::new()
            .max_rect(nav)
            .layout(Layout::left_to_right(Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (dir, glyph, label) in [
                (Step::Back, icon::ARROW_LEFT, "Back"),
                (Step::Forward, icon::ARROW_RIGHT, "Forward"),
            ] {
                let on = self.history.can(dir);
                let tip = if dir == Step::Back {
                    "Back (Alt+Left)"
                } else {
                    "Forward (Alt+Right)"
                };
                if nav_button(ui, pal, glyph, label, tip, on).clicked() {
                    self.step = Some(dir);
                }
            }
        });
        let mut free_from = nav.right();

        // The search sits at the window's centre, or as near as the rest
        // allows, shrinking (and then going) when there's little room.
        let name_w = ui
            .painter()
            .layout_no_wrap(
                "Competency Tracker".to_owned(),
                egui::FontId::new(17.0, serif()),
                pal.text,
            )
            .size();
        // The name gives way to the search when there's no room for both.
        let name_at = free_from + 12.0;
        if controls_left - 16.0 - (name_at + name_w.x + 20.0) >= 120.0 {
            let at = egui::Rect::from_min_size(
                egui::pos2(name_at, rect.center().y - name_w.y / 2.0),
                name_w,
            );
            let name = RichText::new("Competency Tracker")
                .family(serif())
                .size(17.0)
                .color(pal.text);
            // Hover-only, so the strip under it still drags the window.
            ui.put(at, egui::Label::new(name).selectable(false));
            free_from = at.right();
        }
        let lo = free_from + 20.0;
        let search_w = (controls_left - 16.0 - lo).min(TITLE_SEARCH_W);
        if search_w >= 120.0 {
            let centre = ui.ctx().input(|i| i.viewport_rect()).center().x;
            let x = (centre - search_w / 2.0).clamp(lo, controls_left - 16.0 - search_w);
            let area = egui::Rect::from_min_max(
                egui::pos2(x, rect.top()),
                egui::pos2(x + search_w, rect.bottom()),
            );
            let builder = egui::UiBuilder::new()
                .max_rect(area)
                .layout(Layout::left_to_right(Align::Center));
            // The field's own margins (10 each side) come out of its width.
            ui.scope_builder(builder, |ui| {
                self.search_field(ui, search_w - 20.0, focus_search)
            });
        }
        clicks
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let compact = self.compact_sidebar();
        // The header's empty space moves the window, like a title bar. Added
        // first so the collapse button on top of it still takes its clicks.
        let header = egui::Rect::from_min_size(
            ui.max_rect().min - egui::vec2(12.0, 12.0),
            egui::vec2(ui.max_rect().width() + 24.0, 44.0),
        );
        let drag = ui.interact(
            header,
            ui.id().with("sidebar_drag"),
            Sense::click_and_drag(),
        );
        window_drag(ui.ctx(), &drag);
        ui.horizontal(|ui| {
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
    }

    fn search_field(&mut self, ui: &mut egui::Ui, width: f32, focus: bool) {
        let resp = search_box(ui, &mut self.search, "Search  (Ctrl+K)", width);
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
        let pal = theme::palette(self.prefs.theme);
        Popup::from_response(&resp)
            .open(true)
            .width(resp.rect.width())
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if hits.is_empty() {
                    ui.label(RichText::new("No matches").weak());
                }
                for (hit, label) in hits {
                    let glyph = match hit {
                        SearchHit::Staff(_) => icon::USER,
                        SearchHit::Section(_) => icon::SQUARES_FOUR,
                        SearchHit::Test { .. } => icon::FLASK,
                    };
                    if choice(ui, &pal, false, &format!("{glyph}  {label}"), true).clicked() {
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
        if !self.allow_close {
            let now = now_ts();
            if self.session.on_exit(now).is_err() {
                let _ = self.session.abandon(now);
            }
        }
        // "Restart now" after an update: the lock is released by now, so the
        // new version can start editing straight away.
        if self.updater.restart_requested {
            if let Err(e) = relaunch() {
                crate::startup::note(&format!("restart after update failed: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::DataDir;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    /// A session whose inactivity prompt is already counting down (idle
    /// time 1 minute, so a tick a minute after opening starts the prompt).
    fn prompting_app(dir: &tempfile::TempDir) -> TrackerApp {
        let data_dir = DataDir {
            root: dir.path().join("CompetencyData"),
        };
        let start = Instant::now() - Duration::from_secs(61);
        let mut session = Session::open(data_dir, "tester", "pc", 1, now_ts(), start).unwrap();
        session
            .mutate(now_ts(), |d, _| d.settings.inactivity_minutes = 1)
            .unwrap();
        let ev = session.tick(now_ts(), Instant::now());
        assert!(ev
            .iter()
            .any(|e| matches!(e, SessionEvent::InactivityWarning { .. })));
        TrackerApp::new_for_test(session)
    }

    fn harness(app: TrackerApp) -> Harness<'static, TrackerApp> {
        Harness::builder()
            .with_size(egui::vec2(1280.0, 800.0))
            .build_ui_state(|ui, app: &mut TrackerApp| app.show_root(ui), app)
    }

    #[test]
    fn inactivity_prompt_ignores_activity_until_continue() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(prompting_app(&dir));
        h.run_steps(3);
        assert!(h.query_by_label("Are you still editing?").is_some());
        assert!(
            h.query_by_label_contains("15:00").is_some()
                || h.query_by_label_contains("14:59").is_some()
        );

        h.event(egui::Event::Text("x".into()));
        h.run_steps(3);
        assert!(
            h.query_by_label("Are you still editing?").is_some(),
            "typing doesn't end the countdown"
        );
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(h.query_by_label("Are you still editing?").is_some());

        h.get_by_label("Continue editing").click();
        h.run_steps(3);
        assert!(h.query_by_label("Are you still editing?").is_none());
        assert!(h.state().modal.is_none());
        assert!(h.state().session().warning_left(Instant::now()).is_none());
        assert!(h.state().session().is_editing());
    }

    #[test]
    fn inactivity_prompt_can_stop_editing() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(prompting_app(&dir));
        h.run_steps(3);
        h.get_by_label("Stop editing now").click();
        h.run_steps(3);
        assert!(!h.state().session().is_editing());
        assert!(h.query_by_label("Are you still editing?").is_none());
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
