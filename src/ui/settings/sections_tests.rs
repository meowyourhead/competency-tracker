//! Settings → Sections & tests: sections in their configured order, each with
//! a drag handle to reorder it, its colour, inline rename, retire / restore
//! and delete (only without records). Expanding a section lists its test
//! systems the same way (dragged within their section), plus the schedule
//! rule each one uses ("Global" or a named rule).

use std::collections::{HashMap, HashSet};

use egui::text::{CCursor, CCursorRange};
use egui::{
    Align, Button, Color32, ComboBox, CornerRadius, CursorIcon, DragAndDrop, Frame, Key, Layout,
    Margin, Popup, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Vec2,
};
use egui_phosphor::regular as icon;

use super::{apply, card};
use crate::model::{Data, Id, SECTION_COLORS};
use crate::ops;
use crate::session::Session;
use crate::ui::app::Ctx;
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::icon_button;

/// What a drag handle carries: the row, its place in its list, and the list
/// (the sections, or one section's tests) it may be dropped back into.
#[derive(Debug, Clone)]
struct DragRow {
    group: Id,
    index: usize,
    target: Target,
}

/// The drag group of the section list (test lists use their section's id).
const SECTIONS_GROUP: &str = "\u{0}sections";

#[derive(Default)]
pub(super) struct SectionsTab {
    /// A custom colour being picked: (section id, colour so far).
    color_draft: Option<(Id, Color32)>,
    rename: Option<Rename>,
    /// Section or test awaiting a delete confirmation.
    confirm_delete: Option<Target>,
    expanded: HashSet<Id>,
    new_section: String,
    /// New-test name being typed, per section.
    new_test: HashMap<Id, String>,
    /// Test ids with at least one record, for a data revision.
    with_records: Option<(u64, HashSet<Id>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Target {
    Section(Id),
    Test(Id),
}

struct Rename {
    target: Target,
    text: String,
    /// Focus and select the text on the first frame.
    fresh: bool,
}

/// What the user did this frame; applied once drawing is done.
enum Act {
    StartRename(Target, String),
    Rename(Target, String),
    CancelRename,
    Move(Target, i32),
    Retire(Target, bool),
    AskDelete(Target),
    CancelDelete,
    Delete(Target),
    SetRule(Id, Option<Id>),
    /// `None` goes back to the default colour.
    SetColor(Id, Option<u32>),
    AddSection,
    AddTest(Id),
}

/// One row's facts, for `item_row`.
struct Item<'a> {
    target: Target,
    name: &'a str,
    retired: bool,
    has_records: bool,
    /// Place in its list, and the list's drag group.
    index: usize,
    group: &'a str,
}

impl SectionsTab {
    fn refresh(&mut self, session: &Session) {
        let rev = session.revision();
        if !matches!(&self.with_records, Some((r, _)) if *r == rev) {
            let ids = session
                .data()
                .records
                .iter()
                .map(|r| r.test_id.clone())
                .collect();
            self.with_records = Some((rev, ids));
        }
    }
}

pub(super) fn show(ui: &mut egui::Ui, cx: &mut Ctx, st: &mut SectionsTab) {
    let pal = cx.pal;
    let editing = cx.session.is_editing();
    if !editing {
        st.rename = None;
        st.confirm_delete = None;
    }
    st.refresh(cx.session);
    let data = cx.session.data();
    let mut act = None;
    card(ui, pal, "Sections and test systems", |ui| {
        ui.label(
            RichText::new("Drag a row by its handle to reorder it. Retire a section or test to hide it but keep its history. Delete only works when nothing was recorded.")
                .color(pal.muted),
        );
        ui.add_space(6.0);
        let sections = data.sections_sorted();
        let mut blocks = Vec::with_capacity(sections.len());
        for (i, sec) in sections.iter().enumerate() {
            let top = ui.cursor().top();
            let with = &st.with_records.as_ref().expect("refreshed above").1;
            let has_records = sec.tests.iter().any(|t| with.contains(&t.id));
            let open = st.expanded.contains(&sec.id);
            let item = Item {
                target: Target::Section(sec.id.clone()),
                name: &sec.name,
                retired: sec.retired,
                has_records,
                index: i,
                group: SECTIONS_GROUP,
            };
            let color = rgb(sec.color_rgb(i));
            let custom = sec.color.is_some();
            let toggled = item_row(ui, pal, editing, st, &item, &mut act, |ui, st, act| {
                let glyph = if open {
                    icon::CARET_DOWN
                } else {
                    icon::CARET_RIGHT
                };
                let verb = if open { "Hide" } else { "Show" };
                let toggled =
                    icon_button(ui, pal, glyph, &format!("{verb} {} tests", sec.name)).clicked();
                color_swatch(ui, pal, editing, st, &sec.id, &sec.name, color, custom, act);
                toggled
            });
            if toggled && !st.expanded.remove(&sec.id) {
                st.expanded.insert(sec.id.clone());
            }
            if open {
                Frame::new()
                    .inner_margin(Margin {
                        left: 76,
                        right: 0,
                        top: 0,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        tests(ui, pal, editing, data, st, &sec.id, &mut act);
                    });
            }
            ui.separator();
            let rect = Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
            blocks.push(rect);
        }
        if let Some(a) = drop_target(ui, pal, SECTIONS_GROUP, &blocks) {
            act = Some(a);
        }
        ui.horizontal(|ui| {
            let field = ui.add_enabled(
                editing,
                TextEdit::singleline(&mut st.new_section)
                    .hint_text("New section name")
                    .desired_width(260.0),
            );
            let ready = editing && !st.new_section.trim().is_empty();
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui
                .add_enabled(ready, Button::new(format!("{}  Add section", icon::PLUS)))
                .clicked()
                || (ready && enter)
            {
                act = Some(Act::AddSection);
            }
        });
    });
    if let Some(a) = act {
        run(cx, st, a);
    }
}

/// The tests of one section, then the "Add test" field.
fn tests(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    data: &Data,
    st: &mut SectionsTab,
    section_id: &str,
    act: &mut Option<Act>,
) {
    let Some(sec) = data.section(section_id) else {
        return;
    };
    let mut tests: Vec<_> = sec.tests.iter().collect();
    tests.sort_by_key(|t| t.order);
    if tests.is_empty() {
        ui.label(RichText::new("No test systems yet.").color(pal.muted));
    }
    let named: Vec<(Id, String)> = data
        .rules
        .iter()
        .filter(|r| r.id != data.default_rule_id)
        .map(|r| (r.id.clone(), r.name.clone()))
        .collect();
    let mut blocks = Vec::with_capacity(tests.len());
    for (i, t) in tests.iter().enumerate() {
        let top = ui.cursor().top();
        let has_records = st
            .with_records
            .as_ref()
            .is_some_and(|(_, w)| w.contains(&t.id));
        let item = Item {
            target: Target::Test(t.id.clone()),
            name: &t.name,
            retired: t.retired,
            has_records,
            index: i,
            group: section_id,
        };
        let current = t
            .rule_id
            .as_ref()
            .filter(|r| named.iter().any(|(id, _)| id == *r))
            .cloned();
        let picked = item_row(ui, pal, editing, st, &item, act, |ui, _, _| {
            rule_combo(ui, editing, &t.id, &t.name, &current, &named)
        });
        if let Some(rule) = picked {
            *act = Some(Act::SetRule(t.id.clone(), rule));
        }
        // Half the row gap on each side, so the drop line sits between rows.
        let gap = ui.spacing().item_spacing.y / 2.0;
        blocks.push(Rect::from_x_y_ranges(
            ui.max_rect().x_range(),
            top - gap..=ui.cursor().top() - gap,
        ));
    }
    if let Some(a) = drop_target(ui, pal, section_id, &blocks) {
        *act = Some(a);
    }
    ui.horizontal(|ui| {
        let text = st.new_test.entry(section_id.to_owned()).or_default();
        let field = ui.add_enabled(
            editing,
            TextEdit::singleline(text)
                .hint_text("New test system")
                .desired_width(240.0),
        );
        let ready = editing && !text.trim().is_empty();
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if ui
            .add_enabled(ready, Button::new(format!("{}  Add test", icon::PLUS)))
            .clicked()
            || (ready && enter)
        {
            *act = Some(Act::AddTest(section_id.to_owned()));
        }
    });
}

/// "Global" or a named rule; returns the newly picked choice.
fn rule_combo(
    ui: &mut egui::Ui,
    editing: bool,
    test_id: &str,
    name: &str,
    current: &Option<Id>,
    named: &[(Id, String)],
) -> Option<Option<Id>> {
    let shown = current
        .as_ref()
        .and_then(|c| named.iter().find(|(id, _)| id == c))
        .map_or("Global", |(_, n)| n.as_str());
    let mut picked = None;
    ui.add_enabled_ui(editing, |ui| {
        ComboBox::from_id_salt(("test_rule", test_id))
            .selected_text(shown)
            .width(150.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "Global").clicked() && current.is_some() {
                    picked = Some(None);
                }
                for (id, n) in named {
                    let selected = current.as_ref() == Some(id);
                    if ui.selectable_label(selected, n).clicked() && !selected {
                        picked = Some(Some(id.clone()));
                    }
                }
            })
            .response
            .on_hover_text(format!("Schedule rule for {name}"));
    });
    picked
}

