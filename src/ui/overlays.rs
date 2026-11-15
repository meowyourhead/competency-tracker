//! Overlays drawn over the pages: the status banner, modal dialogs and toasts.
//! They only draw and report what the user chose; `app.rs` acts on it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Frame, Margin, Panel, RichText, Stroke};
use egui_phosphor::regular as icon;

use crate::session::{Mode, Session};
use crate::ui::theme::{medium, serif, Palette};

/// Widest a toast gets before its text wraps.
const TOAST_MAX_W: f32 = 460.0;

/// A short confirmation shown bottom-centre.
pub struct Toast {
    pub(crate) text: String,
    pub(crate) until: Instant,
}

impl Toast {
    pub fn new(text: impl Into<String>) -> Toast {
        Toast::lasting(text, Duration::from_secs(3))
    }

    pub fn lasting(text: impl Into<String>, d: Duration) -> Toast {
        Toast {
            text: text.into(),
            until: Instant::now() + d,
        }
    }
}

/// An action that runs once the user confirms it. Pages add variants as they
/// need confirmations (Tasks 12–16).
pub enum PendingAction {}

pub enum Modal {
    Confirm {
        title: String,
        body: String,
        action: PendingAction,
    },
    InactivityCountdown,
    /// Saving on close failed: offer Retry or Quit anyway.
    ExitFailed {
        error: String,
    },
    /// "Quit anyway" ran; `rescued` is the rescue copy, if one was written.
    ExitRescued {
        rescued: Option<PathBuf>,
    },
    /// Closing found another user holding the lock; the edits went to `path`.
    ExitConflict {
        path: PathBuf,
    },
}

/// What the user chose in the modal this frame.
pub(crate) enum ModalChoice {
    /// Dismiss the modal.
    Close,
    /// Run the confirmed action.
    Confirm,
    /// Keep editing (inactivity countdown).
    Stay,
    /// Stop editing now (inactivity countdown).
    Relinquish,
    RetryExit,
    QuitAnyway,
    /// Close the window for real.
    Quit,
}

enum Tone {
    Error,
    Warn,
    Info,
}

/// The single most important banner, if any: load error, read-only, save
/// error, corrupt files, then viewing / editing available. Starting to edit
/// is the title bar's job, so the banner only informs.
pub(crate) fn banner(ui: &mut egui::Ui, pal: &Palette, s: &Session) {
    let mut hover = None;
    let (tone, text) = if let Some(e) = s.load_error() {
        (Tone::Error, format!("Can't read the data folder: {e}"))
    } else if s.read_only_reason().is_some() {
        (
            Tone::Warn,
            "Read-only: data saved by a newer version".to_owned(),
        )
    } else if let Some(e) = s.save_error() {
        hover = Some(e.to_owned());
        (Tone::Error, "Not saved \u{2014} retrying".to_owned())
    } else if !s.corrupt_files().is_empty() {
        let names: Vec<String> = s
            .corrupt_files()
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        (
            Tone::Warn,
            format!("Skipped unreadable snapshot files: {}", names.join(", ")),
        )
    } else {
        match s.mode() {
            Mode::Editing => return,
            Mode::Viewing { holder: Some(h) } => (
                Tone::Info,
                format!(
                    "Viewing \u{2014} {} is editing (since {})",
                    h.user,
                    h.acquired_at.format("%H:%M")
                ),
            ),
            Mode::Viewing { holder: None } => (Tone::Info, "Editing is available".to_owned()),
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
    Panel::top("banner")
        .frame(
            Frame::new()
                .fill(fill)
                .inner_margin(Margin::symmetric(20, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph).color(fg));
                let resp = ui.label(RichText::new(text).color(fg));
                if let Some(h) = hover {
                    resp.on_hover_text(h);
                }
            });
        });
}

/// Draws `modal`; returns the user's choice, if any. `countdown` is (time
/// left, the countdown's full length, the idle time that opened it) while
/// the inactivity prompt is counting down.
pub(crate) fn show_modal(
    ctx: &egui::Context,
    pal: &Palette,
    modal: &Modal,
    countdown: Option<(Duration, Duration, Duration)>,
) -> Option<ModalChoice> {
    if let Modal::InactivityCountdown = modal {
        return inactivity_prompt(ctx, pal, countdown);
    }
    let mut choice = None;
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let resp = egui::Modal::new(egui::Id::new("app_modal"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_max_width(440.0);
            let title =
                |ui: &mut egui::Ui, t: &str| ui.label(RichText::new(t).family(serif()).size(20.0));
            match modal {
                Modal::Confirm { title: t, body, .. } => {
                    title(ui, t);
                    ui.label(body.as_str());
                    ui.horizontal(|ui| {
                        if ui.button("Confirm").clicked() {
                            choice = Some(ModalChoice::Confirm);
                        }
                        if ui.button("Cancel").clicked() {
                            choice = Some(ModalChoice::Close);
                        }
                    });
                }
                Modal::InactivityCountdown => unreachable!("drawn by inactivity_prompt"),
                Modal::ExitFailed { error } => {
                    title(ui, "Couldn't save your changes");
                    ui.label(format!("Couldn't save your changes: {error}"));
                    ui.horizontal(|ui| {
                        if ui.button("Retry").clicked() {
                            choice = Some(ModalChoice::RetryExit);
                        }
                        if ui.button("Quit anyway").clicked() {
                            choice = Some(ModalChoice::QuitAnyway);
                        }
                    });
                }
                Modal::ExitRescued { rescued } => {
                    title(ui, "Quitting without saving");
                    match rescued {
                        Some(p) => ui.label(format!(
                            "Your unsaved changes were copied to {}",
                            p.display()
                        )),
                        None => ui.label("Your unsaved changes could not be rescued."),
                    };
                    if ui.button("Close").clicked() {
                        choice = Some(ModalChoice::Quit);
                    }
                }
                Modal::ExitConflict { path } => {
                    title(ui, "Someone else is editing");
                    ui.label(format!(
                        "Someone else took over editing. Your changes were saved to {}",
                        path.display()
                    ));
                    if ui.button("Close").clicked() {
                        choice = Some(ModalChoice::Quit);
                    }
                }
            }
        });
    if choice.is_none() && resp.should_close() {
        choice = match modal {
            Modal::Confirm { .. } => Some(ModalChoice::Close),
            _ => None,
        };
    }
    choice
}

