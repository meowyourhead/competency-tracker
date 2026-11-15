//! Settings → Sections & tests: sections in their configured order, each with
//! inline rename, ↑/↓, retire / restore and delete (only without records).
//! Expanding a section lists its test systems the same way, plus the
//! schedule rule each one uses ("Global" or a named rule).

use std::collections::{HashMap, HashSet};

use egui::text::{CCursor, CCursorRange};
use egui::{Align, Button, ComboBox, Frame, Key, Layout, Margin, RichText, TextEdit};
use egui_phosphor::regular as icon;

use super::{apply, card};
use crate::model::{Data, Id};
use crate::ops;
use crate::session::Session;
use crate::ui::app::Ctx;
use crate::ui::theme::{medium, Palette};
use crate::ui::widgets::icon_button;

#[derive(Default)]
pub(super) struct SectionsTab {
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
    AddSection,
    AddTest(Id),
}

/// One row's facts, for `item_row`.
struct Item<'a> {
    target: Target,
    name: &'a str,
    retired: bool,
    has_records: bool,
    first: bool,
    last: bool,
}

impl SectionsTab {
    fn refresh(&mut self, session: &Session) {
        let rev = session.revision();
        if !matches!(&self.with_records, Some((r, _)) if *r == rev) {
            let ids = session.data().records.iter().map(|r| r.test_id.clone()).collect();
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
            RichText::new("Retire a section or test to hide it but keep its history. Delete only works when nothing was recorded.")
                .color(pal.muted),
        );
        ui.add_space(6.0);
        let sections = data.sections_sorted();
        for (i, sec) in sections.iter().enumerate() {
            let with = &st.with_records.as_ref().expect("refreshed above").1;
            let has_records = sec.tests.iter().any(|t| with.contains(&t.id));
            let open = st.expanded.contains(&sec.id);
            let item = Item {
                target: Target::Section(sec.id.clone()),
                name: &sec.name,
                retired: sec.retired,
                has_records,
                first: i == 0,
                last: i + 1 == sections.len(),
            };
            let toggled = item_row(ui, pal, editing, st, &item, &mut act, |ui| {
                let glyph = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
                let verb = if open { "Hide" } else { "Show" };
                icon_button(ui, pal, glyph, &format!("{verb} {} tests", sec.name)).clicked()
            });
            if toggled && !st.expanded.remove(&sec.id) {
                st.expanded.insert(sec.id.clone());
            }
            if open {
                Frame::new().inner_margin(Margin { left: 40, right: 0, top: 0, bottom: 6 }).show(ui, |ui| {
                    tests(ui, pal, editing, data, st, &sec.id, &mut act);
                });
            }
            ui.separator();
        }
        ui.horizontal(|ui| {
            let field = ui.add_enabled(
                editing,
                TextEdit::singleline(&mut st.new_section).hint_text("New section name").desired_width(260.0),
            );
            let ready = editing && !st.new_section.trim().is_empty();
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.add_enabled(ready, Button::new(format!("{}  Add section", icon::PLUS))).clicked() || (ready && enter) {
                act = Some(Act::AddSection);
            }
        });
    });
    if let Some(a) = act {
        run(cx, st, a);
    }
}

/// The tests of one section, then the "Add test" field.
fn tests(ui: &mut egui::Ui, pal: &Palette, editing: bool, data: &Data, st: &mut SectionsTab, section_id: &str, act: &mut Option<Act>) {
    let Some(sec) = data.section(section_id) else { return };
    let mut tests: Vec<_> = sec.tests.iter().collect();
    tests.sort_by_key(|t| t.order);
    if tests.is_empty() {
        ui.label(RichText::new("No test systems yet.").color(pal.muted));
    }
    let named: Vec<(Id, String)> =
        data.rules.iter().filter(|r| r.id != data.default_rule_id).map(|r| (r.id.clone(), r.name.clone())).collect();
    for (i, t) in tests.iter().enumerate() {
        let has_records = st.with_records.as_ref().is_some_and(|(_, w)| w.contains(&t.id));
        let item = Item {
            target: Target::Test(t.id.clone()),
            name: &t.name,
            retired: t.retired,
            has_records,
            first: i == 0,
            last: i + 1 == tests.len(),
        };
        let current = t.rule_id.as_ref().filter(|r| named.iter().any(|(id, _)| id == *r)).cloned();
        let picked = item_row(ui, pal, editing, st, &item, act, |ui| rule_combo(ui, editing, &t.id, &t.name, &current, &named));
        if let Some(rule) = picked {
            *act = Some(Act::SetRule(t.id.clone(), rule));
        }
    }
    ui.horizontal(|ui| {
        let text = st.new_test.entry(section_id.to_owned()).or_default();
        let field = ui.add_enabled(editing, TextEdit::singleline(text).hint_text("New test system").desired_width(240.0));
        let ready = editing && !text.trim().is_empty();
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if ui.add_enabled(ready, Button::new(format!("{}  Add test", icon::PLUS))).clicked() || (ready && enter) {
            *act = Some(Act::AddTest(section_id.to_owned()));
        }
    });
}