/// One section or test row. `lead` draws the leading widget (expander or rule
/// dropdown) and its result is returned. Row actions go into `act`.
fn item_row<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    st: &mut SectionsTab,
    item: &Item,
    act: &mut Option<Act>,
    lead: impl FnOnce(&mut egui::Ui, &mut SectionsTab, &mut Option<Act>) -> R,
) -> R {
    let is_section = matches!(item.target, Target::Section(_));
    // The row being dragged stays in place, tinted, so it's clear what moves.
    let dragging = DragAndDrop::payload::<DragRow>(ui.ctx())
        .is_some_and(|p| p.group == item.group && p.index == item.index);
    if dragging {
        let h = ui.spacing().interact_size.y + 4.0;
        let r = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), h));
        ui.painter().rect_filled(
            r.expand2(Vec2::new(4.0, 1.0)),
            8,
            pal.accent.gamma_multiply(0.10),
        );
    }
    let out = ui
        .horizontal(|ui| {
            drag_handle(ui, pal, editing, item);
            // Sections lead with the expander; tests end with the rule dropdown.
            let mut lead = Some(lead);
            let mut out = None;
            if is_section {
                out = lead.take().map(|f| f(ui, st, act));
            }
            name_cell(ui, pal, editing, st, item, act);
            if item.retired {
                Frame::new()
                    .fill(pal.grey_bg)
                    .corner_radius(255)
                    .inner_margin(Margin::symmetric(8, 2))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Retired").size(11.5).color(pal.grey_fg));
                    });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let delete = ui.add_enabled(editing && !item.has_records, Button::new("Delete"));
                let why = if item.has_records {
                    "Has records \u{2014} retire instead"
                } else {
                    "Start editing to make changes"
                };
                if delete.on_disabled_hover_text(why).clicked() {
                    *act = Some(Act::AskDelete(item.target.clone()));
                }
                let retire = if item.retired { "Restore" } else { "Retire" };
                if ui.add_enabled(editing, Button::new(retire)).clicked() {
                    *act = Some(Act::Retire(item.target.clone(), !item.retired));
                }
                if let Some(f) = lead.take() {
                    out = Some(f(ui, st, act));
                }
            });
            out.expect("lead drawn once")
        })
        .inner;
    if st.confirm_delete.as_ref() == Some(&item.target) {
        ui.horizontal(|ui| {
            let what = if is_section { "section" } else { "test" };
            ui.label(
                RichText::new(format!(
                    "Delete the {what} {}? This can't be undone.",
                    item.name
                ))
                .color(pal.red_fg),
            );
            if ui
                .add_enabled(
                    editing,
                    Button::new(RichText::new("Delete").color(pal.red_fg)),
                )
                .clicked()
            {
                *act = Some(Act::Delete(item.target.clone()));
            }
            if ui.button("Cancel").clicked() {
                *act = Some(Act::CancelDelete);
            }
        });
    }
    out
}

