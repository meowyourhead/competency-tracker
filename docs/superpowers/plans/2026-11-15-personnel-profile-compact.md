# Compact Personnel Profile + Section Colours Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sprawling per-person profile with a compact page (status strip, next-up line, Cards/Table toggle, last-done dates) and show each section's colour wherever a section is named.

**Architecture:** The profile moves out of `personnel.rs` into a new `src/ui/profile.rs`, split into a pure part (rows, counts, next-up, filtering — unit tested) and drawing. Section colour helpers live in `widgets.rs` and are reused by every page. The chosen view is a `Prefs` field reached through `Ctx`.

**Tech Stack:** Rust, egui (+ egui_phosphor icons), egui_kittest smoke tests in `tests/ui_smoke.rs`.

**Spec:** `docs/superpowers/specs/2026-11-15-personnel-profile-compact-design.md`

## Global Constraints

- Colour treatment: soft tint fill (`color.gamma_multiply(0.14)`) + solid dot; text stays `pal.text`. Matches the Sections page; must read in light and dark.
- Section colour = `Section::color_rgb(position)` where `position` is the index in `data.sections_sorted()` (retired included), exactly as `sections.rs::combine` does.
- Status strip counts only `AssignState::Assigned` tests; labels: "Overdue", "Due soon", "Up to date", "Not done" (mapping `DueStatus::Overdue/DueSoon/OnTrack/NotStarted`).
- Cards grid: min card width 320 pt, max 3 columns.
- View choice persists in `prefs.json`; status filter does not persist.
- History rows stay fixed height (`ROW_H = 44.0`).
- After all tasks: `cargo build --release` and copy `target\release\competency-tracker.exe` to the project root.

## Review Focus

- Person with only N/A tests: strip shows all zeros, no next-up line, both views still list the N/A tests (muted) — test in Task 2.
- Status filter on, then view toggled: filter still applies in the other view — test in Task 5.
- Assigned test never completed: "Never done" in cards, "—" in table, counted as Not done, excluded from next-up (no due date) — test in Task 2.
- Audit entry whose `section_id` refers to a deleted section: row draws without a chip, no panic — test in Task 6.
- Narrow window (≤ 700 pt content): cards drop to 1–2 columns, no horizontal overflow — test in Task 4 (harness at 900×800).

---

### Task 1: Section colour helpers

**Files:**
- Modify: `src/ui/widgets.rs` (add helpers + `#[cfg(test)] mod tests` if absent)
- Modify: `src/ui/sections.rs:176-179,225-227` (use the shared `rgb`/lookup instead of its local `rgb`)

**Interfaces:**
- Produces:
  - `pub fn rgb(c: u32) -> Color32`
  - `pub fn section_color(data: &Data, section_id: &str) -> Option<Color32>` — `None` if the id is unknown.
  - `pub fn section_chip(ui: &mut Ui, pal: &Palette, name: &str, color: Color32) -> egui::Response` — pill: 6 pt dot + name at 11.5 pt, tint fill, `widget_info` labeled with `name`.
  - `pub fn section_dot(ui: &mut Ui, color: Color32)` — 7 pt filled circle, allocated inline.

- [ ] **Step 1: Write failing unit tests** in `widgets.rs`:
  - `section_color_by_position`: `Data::new_empty("t", ts)`; `section_color(&d, &d.sections_sorted()[1].id) == Some(rgb(SECTION_COLORS[1]))`.
  - `section_color_custom_and_unknown`: set `d.sections[0].color = Some("#112233")` → `Some(rgb(0x112233))`; `section_color(&d, "nope") == None`.
- [ ] **Step 2:** `cargo test --lib widgets` → FAIL (not defined).
- [ ] **Step 3:** Implement the four helpers; switch `sections.rs` to `widgets::rgb`.
- [ ] **Step 4:** `cargo test --lib widgets` → PASS; `cargo test` → all pass.
- [ ] **Step 5:** Commit `feat: shared section colour helpers`.

### Task 2: Profile data (pure)

**Files:**
- Create: `src/ui/profile.rs` (register `mod profile;` in `src/ui/mod.rs`)

**Interfaces:**
- Consumes: `section_color` (Task 1), `Index::build`, `next_due`, `classify` (as in `personnel.rs::summaries`).
- Produces:
  ```rust
  pub(crate) struct ProfileRow {
      pub section_id: Id, pub section_name: String, pub color: Color32,
      pub test_id: Id, pub test_name: String,
      /// None = N/A.
      pub due: Option<(DueInfo, DueStatus, bool /*delayed*/)>,
      pub last_done: Option<NaiveDate>, // DueInfo::last_completed_on; None for N/A
  }
  pub(crate) fn rows(data: &Data, staff: &Staff, today: NaiveDate) -> Vec<ProfileRow>; // sections_sorted (non-retired), tests by order, non-retired, any assignment
  pub(crate) fn counts(rows: &[ProfileRow]) -> [usize; 4]; // [Overdue, DueSoon, OnTrack, NotStarted]
  pub(crate) fn next_up(rows: &[ProfileRow]) -> Option<&ProfileRow>; // earliest Some(due_on) among assigned
  pub(crate) fn filtered(rows: &[ProfileRow], f: Option<DueStatus>) -> Vec<&ProfileRow>; // None = all; Some = assigned with that status only
  ```