/// "Global" or a named rule; returns the newly picked choice.
fn rule_combo(ui: &mut egui::Ui, editing: bool, test_id: &str, name: &str, current: &Option<Id>, named: &[(Id, String)]) -> Option<Option<Id>> {
    let shown = current.as_ref().and_then(|c| named.iter().find(|(id, _)| id == c)).map_or("Global", |(_, n)| n.as_str());
    let mut picked = None;
    ui.add_enabled_ui(editing, |ui| {
        ComboBox::from_id_salt(("test_rule", test_id)).selected_text(shown).width(150.0).show_ui(ui, |ui| {
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
    lead: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let is_section = matches!(item.target, Target::Section(_));
    let out = ui
        .horizontal(|ui| {
            // Sections lead with the expander; tests end with the rule dropdown.
            let mut lead = Some(lead);
            let mut out = None;
            if is_section {
                out = lead.take().map(|f| f(ui));
            }
            name_cell(ui, pal, editing, st, item, act);
            if item.retired {
                Frame::new().fill(pal.grey_bg).corner_radius(255).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
                    ui.label(RichText::new("Retired").size(11.5).color(pal.grey_fg));
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let delete = ui.add_enabled(editing && !item.has_records, Button::new("Delete"));
                let why = if item.has_records { "Has records \u{2014} retire instead" } else { "Start editing to make changes" };
                if delete.on_disabled_hover_text(why).clicked() {
                    *act = Some(Act::AskDelete(item.target.clone()));
                }
                let retire = if item.retired { "Restore" } else { "Retire" };
                if ui.add_enabled(editing, Button::new(retire)).clicked() {
                    *act = Some(Act::Retire(item.target.clone(), !item.retired));
                }
                let down = ui.add_enabled_ui(editing && !item.last, |ui| icon_button(ui, pal, icon::ARROW_DOWN, &format!("Move {} down", item.name)));
                if down.inner.clicked() {
                    *act = Some(Act::Move(item.target.clone(), 1));
                }
                let up = ui.add_enabled_ui(editing && !item.first, |ui| icon_button(ui, pal, icon::ARROW_UP, &format!("Move {} up", item.name)));
                if up.inner.clicked() {
                    *act = Some(Act::Move(item.target.clone(), -1));
                }
                if let Some(f) = lead.take() {
                    out = Some(f(ui));
                }
            });
            out.expect("lead drawn once")
        })
        .inner;
    if st.confirm_delete.as_ref() == Some(&item.target) {
        ui.horizontal(|ui| {
            let what = if is_section { "section" } else { "test" };
            ui.label(RichText::new(format!("Delete the {what} {}? This can't be undone.", item.name)).color(pal.red_fg));
            if ui.add_enabled(editing, Button::new(RichText::new("Delete").color(pal.red_fg))).clicked() {
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
fn name_cell(ui: &mut egui::Ui, pal: &Palette, editing: bool, st: &mut SectionsTab, item: &Item, act: &mut Option<Act>) {
    let font = if matches!(item.target, Target::Section(_)) { medium() } else { egui::FontFamily::Proportional };
    if let Some(r) = st.rename.as_mut().filter(|r| r.target == item.target) {
        let id = ui.make_persistent_id(("rename", &item.target));
        let resp = ui.add(TextEdit::singleline(&mut r.text).id(id).desired_width(240.0));
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
    let resp = ui.add_enabled(editing, button).on_hover_text("Click to rename");
    if resp.clicked() {
        *act = Some(Act::StartRename(item.target.clone(), item.name.to_owned()));
    }
}

fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::default(), CCursor::new(text.chars().count()))));
    state.store(ctx, id);
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
            st.rename = Some(Rename { target, text, fresh: true });
        }
        Act::CancelRename => st.rename = None,
        Act::Rename(target, text) => {
            st.rename = None;
            if current_name(cx.session.data(), &target).as_deref() == Some(text.trim()) {
                return;
            }
            match target {
                Target::Section(id) => apply(cx, "Section renamed", "Couldn't rename", |d, a| ops::rename_section(d, a, &id, &text)),
                Target::Test(id) => apply(cx, "Test renamed", "Couldn't rename", |d, a| ops::rename_test(d, a, &id, &text)),
            };
        }
        Act::Move(target, delta) => {
            match target {
                Target::Section(id) => apply(cx, "", "Couldn't move", |d, a| ops::move_section(d, a, &id, delta)),
                Target::Test(id) => apply(cx, "", "Couldn't move", |d, a| ops::move_test(d, a, &id, delta)),
            };
        }
        Act::Retire(target, retired) => {
            let done = if retired { "Retired" } else { "Restored" };
            match target {
                Target::Section(id) => apply(cx, done, "Couldn't update", |d, a| ops::set_section_retired(d, a, &id, retired)),
                Target::Test(id) => apply(cx, done, "Couldn't update", |d, a| ops::set_test_retired(d, a, &id, retired)),
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
                    apply(cx, "Section deleted", "Couldn't delete", |d, a| ops::delete_section(d, a, &id))
                }
                Target::Test(id) => apply(cx, "Test deleted", "Couldn't delete", |d, a| ops::delete_test(d, a, &id)),
            };
        }
        Act::SetRule(test_id, rule) => {
            apply(cx, "Schedule rule updated", "Couldn't change the rule", |d, a| ops::set_test_rule(d, a, &test_id, rule));
        }
        Act::AddSection => {
            let name = st.new_section.clone();
            if apply(cx, "Section added", "Couldn't add the section", |d, a| ops::add_section(d, a, &name)).is_some() {
                st.new_section.clear();
            }
        }
        Act::AddTest(section_id) => {
            let name = st.new_test.get(&section_id).cloned().unwrap_or_default();
            if apply(cx, "Test added", "Couldn't add the test", |d, a| ops::add_test(d, a, &section_id, &name)).is_some() {
                st.new_test.remove(&section_id);
            }
        }
    }
}