/// The name as a click-to-rename button, or the rename field
/// (Enter or clicking away saves, Esc cancels).
fn name_cell(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    st: &mut SectionsTab,
    item: &Item,
    act: &mut Option<Act>,
) {
    let font = if matches!(item.target, Target::Section(_)) {
        medium()
    } else {
        egui::FontFamily::Proportional
    };
    if let Some(r) = st.rename.as_mut().filter(|r| r.target == item.target) {
        let id = ui.make_persistent_id(("rename", &item.target));
        let resp = ui.add(
            TextEdit::singleline(&mut r.text)
                .id(id)
                .desired_width(240.0),
        );
        if r.fresh {
            r.fresh = false;
            resp.request_focus();
            select_all(ui.ctx(), id, &r.text);
        } else if resp.lost_focus() {
            *act = Some(if ui.input(|i| i.key_pressed(Key::Escape)) {
                Act::CancelRename
            } else {
                Act::Rename(item.target.clone(), r.text.clone())
            });
        }
        return;
    }
    let color = if item.retired { pal.muted } else { pal.text };
    let button = Button::new(RichText::new(item.name).family(font).color(color)).frame(false);
    let resp = ui
        .add_enabled(editing, button)
        .on_hover_text("Click to rename");
    if resp.clicked() {
        *act = Some(Act::StartRename(item.target.clone(), item.name.to_owned()));
    }
}

fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::default(),
        CCursor::new(text.chars().count()),
    )));
    state.store(ctx, id);
}

pub(crate) fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn to_u32(c: Color32) -> u32 {
    (u32::from(c.r()) << 16) | (u32::from(c.g()) << 8) | u32::from(c.b())
}

/// Six dots to grab a row by. Dragging it lifts the row: a chip with its
/// name follows the pointer until it is dropped on the same list.
fn drag_handle(ui: &mut egui::Ui, pal: &Palette, editing: bool, item: &Item) {
    let sense = if editing {
        Sense::drag()
    } else {
        Sense::hover()
    };
    let (_, rect) = ui.allocate_space(Vec2::new(18.0, 30.0));
    let id = ui.make_persistent_id(("drag_row", &item.target));
    let resp = ui.interact(rect, id, sense);
    let active = resp.dragged() || resp.hovered();
    let color = match (editing, active) {
        (false, _) => pal.muted.gamma_multiply(0.4),
        (true, true) => pal.text,
        (true, false) => pal.muted,
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon::DOTS_SIX_VERTICAL,
        egui::FontId::proportional(17.0),
        color,
    );
    let label = format!("Drag to reorder {}", item.name);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, editing, &label));
    if !editing {
        return;
    }
    if resp.drag_started() {
        DragAndDrop::set_payload(
            ui.ctx(),
            DragRow {
                group: item.group.to_owned(),
                index: item.index,
                target: item.target.clone(),
            },
        );
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let Some(pos) = ui.ctx().pointer_interact_pos() {
            drag_chip(ui.ctx(), pal, item.name, pos);
        }
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
}

/// The floating chip under the pointer while a row is dragged.
fn drag_chip(ctx: &egui::Context, pal: &Palette, name: &str, pos: egui::Pos2) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("drag_row_chip"),
    ));
    let galley = painter.layout_no_wrap(
        format!("{}  {name}", icon::DOTS_SIX_VERTICAL),
        egui::FontId::new(14.0, medium()),
        pal.text,
    );
    let rect = Rect::from_min_size(
        pos + Vec2::new(14.0, -galley.size().y / 2.0 - 8.0),
        galley.size() + Vec2::new(24.0, 16.0),
    );
    painter.add(
        egui::epaint::Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(40),
        }
        .as_shape(rect, CornerRadius::same(8)),
    );
    painter.rect(
        rect,
        CornerRadius::same(8),
        pal.card,
        Stroke::new(1.0, pal.accent),
        StrokeKind::Inside,
    );
    painter.galley(rect.min + Vec2::new(12.0, 8.0), galley, pal.text);
}

