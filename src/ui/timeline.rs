//! The competency timeline in the detail panel: every completion placed to
//! scale on a time axis, coloured by stage (Initial, the one-off stages after
//! it, and the repeating Annual), with today and the next due date, and a
//! stage key underneath that says when each stage was done.

use chrono::{Datelike, NaiveDate};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2};

use crate::dates::fmt_date;
use crate::model::Rule;
use crate::schedule::{classify, stage_label, DueInfo, DueStatus, PairState};
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::status_colors;

/// Where a stage sits in its rule; decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageKind {
    /// The first stage (Initial).
    First,
    /// A one-off stage after the first (Semi-annual).
    Interval,
    /// The last stage, when it repeats (Annual).
    Recurring,
}

pub(crate) fn stage_kind(rule: &Rule, idx: u32) -> StageKind {
    let n = rule.stages.len() as u32;
    let repeats = rule.stages.last().is_some_and(|s| s.repeats);
    if idx == 0 {
        StageKind::First
    } else if repeats && n > 0 && idx >= n - 1 {
        StageKind::Recurring
    } else {
        StageKind::Interval
    }
}

pub(crate) fn stage_color(pal: &Palette, kind: StageKind) -> Color32 {
    match kind {
        StageKind::First => pal.stage_first,
        StageKind::Interval => pal.stage_interval,
        StageKind::Recurring => pal.stage_recurring,
    }
}

/// A stage name as a small tinted chip in its stage colour.
pub(crate) fn stage_chip(ui: &mut Ui, pal: &Palette, rule: &Rule, idx: u32, text: &str) {
    let color = stage_color(pal, stage_kind(rule, idx));
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .corner_radius(255)
        .inner_margin(egui::Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).family(medium()).color(color));
        });
}

const TRACK_H: f32 = 128.0;
const INSET: f32 = 34.0;
const TIER: f32 = 17.0;

/// One labelled point on the axis.
struct Mark {
    x: f32,
    top: String,
    top_color: Color32,
    bottom: String,
    bottom_color: Color32,
    /// Higher keeps its label when labels collide.
    priority: i32,
}