- [ ] **Step 1: Write failing tests** in `profile.rs` `mod tests` (build `Data` by hand like `query.rs` tests):
  - `rows_follow_section_and_test_order` — two sections, tests out of order → rows ordered by section, then `order`.
  - `counts_only_assigned` — one overdue, one never done, one N/A → `[1,0,0,1]`.
  - `next_up_skips_undated_and_na` — never-done + N/A + due-in-10-days → the due-in-10-days row; only N/A → `None`.
  - `never_done_has_no_last_done` — `last_done == None`, status `NotStarted`.
  - `filter_hides_na` — `filtered(rows, Some(Overdue))` excludes N/A; `filtered(rows, None)` keeps it.
- [ ] **Step 2:** `cargo test --lib profile` → FAIL.
- [ ] **Step 3:** Implement the four functions.
- [ ] **Step 4:** `cargo test --lib profile` → PASS.
- [ ] **Step 5:** Commit `feat: profile rows, counts and next-up`.

### Task 3: Remembered view choice

**Files:**
- Modify: `src/prefs.rs` (enum + field + default)
- Modify: `src/ui/app.rs:194-205` (Ctx field), `:405-414`, `:426-435` (pass it), `:453-457` (save on change)

**Interfaces:**
- Produces: `#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)] #[serde(rename_all = "lowercase")] pub enum ProfileView { #[default] Cards, Table }`; `Prefs::profile_view: ProfileView`; `Ctx::profile_view: &'a mut ProfileView`.

- [ ] **Step 1: Failing test** in `prefs.rs`: `old_prefs_default_to_cards` — `load_prefs_from` a file containing `{"theme":"dark"}` → `profile_view == ProfileView::Cards`; round-trip a `Table` value through `save_prefs_to`/`load_prefs_from`.
- [ ] **Step 2:** `cargo test --lib prefs` → FAIL.
- [ ] **Step 3:** Add enum/field; plumb `Ctx`; in `app.rs` record `view_before` next to `scale_before` and call `self.save_prefs()` when it changed.
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5:** Commit `feat: remember profile view in prefs`.

### Task 4: Profile header + Cards view

**Files:**
- Modify: `src/ui/profile.rs` (drawing), `src/ui/personnel.rs:1053-1170` (delete `profile()`; call `profile::show`)
- Test: `tests/ui_smoke.rs`

**Interfaces:**
- Consumes: Tasks 1–3; `due_pill`, `status_colors`, `segmented`, `heading`, `StaffForm::edit`.
- Produces: `pub(crate) struct ProfileUi { pub filter: Option<DueStatus> }` (Default), held as `PersonnelState::profile_ui`, reset when `PersonnelState::profile` changes; `pub(crate) fn show(ui, cx: &mut Ctx, staff_id: &Id, st: &mut ProfileUi) -> ProfileOut` where `ProfileOut { pub back: bool, pub edit: bool }`.

Layout (top to bottom): back link; name + Edit (unchanged); meta line; one row with the four status chips (each: status-coloured dot, label, count; `widget_info` `selected` with label `"{label} {count}"`; zero counts drawn with `pal.muted`; click toggles `st.filter`) and the `Cards | Table` `segmented` right-aligned; next-up line `"Next up: {test} · [section chip] · {stage} — {in N days | due today | overdue by N days}"` as a clickable row opening `cx.open_detail`.

Cards: `cols = clamp(floor((avail + gap) / (320 + gap)), 1, 3)`, gap 12; cards row-major in `ui.columns`-style rows, top-aligned. Card: `pal.card` fill, border stroke, radius 10, 3 pt top strip in section colour, `section_dot` + name (medium 14), per test: name button (truncate, hover full) and pill right within card; second line muted 12 pt `"Last done {date}"` / `"Never done"`; N/A rows: muted "N/A", no second line. Sections with no rows after filtering are skipped; if nothing is left: `empty_state("Nothing here", "No tests have this status.")`.

- [ ] **Step 1: Failing smoke tests** (use `seeded(&dir, 365)`: Agnes has overdue Cobas):
  - `profile_status_strip_counts` — open Agnes; `get_by_label("Overdue 1")`, `get_by_label("Up to date 0")`.
  - `profile_next_up_opens_detail` — `get_by_label_contains("Next up: Cobas").click()` → `get_by_label("Close panel")` exists.
  - `profile_cards_show_last_done` — `get_by_label_contains("Last done ")`.
  - `profile_cards_narrow_window` — harness 900×800, open Agnes, `Cobas` still found, no panic.
  - Keep existing `profile_opens_detail_for_a_test` passing.