/// While a row of `group` is dragged over the list whose rows span `blocks`:
/// draws the line where it would land, and on release returns the move.
/// Released anywhere else, the drag is dropped with no change.
fn drop_target(ui: &mut egui::Ui, pal: &Palette, group: &str, blocks: &[Rect]) -> Option<Act> {
    let payload = DragAndDrop::payload::<DragRow>(ui.ctx())?;
    if payload.group != group || blocks.is_empty() {
        return None;
    }
    ui.ctx().request_repaint();
    let pos = ui.ctx().pointer_interact_pos()?;
    let span = blocks
        .iter()
        .fold(Rect::NOTHING, |a, b| a.union(*b))
        .expand2(Vec2::new(24.0, 18.0));
    if !span.contains(pos) {
        return None;
    }
    let slot = blocks.iter().filter(|b| b.center().y < pos.y).count();
    let from = payload.index;
    let moves = slot != from && slot != from + 1;
    if moves {
        let y = if slot == 0 {
            blocks[0].top()
        } else {
            blocks[slot - 1].bottom()
        };
        let x = blocks[0].x_range();
        let p = ui.painter();
        p.hline(x, y, Stroke::new(2.0, pal.accent));
        p.circle_stroke(egui::pos2(x.min, y), 3.5, Stroke::new(2.0, pal.accent));
    }
    if !ui.input(|i| i.pointer.any_released()) {
        return None;
    }
    DragAndDrop::clear_payload(ui.ctx());
    if !moves {
        return None;
    }
    let to = if slot > from { slot - 1 } else { slot };
    Some(Act::Move(payload.target.clone(), to as i32 - from as i32))
}

/// The section's colour dot. Clicking it offers the preset colours, a custom
/// colour picker (applied with "Use this colour") and the default.
#[allow(clippy::too_many_arguments)]
fn color_swatch(
    ui: &mut egui::Ui,
    pal: &Palette,
    editing: bool,
    st: &mut SectionsTab,
    section_id: &str,
    name: &str,
    color: Color32,
    custom: bool,
    act: &mut Option<Act>,
) {
    let sense = if editing {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), sense);
    let p = ui.painter();
    if resp.hovered() && editing {
        p.rect_filled(rect, CornerRadius::same(8), pal.border);
    }
    p.circle_filled(rect.center(), 8.0, color);
    p.circle_stroke(
        rect.center(),
        8.0,
        Stroke::new(1.0, pal.text.gamma_multiply(0.15)),
    );
    let label = format!("Colour of {name}");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, editing, &label));
    let resp = resp.on_hover_text(&label);
    if !editing {
        return;
    }
    Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(232.0);
            ui.label(RichText::new(&label).family(medium()).color(pal.text));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(6.0);
                for (i, c) in SECTION_COLORS.iter().enumerate() {
                    let swatch = rgb(*c);
                    let (r, s) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    let picked = swatch == color;
                    ui.painter().circle_filled(r.center(), 10.0, swatch);
                    if picked || s.hovered() {
                        ui.painter().circle_stroke(
                            r.center(),
                            10.5,
                            Stroke::new(2.0, pal.text.gamma_multiply(0.7)),
                        );
                    }
                    let n = format!("Preset colour {}", i + 1);
                    s.widget_info(|| {
                        egui::WidgetInfo::selected(egui::WidgetType::Button, true, picked, &n)
                    });
                    if s.clicked() {
                        *act = Some(Act::SetColor(section_id.to_owned(), Some(*c)));
                        ui.close();
                    }
                }
            });
            ui.add_space(6.0);
            ui.separator();
            let draft = match &mut st.color_draft {
                Some((id, c)) if id == section_id => c,
                _ => &mut st.color_draft.insert((section_id.to_owned(), color)).1,
            };
            egui::widgets::color_picker::color_picker_color32(
                ui,
                draft,
                egui::widgets::color_picker::Alpha::Opaque,
            );
            let chosen = *draft;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(chosen != color, Button::new("Use this colour"))
                    .clicked()
                {
                    *act = Some(Act::SetColor(section_id.to_owned(), Some(to_u32(chosen))));
                    ui.close();
                }
                if ui.add_enabled(custom, Button::new("Use default")).clicked() {
                    *act = Some(Act::SetColor(section_id.to_owned(), None));
                    ui.close();
                }
            });
        });
}