#[allow(clippy::too_many_arguments)]
/// The track and its stage key. Draws nothing but a note when there is
/// nothing to place yet.
pub(crate) fn show(
    ui: &mut Ui,
    pal: &Palette,
    rule: &Rule,
    pair: &PairState,
    info: &DueInfo,
    today: NaiveDate,
    due_soon_days: i64,
    assigned: bool,
) {
    let mut done: Vec<_> = pair.records.clone();
    done.sort_by_key(|r| (r.completed_on, r.stage_index));
    let due = info.due_on.filter(|_| assigned);
    if done.is_empty() && due.is_none() {
        ui.label(
            RichText::new("Nothing completed yet. Completions appear here as they're recorded.")
                .color(pal.muted),
        );
        stage_key(ui, pal, rule, pair);
        return;
    }
    let mut start = done.first().map_or(today, |r| r.completed_on);
    let mut end = today;
    for d in done.iter().map(|r| r.completed_on).chain(due) {
        start = start.min(d);
        end = end.max(d);
    }
    if (end - start).num_days() < 120 {
        start -= chrono::Duration::days(60);
        end += chrono::Duration::days(60);
    }
    let span = (end - start).num_days().max(1) as f32;

    let width = ui.available_width().max(200.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, TRACK_H), Sense::hover());
    let line_y = rect.top() + TIER * 2.0 + 18.0;
    let x0 = rect.left() + INSET;
    let x1 = rect.right() - INSET;
    let x_of = |d: NaiveDate| x0 + (x1 - x0) * ((d - start).num_days() as f32 / span);
    let painter = ui.painter_at(rect.expand(2.0));

    // Year ticks with their labels along the bottom.
    let years_y = rect.bottom() - 8.0;
    let year_font = FontId::proportional(10.5);
    let step = {
        let years = (end.year() - start.year()).max(1) as f32;
        let per = (x1 - x0) / years;
        if per >= 44.0 {
            1
        } else if per >= 22.0 {
            2
        } else {
            5
        }
    };
    for y in (start.year() + 1)..=end.year() {
        if y % step != 0 {
            continue;
        }
        let Some(jan) = NaiveDate::from_ymd_opt(y, 1, 1) else {
            continue;
        };
        let x = x_of(jan);
        painter.vline(
            x,
            (line_y + 4.0)..=(years_y - 8.0),
            Stroke::new(1.0, pal.border),
        );
        painter.text(
            Pos2::new(x, years_y),
            Align2::CENTER_CENTER,
            y.to_string(),
            year_font.clone(),
            pal.muted,
        );
    }

    // The path so far, then the stretch to the next due date.
    let last = done.last().map(|r| r.completed_on);
    if let (Some(first), Some(last)) = (done.first(), last) {
        painter.hline(
            x_of(first.completed_on)..=x_of(last),
            line_y,
            Stroke::new(2.5, pal.muted.gamma_multiply(0.55)),
        );
    }
    let status = classify(due, today, due_soon_days);
    let (status_bg, status_fg) = status_colors(pal, status);
    if let Some(due) = due {
        let from = last.map_or(x0, x_of);
        painter.extend(Shape::dashed_line(
            &[Pos2::new(from, line_y), Pos2::new(x_of(due), line_y)],
            Stroke::new(2.0, status_fg.gamma_multiply(0.8)),
            5.0,
            4.0,
        ));
        if status == DueStatus::Overdue {
            // How far past due it is, as a band from the due date to today.
            let band =
                Rect::from_x_y_ranges(x_of(due)..=x_of(today), (line_y - 5.0)..=(line_y + 5.0));
            painter.rect_filled(band, 3, status_bg);
            painter.hline(band.x_range(), line_y, Stroke::new(2.0, status_fg));
        }
    }

    let mut marks = Vec::new();
    let n = done.len() as i32;
    for (i, r) in done.iter().enumerate() {
        let kind = stage_kind(rule, r.stage_index);
        let color = stage_color(pal, kind);
        let x = x_of(r.completed_on);
        let c = Pos2::new(x, line_y);
        painter.circle_filled(c, 8.5, pal.card);
        painter.circle_filled(c, 6.5, color);
        let label = if r.stage_label.is_empty() {
            stage_label(rule, r.stage_index)
        } else {
            r.stage_label.clone()
        };
        let hit = Rect::from_center_size(c, Vec2::splat(18.0));
        let id = ui.id().with(("timeline_mark", &r.id));
        ui.interact(hit, id, Sense::hover())
            .on_hover_text(format!("{label}\nCompleted {}", fmt_date(r.completed_on)));
        marks.push(Mark {
            x,
            top: label,
            top_color: color,
            bottom: r.completed_on.format("%b %Y").to_string(),
            bottom_color: pal.muted,
            priority: 10 + i as i32 - n,
        });
    }
    // Today: a short upright line through the axis.
    let tx = x_of(today);
    painter.vline(
        tx,
        (line_y - 9.0)..=(line_y + 9.0),
        Stroke::new(1.5, pal.text),
    );
    marks.push(Mark {
        x: tx,
        top: "Today".to_owned(),
        top_color: pal.text,
        bottom: String::new(),
        bottom_color: pal.muted,
        priority: 20,
    });
    if let Some(due) = due {
        let c = Pos2::new(x_of(due), line_y);
        painter.circle_filled(c, 7.0, pal.card);
        painter.circle_stroke(c, 6.0, Stroke::new(2.0, status_fg));
        let hit = Rect::from_center_size(c, Vec2::splat(18.0));
        ui.interact(hit, ui.id().with("timeline_due"), Sense::hover())
            .on_hover_text(format!("{} due {}", info.next_stage_label, fmt_date(due)));
        marks.push(Mark {
            x: c.x,
            top: format!("{} due", info.next_stage_label),
            top_color: status_fg,
            bottom: fmt_date(due),
            bottom_color: status_fg,
            priority: 30,
        });
    }
    place_labels(&painter, pal, rect, line_y, marks);
    ui.add_space(6.0);
    stage_key(ui, pal, rule, pair);
}