/// "Are you still editing?": a countdown ring with the time left, over a
/// near-opaque veil of the page colour that hides the app behind it (egui
/// can't blur, so it frosts instead). Escape and clicks outside do nothing;
/// only its two buttons answer it.
fn inactivity_prompt(
    ctx: &egui::Context,
    pal: &Palette,
    countdown: Option<(Duration, Duration, Duration)>,
) -> Option<ModalChoice> {
    let (left, total, idle) = countdown?;
    let mut choice = None;
    let veil = pal.bg.gamma_multiply(0.94);
    let frame = Frame::window(&ctx.global_style())
        .inner_margin(Margin::symmetric(36, 32))
        .corner_radius(16);
    egui::Modal::new(egui::Id::new("inactivity_prompt"))
        .frame(frame)
        .backdrop_color(veil)
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.vertical_centered(|ui| {
                countdown_ring(ui, pal, left, total);
                ui.add_space(18.0);
                ui.label(
                    RichText::new("Are you still editing?")
                        .family(serif())
                        .size(22.0)
                        .color(pal.text),
                );
                ui.add_space(6.0);
                let mins = idle.as_secs().div_ceil(60);
                let s = if mins == 1 { "" } else { "s" };
                ui.label(
                    RichText::new(format!(
                        "Nothing has happened for {mins} minute{s}. When the timer runs out, \
                         your changes are saved and editing stops so someone else can edit."
                    ))
                    .color(pal.muted),
                );
                ui.add_space(20.0);
                let wide = egui::vec2(ui.available_width(), 36.0);
                let primary = egui::Button::new(
                    RichText::new("Continue editing")
                        .family(medium())
                        .color(Color32::WHITE),
                )
                .fill(pal.accent)
                .min_size(wide);
                if ui.add(primary).clicked() {
                    choice = Some(ModalChoice::Stay);
                }
                ui.add_space(2.0);
                let quiet = egui::Button::new(RichText::new("Stop editing now").color(pal.text))
                    .frame(false)
                    .min_size(wide);
                if ui.add(quiet).clicked() {
                    choice = Some(ModalChoice::Relinquish);
                }
            });
        });
    // The countdown has to keep moving even with no input.
    ctx.request_repaint_after(Duration::from_millis(250));
    choice
}

/// A large ring that empties clockwise as the countdown runs, with the time
/// left in the middle. It turns red for the last minute.
fn countdown_ring(ui: &mut egui::Ui, pal: &Palette, left: Duration, total: Duration) {
    const D: f32 = 132.0;
    const W: f32 = 5.0;
    let frac = (left.as_secs_f32() / total.as_secs_f32().max(1.0)).clamp(0.0, 1.0);
    let urgent = left.as_secs() < 60;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(D, D), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let r = D / 2.0 - W;
    p.circle_stroke(c, r, Stroke::new(W, pal.border));
    let color = if urgent { pal.red_fg } else { pal.accent };
    if frac > 0.001 {
        let steps = (96.0 * frac).ceil().max(2.0) as usize;
        let start = -std::f32::consts::FRAC_PI_2;
        let sweep = std::f32::consts::TAU * frac;
        let points = (0..=steps)
            .map(|i| c + r * egui::Vec2::angled(start + sweep * i as f32 / steps as f32))
            .collect();
        p.add(egui::Shape::line(points, Stroke::new(W, color)));
    }
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    let clock = format!("{}:{:02}", secs / 60, secs % 60);
    p.text(
        c - egui::vec2(0.0, 6.0),
        Align2::CENTER_CENTER,
        &clock,
        egui::FontId::new(32.0, serif()),
        if urgent { pal.red_fg } else { pal.text },
    );
    p.text(
        c + egui::vec2(0.0, 22.0),
        Align2::CENTER_CENTER,
        "left",
        egui::FontId::proportional(12.0),
        pal.muted,
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            true,
            format!("{clock} left before editing stops"),
        )
    });
}

pub(crate) fn show_toasts(ctx: &egui::Context, pal: &Palette, toasts: &[Toast]) {
    if toasts.is_empty() {
        return;
    }
    egui::Area::new(egui::Id::new("toasts"))
        .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            // An area sizes itself from last frame's content, so a label left
            // to wrap at the area's width breaks a new toast's words onto
            // separate rows. Lay each toast out at a fixed width instead: one
            // line when it fits, wrapped at TOAST_MAX_W when it doesn't.
            let max_w = (ctx.content_rect().width() - 64.0).clamp(160.0, TOAST_MAX_W);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                for t in toasts {
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let one_line = ui
                        .painter()
                        .layout_no_wrap(t.text.clone(), font.clone(), pal.bg)
                        .size()
                        .x;
                    let width = one_line.min(max_w).ceil();
                    let galley = ui.painter().layout(t.text.clone(), font, pal.bg, width);
                    Frame::new()
                        .fill(pal.text)
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(16, 10))
                        .show(ui, |ui| {
                            let (rect, resp) =
                                ui.allocate_exact_size(galley.size(), egui::Sense::hover());
                            ui.painter().galley(rect.min, galley, pal.bg);
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &t.text)
                            });
                        });
                }
            });
        });
}