- [ ] **Step 2:** `cargo test --test ui_smoke profile` → new tests FAIL.
- [ ] **Step 3:** Implement header + cards; wire into `personnel.rs`.
- [ ] **Step 4:** `cargo test --test ui_smoke profile` → PASS.
- [ ] **Step 5:** Commit `feat: compact profile header and cards view`.

### Task 5: Table view

**Files:**
- Modify: `src/ui/profile.rs`
- Test: `tests/ui_smoke.rs`

**Interfaces:**
- Consumes: Task 4's `ProfileUi`; add `pub sort_by_due: bool` to it (not persisted).

One `pal.card` frame with an `egui::Grid` (`spacing [20, 8]`, striped off): headers `Section | Test | Last done | Next due` (muted 12.5); "Next due" header is a frameless button showing `▾` when `sort_by_due`, clicking toggles it. Grouped mode: rows in `rows()` order, section chip only on the first row of each section. Sorted mode: assigned rows by `due_on` (None last), then N/A; chip on every row. Last done: date or "—". Test name and pill open `cx.open_detail`.

- [ ] **Step 1: Failing smoke tests:**
  - `profile_table_view` — open Agnes, click `"Table"`; `get_by_label("Last done")` header and `Cobas` present; clicking `Cobas` opens detail.
  - `profile_filter_survives_view_toggle` — seed adds a second assigned test for Agnes completed 30 days ago (on track); click `"Overdue 1"`, then `"Table"` → `Cobas` present, `Architect` absent.
  - `profile_view_is_remembered` — after clicking `"Table"`, `h.state()` prefs (add `pub fn prefs(&self) -> &Prefs` on `TrackerApp` if missing) have `profile_view == Table`.
- [ ] **Step 2:** `cargo test --test ui_smoke profile_` → FAIL.
- [ ] **Step 3:** Implement table + sort toggle.
- [ ] **Step 4:** `cargo test --test ui_smoke` → PASS.
- [ ] **Step 5:** Commit `feat: profile table view`.

### Task 6: Section colours across the app

**Files:**
- Modify: `src/ui/personnel.rs` (`Summary::sections` → `Vec<(String, Color32)>`; `section_chips` fills with `color.gamma_multiply(0.14)` + dot, text `pal.text`; `chip_width` adds dot width)
- Modify: `src/ui/detail.rs:133-138,226-237` (`header` takes `Option<(&str, Color32)>` and draws `test name ·` + `section_chip`)
- Modify: `src/ui/summary.rs:582-611` (`section_row` takes `color: Color32`, paints a 3.5 pt dot before the name, name shifted 13 pt), `:836,1033-1039` (register Sections cell: a `LayoutJob` of `"● "` in each colour + name, comma-separated, still truncating)
- Modify: `src/ui/audit.rs:120-136` (`row` takes `&Data`; first line: when · by, then `section_chip` if `e.refs.get("section_id")` resolves via `section_color` + `data.section`)
- Modify: `src/ui/staff_editor.rs:165` (`section_dot` before the name)
- Test: `tests/ui_smoke.rs`

Note: the register uses coloured dots rather than full chips so the truncating fixed-width cell keeps working (my call, consistent with the spec's "dot + tint" rule).

- [ ] **Step 1: Failing smoke tests:**
  - `history_row_shows_section_chip` — push an `AuditEntry` with `refs = {"section_id": <Chemistry id>}`; open History; `get_all_by_label("Chemistry")` includes a non-filter node (count ≥ 2 incl. filter chip, or query within the list).
  - `history_row_deleted_section` — `refs = {"section_id": "s-gone"}`; open History; detail text visible, no panic.
  - `detail_subtitle_has_section_chip` — open Agnes → Cobas; `get_by_label("Chemistry")` present in panel.
- [ ] **Step 2:** `cargo test --test ui_smoke` → new tests FAIL.
- [ ] **Step 3:** Apply the changes listed under Files.
- [ ] **Step 4:** `cargo test` → all PASS; `cargo clippy --all-targets` clean.
- [ ] **Step 5:** Commit `feat: section colours across the app`.

### Task 7: Release build

- [ ] **Step 1:** `cargo fmt; cargo test` → PASS.
- [ ] **Step 2:** `cargo build --release` → success.
- [ ] **Step 3:** `Copy-Item target\release\competency-tracker.exe . -Force` → exe in project root with a new timestamp.
- [ ] **Step 4:** Launch the exe, open a person, check both views, light and dark, and the History page.
- [ ] **Step 5:** Commit any fmt changes `chore: rustfmt`.