/// Puts each mark's two labels (above and below the axis) on the first of
/// two tiers where they don't collide, most important first; a mark with no
/// room keeps only its dot (its tooltip still says what it is).
fn place_labels(
    painter: &egui::Painter,
    pal: &Palette,
    rect: Rect,
    line_y: f32,
    mut marks: Vec<Mark>,
) {
    let top_font = FontId::new(12.0, medium());
    let bottom_font = FontId::proportional(11.5);
    marks.sort_by_key(|m| -m.priority);
    // Taken x-ranges per tier (0 nearest the axis).
    let mut taken: [Vec<(f32, f32)>; 2] = [Vec::new(), Vec::new()];
    let gap = 6.0;
    for m in marks {
        let top = painter.layout_no_wrap(m.top.clone(), top_font.clone(), m.top_color);
        let bottom = painter.layout_no_wrap(m.bottom.clone(), bottom_font.clone(), m.bottom_color);
        let w = top.size().x.max(bottom.size().x);
        // Keep the block inside the track.
        let cx = m.x.clamp(rect.left() + w / 2.0, rect.right() - w / 2.0);
        let span = (cx - w / 2.0 - gap, cx + w / 2.0 + gap);
        let Some(tier) =
            (0..2).find(|&t| taken[t].iter().all(|&(a, b)| span.1 <= a || span.0 >= b))
        else {
            continue;
        };
        taken[tier].push(span);
        let off = 11.0 + tier as f32 * TIER;
        if tier == 1 {
            // A leader line ties the outer label to its dot.
            painter.vline(
                m.x,
                (line_y - off + 2.0)..=(line_y - 9.0),
                Stroke::new(1.0, pal.border),
            );
        }
        // Each label sits on a patch of the card, so leader lines and
        // gridlines pass behind it.
        let put = |pos: Pos2, galley: std::sync::Arc<egui::Galley>, color: Color32| {
            let r = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(3.0, 0.0));
            painter.rect_filled(r, 3, pal.card);
            painter.galley(pos, galley, color);
        };
        put(
            Pos2::new(cx - top.size().x / 2.0, line_y - off - top.size().y),
            top,
            m.top_color,
        );
        if !m.bottom.is_empty() {
            put(
                Pos2::new(cx - bottom.size().x / 2.0, line_y + off - 2.0),
                bottom,
                m.bottom_color,
            );
        }
    }
}

/// One entry per stage of the rule: its colour, name, and when it was done.
fn stage_key(ui: &mut Ui, pal: &Palette, rule: &Rule, pair: &PairState) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(22.0, 8.0);
        for (i, stage) in rule.stages.iter().enumerate() {
            let i = i as u32;
            let kind = stage_kind(rule, i);
            let value = if kind == StageKind::Recurring {
                let mut reps: Vec<_> = pair
                    .records
                    .iter()
                    .filter(|r| r.stage_index >= i)
                    .map(|r| r.completed_on)
                    .collect();
                reps.sort();
                match (reps.len(), reps.last()) {
                    (0, _) | (_, None) => "Not yet".to_owned(),
                    (1, Some(d)) => fmt_date(*d),
                    (k, Some(d)) => format!("{k} done, last {}", fmt_date(*d)),
                }
            } else if let Some(r) = pair.records.iter().find(|r| r.stage_index == i) {
                fmt_date(r.completed_on)
            } else if pair.skipped.contains(&i) {
                // A stage that doesn't apply to this person or test.
                "N/A".to_owned()
            } else {
                "Not yet".to_owned()
            };
            let done = value != "Not yet" && value != "N/A";
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    let color = stage_color(pal, kind);
                    if done {
                        ui.painter().circle_filled(dot.center(), 4.5, color);
                    } else {
                        ui.painter()
                            .circle_stroke(dot.center(), 4.0, Stroke::new(1.5, color));
                    }
                    ui.label(RichText::new(&stage.label).size(12.5).color(pal.muted));
                });
                let color = if done { pal.text } else { pal.muted };
                ui.label(
                    RichText::new(value)
                        .family(medium())
                        .size(13.5)
                        .color(color),
                );
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_kinds_of_the_default_rule() {
        let r = Rule::default_global();
        assert_eq!(stage_kind(&r, 0), StageKind::First);
        assert_eq!(stage_kind(&r, 1), StageKind::Interval);
        assert_eq!(stage_kind(&r, 2), StageKind::Recurring);
        assert_eq!(stage_kind(&r, 9), StageKind::Recurring);
    }
}