fn current_name(data: &Data, target: &Target) -> Option<String> {
    match target {
        Target::Section(id) => data.section(id).map(|s| s.name.clone()),
        Target::Test(id) => data.test(id).map(|(_, t)| t.name.clone()),
    }
}

fn run(cx: &mut Ctx, st: &mut SectionsTab, act: Act) {
    match act {
        Act::StartRename(target, text) => {
            st.confirm_delete = None;
            st.rename = Some(Rename {
                target,
                text,
                fresh: true,
            });
        }
        Act::CancelRename => st.rename = None,
        Act::Rename(target, text) => {
            st.rename = None;
            if current_name(cx.session.data(), &target).as_deref() == Some(text.trim()) {
                return;
            }
            match target {
                Target::Section(id) => apply(cx, "Section renamed", "Couldn't rename", |d, a| {
                    ops::rename_section(d, a, &id, &text)
                }),
                Target::Test(id) => apply(cx, "Test renamed", "Couldn't rename", |d, a| {
                    ops::rename_test(d, a, &id, &text)
                }),
            };
        }
        Act::Move(target, delta) => {
            match target {
                Target::Section(id) => apply(cx, "", "Couldn't move", |d, a| {
                    ops::move_section(d, a, &id, delta)
                }),
                Target::Test(id) => apply(cx, "", "Couldn't move", |d, a| {
                    ops::move_test(d, a, &id, delta)
                }),
            };
        }
        Act::Retire(target, retired) => {
            let done = if retired { "Retired" } else { "Restored" };
            match target {
                Target::Section(id) => apply(cx, done, "Couldn't update", |d, a| {
                    ops::set_section_retired(d, a, &id, retired)
                }),
                Target::Test(id) => apply(cx, done, "Couldn't update", |d, a| {
                    ops::set_test_retired(d, a, &id, retired)
                }),
            };
        }
        Act::AskDelete(target) => {
            st.rename = None;
            st.confirm_delete = Some(target);
        }
        Act::CancelDelete => st.confirm_delete = None,
        Act::Delete(target) => {
            st.confirm_delete = None;
            match target {
                Target::Section(id) => {
                    st.expanded.remove(&id);
                    apply(cx, "Section deleted", "Couldn't delete", |d, a| {
                        ops::delete_section(d, a, &id)
                    })
                }
                Target::Test(id) => apply(cx, "Test deleted", "Couldn't delete", |d, a| {
                    ops::delete_test(d, a, &id)
                }),
            };
        }
        Act::SetColor(id, color) => {
            st.color_draft = None;
            apply(
                cx,
                "Colour updated",
                "Couldn't change the colour",
                |d, a| ops::set_section_color(d, a, &id, color),
            );
        }
        Act::SetRule(test_id, rule) => {
            apply(
                cx,
                "Schedule rule updated",
                "Couldn't change the rule",
                |d, a| ops::set_test_rule(d, a, &test_id, rule),
            );
        }
        Act::AddSection => {
            let name = st.new_section.clone();
            if apply(cx, "Section added", "Couldn't add the section", |d, a| {
                ops::add_section(d, a, &name)
            })
            .is_some()
            {
                st.new_section.clear();
            }
        }
        Act::AddTest(section_id) => {
            let name = st.new_test.get(&section_id).cloned().unwrap_or_default();
            if apply(cx, "Test added", "Couldn't add the test", |d, a| {
                ops::add_test(d, a, &section_id, &name)
            })
            .is_some()
            {
                st.new_test.remove(&section_id);
            }
        }
    }
}
