//! Overlays drawn over the pages: the status banner, modal dialogs and toasts.
//! They only draw and report what the user chose; `app.rs` acts on it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align2, Frame, Margin, Panel, RichText};
use egui_phosphor::regular as icon;

use crate::session::{Mode, Session};
use crate::ui::theme::{serif, Palette};

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
    /// Closing found another user holding the lock; the edits went to `path`.
    ExitConflict { path: PathBuf },
}

/// What the user chose in the modal this frame.
pub(crate) enum ModalChoice {
    /// Dismiss the modal.
    Close,
    /// Run the confirmed action.
    Confirm,
    /// Keep editing (inactivity countdown).
    Stay,
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
/// error, corrupt files, then viewing / editing available. Returns true when
/// "Start editing" was clicked.
pub(crate) fn banner(ui: &mut egui::Ui, pal: &Palette, s: &Session) -> bool {
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
            Mode::Editing => return false,
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
    let mut start = false;
    Panel::top("banner").frame(Frame::new().fill(fill).inner_margin(Margin::symmetric(20, 8))).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).color(fg));
            let resp = ui.label(RichText::new(text).color(fg));
            if let Some(h) = hover {
                resp.on_hover_text(h);
            }
            start = start_button && ui.button("Start editing").clicked();
        });
    });
    start
}

/// Draws `modal`; returns the user's choice, if any. `warn_deadline` is when
/// the inactivity countdown ends.
pub(crate) fn show_modal(
    ctx: &egui::Context,
    modal: &Modal,
    warn_deadline: Option<Instant>,
    inst: Instant,
) -> Option<ModalChoice> {
    let mut choice = None;
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(24));
    let resp = egui::Modal::new(egui::Id::new("app_modal")).frame(frame).show(ctx, |ui| {
        ui.set_max_width(440.0);
        let title = |ui: &mut egui::Ui, t: &str| ui.label(RichText::new(t).family(serif()).size(20.0));
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
            Modal::InactivityCountdown => {
                let left = warn_deadline.map_or(0, |d| d.saturating_duration_since(inst).as_secs());
                title(ui, "Still there?");
                ui.label(format!("Editing stops in {left} s because there has been no activity."));
                if ui.button("Stay").clicked() {
                    choice = Some(ModalChoice::Stay);
                }
            }
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
                    Some(p) => ui.label(format!("Your unsaved changes were copied to {}", p.display())),
                    None => ui.label("Your unsaved changes could not be rescued."),
                };
                if ui.button("Close").clicked() {
                    choice = Some(ModalChoice::Quit);
                }
            }
            Modal::ExitConflict { path } => {
                title(ui, "Someone else is editing");
                ui.label(format!("Someone else took over editing. Your changes were saved to {}", path.display()));
                if ui.button("Close").clicked() {
                    choice = Some(ModalChoice::Quit);
                }
            }
        }
    });
    if choice.is_none() && resp.should_close() {
        choice = match modal {
            Modal::Confirm { .. } => Some(ModalChoice::Close),
            Modal::InactivityCountdown => Some(ModalChoice::Stay),
            _ => None,
        };
    }
    choice
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
            for t in toasts {
                Frame::new()
                    .fill(pal.text)
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| ui.label(RichText::new(&t.text).color(pal.bg)));
            }
        });
}
