# Competency Tracker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a single, standalone Windows `.exe` in pure Rust (egui/eframe). It replaces the Excel competency workbook, runs from a shared network folder without admin rights, allows one editor at a time, and stores its data as JSON snapshots.

**Architecture:**
- A UI-free library crate (`src/lib.rs`) holds every piece of business logic, each in its own unit-tested module: model, scheduling, snapshots, lock, operations, queries, session state machine, Excel import, and export.
- A thin egui layer (`src/ui/`) renders pages from a `Session`. It mutates data only through `Session::mutate` and the `ops` functions.
- `src/main.rs` only launches the window.

**Tech Stack:**
- Rust stable (MSVC target), `eframe`/`egui` (wgpu renderer with glow fallback), `egui_extras` (tables, date picker), `egui-phosphor` (icons).
- `serde`/`serde_json`, `chrono`, `calamine`, `rust_xlsxwriter`, `rfd` (file dialogs), `nanoid`, `fuzzy-matcher`, `thiserror`, `windows-sys`.
- Dev-dependencies: `tempfile`, `egui_kittest`.

**Spec:** `docs/superpowers/specs/2026-11-14-competency-tracker-design.md`. Read it alongside this plan; section numbers below (§n) refer to it.

## Global Constraints

- One `.exe`, statically linked CRT (`-C target-feature=+crt-static`), Windows GUI subsystem (no console window), fonts and icons embedded. No installer, no admin rights, no WebView or .NET dependency.
- The data folder defaults to `<exe dir>\CompetencyData\`, overridable via `tracker.config.json` beside the exe (`{"data_dir": "..."}`; relative paths resolve against the exe dir).
- Snapshot file name: `d MMM yyyy - username.json` (e.g. `2 Dec 2026 - jtan.json`, no leading zero), in `CompetencyData\snapshots\`.
- The UI displays dates as `d MMM yyyy`.
- Identity is the Windows username (`USERNAME` env var, then `GetUserNameW`, then `"unknown"`). Every mutation records it.
- Defaults:
  - `due_soon_days = 30`, `variance_tolerance_days = 30`, `inactivity_minutes = 10`, `initial_due_days = None`.
  - Heartbeat every 30 s; a lock is stale after 120 s; inactivity warning 60 s before timeout.
  - Autosave debounce 3 s; viewer poll every 10 s.
- Default rule (one rule for all tests):
  - Initial (no interval).
  - Semi-annual: 6 months.
  - Annual: 12 months, repeating, ordinal (1st Annual, 2nd Annual, …).
  - Intervals count from the previous **real** completion.
- Due pills:
  - Red: due < today.
  - Yellow: 0 ≤ days until due ≤ `due_soon_days` (due today is yellow).
  - Green: otherwise.
  - Grey: no due date.
- Palette, light (default) / dark: bg `#FAF9F5`/`#262624`, sidebar `#F0EEE6`/`#1F1E1D`, card `#FFFFFF`/`#30302E`, border `#E8E6DC`/`#3E3E38`, text `#1F1E1D`/`#FAF9F5`, muted `#73726C`/`#A6A39A`, accent `#C96442`/`#D97757`.
- No Claude or Anthropic name or logo. App name: "Competency Tracker".
- Never import the `Staff` tab or the hidden tabs.
- History shows the 4 most recent entries by default, with a "Show N older entries" link.
- Fonts: Source Serif 4 (headings) and Inter (UI), both under the SIL Open Font License, embedded from `assets/fonts/`. **Downloading them needs the user's approval in chat**, giving file names and the source (`github.com/adobe-fonts/source-serif`, `github.com/rsms/inter`). Alternatively the user supplies the files.

## Review Focus

These are the conditions most likely to bite a real user. Each line names the expected behaviour, and the test that pins it is added to the owning task.

1. **Network share drops while editing.** Saves fail. Edits stay in memory, a "Not saved — retrying" banner shows, the app retries every 10 s, and it stays in edit mode. *(Task 7: `save_failure_keeps_editing_and_retries`)*
2. **Two PCs open the app at the same moment.** Exactly one gets edit mode. *(Task 7: `two_sessions_only_one_edits`)*
3. **An older exe opens data saved by a newer exe (`schema_version` > supported).** It opens read-only with a banner and never overwrites. *(Task 3: `newer_schema_opens_read_only`; Task 7: `newer_schema_never_saves`)*
4. **A user enters a completion date in the future.** It is rejected with a clear error, and no record is created. *(Task 5: `future_completion_rejected`)*
5. **The Windows username contains characters invalid in file names, or another PC's clock runs ahead.** File names are sanitised. A lock whose heartbeat is in the future counts as live, not stale. *(Task 3: `file_name_sanitises_username`; Task 4: `future_heartbeat_is_not_stale`)*

---

## File Structure

```
Cargo.toml                 crate + release profile
.cargo/config.toml         static CRT rustflags
build.rs                   embeds assets/app.ico (winres)
assets/fonts/              SourceSerif4-Regular.ttf, SourceSerif4-Semibold.ttf, Inter-Regular.ttf, Inter-Medium.ttf
assets/app.ico
src/main.rs                window launch, renderer fallback
src/lib.rs                 module declarations
src/model.rs               serde data types, Data helpers, ids
src/dates.rs               fmt_date, parse_dmy, now helpers
src/schedule.rs            stage labels, next due, status, variance
src/snapshot.rs            data dir, file naming, atomic save, load latest, archive
src/lock.rs                edit.lock acquire/heartbeat/release
src/identity.rs            username, machine name
src/ops.rs                 all mutations + audit entries
src/query.rs               Index, due items, counts, filters, grouping, history, search, audit/staff filters
src/session.rs             edit/view state machine, autosave, polling, inactivity, conflicts
src/import/mod.rs          re-exports
src/import/excel.rs        workbook → RawSheet grid parsing
src/import/mapping.rs      RawSheet → merge groups → ImportPlan (Data + report)
src/export.rs              xlsx export (status, history)
src/prefs.rs               per-user theme prefs in %APPDATA%
src/ui/mod.rs
src/ui/theme.rs            palette, fonts, egui style
src/ui/widgets.rs          due pill, stat card, chips, empty states
src/ui/app.rs              TrackerApp, sidebar, banner, toasts, modal, routing
src/ui/summary.rs
src/ui/sections.rs
src/ui/detail.rs           slide-in competency panel
src/ui/personnel.rs        list, editor, profile
src/ui/audit.rs
src/ui/settings.rs         sections/tests, rules, general, export, unconfirmed review
src/ui/import_wizard.rs
tests/ui_smoke.rs          egui_kittest smoke tests
```

Unit tests live in `#[cfg(test)] mod tests` inside each `src/*.rs` module. Use `tempfile::tempdir()` for every filesystem test.

---

### Task 1: Scaffold, model types, dates

**Files:**
- Create: `Cargo.toml`, `.cargo/config.toml`, `.gitignore` (`/target`, `CompetencyData/`), `src/lib.rs`, `src/main.rs` (placeholder `fn main(){}` until Task 11), `src/model.rs`, `src/dates.rs`

**Interfaces:**
- Produces (`model.rs`, all `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`):
  - `pub type Id = String;`
  - `pub type Ts = chrono::DateTime<chrono::FixedOffset>;`
  - `pub const SCHEMA_VERSION: u32 = 1;`
  - `pub fn new_id(prefix: &str) -> Id`: `"{prefix}-{nanoid(10)}"`.
  - `Settings { due_soon_days: i64, variance_tolerance_days: i64, inactivity_minutes: i64, initial_due_days: Option<i64>, archive_after_days: Option<i64> }`, with `Default` set to the global-constraint values and `archive_after_days: None`.
  - `Stage { label: String, interval_months: Option<u32>, repeats: bool, ordinal: bool }`
  - `Rule { id: Id, name: String, stages: Vec<Stage> }`, plus `Rule::default_global() -> Rule` (id `"r-global"`, name `"Global"`, the three stages from Global Constraints).
  - `TestSystem { id, name: String, order: u32, retired: bool, rule_id: Option<Id> }`
  - `Section { id, name, order: u32, retired: bool, tests: Vec<TestSystem> }`
  - `enum StaffStatus { Active, Hidden, Removed }` (serde lowercase)
  - `enum AssignState { Assigned, Na }` (serde lowercase)
  - `Assignment { state: AssignState, since: chrono::NaiveDate }`
  - `Staff { id, name, title, joined: Option<NaiveDate>, status: StaffStatus, assignments: BTreeMap<Id, Assignment> }` (keyed by test id)
  - `Record { id, staff_id, test_id, stage_index: u32, stage_label: String, completed_on: NaiveDate, comment: String, by: String, at: Ts, imported: bool, unconfirmed: bool }`
  - `Skipped { staff_id, test_id, stage_index: u32, by, at: Ts }`
  - `enum OverrideSource { Excel, Manual }`
  - `DueOverride { staff_id, test_id, due_on: NaiveDate, source: OverrideSource, by, at: Ts }`
  - `DelayNote { id, staff_id, test_id, for_stage_index: u32, note: String, expected_on: Option<NaiveDate>, by, at: Ts }`
  - `Note { id, staff_id, test_id: Option<Id>, section_id: Option<Id>, text: String, by, at: Ts }`. This holds imported free text (§12). The UI shows it in history.
  - `AuditEntry { at: Ts, by, action: String, detail: String, refs: BTreeMap<String, String> }`. Keys used: `staff_id`, `test_id`, `section_id`.
  - `Data { schema_version, saved_at: Ts, saved_by, settings, rules: Vec<Rule>, default_rule_id: Id, sections, staff, records, skipped, due_overrides, delay_notes, notes, audit }`. Every `Vec` field has `#[serde(default)]`.
  - Methods on `Data`:
    - `new_empty(user: &str, now: Ts) -> Data`: global rule; six sections named Chemistry, Haematology, Urinalysis, Molecular, Microbiology, Serology, with no tests.
    - `staff(&self, id) -> Option<&Staff>`, `staff_mut`.
    - `test(&self, test_id) -> Option<(&Section, &TestSystem)>`.
    - `section(&self, id) -> Option<&Section>`.
    - `rule_for_test(&self, test_id) -> &Rule`: the test's `rule_id`, falling back to the default rule.
    - `sections_sorted(&self) -> Vec<&Section>`: by `order`.
- Produces (`dates.rs`):
  - `pub fn fmt_date(d: NaiveDate) -> String`: `"%-d %b %Y"`.
  - `pub fn parse_dmy(s: &str) -> Option<NaiveDate>`: accepts `d/m/yyyy` and `dd/mm/yyyy`; `None` for invalid dates such as `31/06/2025`.
  - `pub fn now_ts() -> Ts`: `Local::now().fixed_offset()`.

- [ ] **Step 1: `git init`, then create `Cargo.toml`**
  - Use `cargo add` for the dependencies listed in Tech Stack, pinning the exact current versions it resolves. `egui`, `eframe`, `egui_extras`, `egui-phosphor` and `egui_kittest` must share one egui version.
  - eframe features: `wgpu`, `glow`. egui_extras features: `datepicker`. chrono features: `serde`.
  - `[profile.release] opt-level=3, lto=true, codegen-units=1, strip=true, panic="abort"`.
  - `.cargo/config.toml`: `[target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"]`.

- [ ] **Step 2: Write failing tests in `model.rs` and `dates.rs`**

```rust
#[test] fn data_roundtrips_json() {
    let d = Data::new_empty("jtan", sample_ts());
    let s = serde_json::to_string(&d).unwrap();
    assert_eq!(serde_json::from_str::<Data>(&s).unwrap(), d);
}
#[test] fn new_empty_has_global_rule_and_six_sections() {
    let d = Data::new_empty("jtan", sample_ts());
    assert_eq!(d.default_rule_id, "r-global");
    assert_eq!(d.sections_sorted().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
      ["Chemistry","Haematology","Urinalysis","Molecular","Microbiology","Serology"]);
    assert_eq!(d.rules[0].stages[1].interval_months, Some(6));
    assert!(d.rules[0].stages[2].repeats && d.rules[0].stages[2].ordinal);
}
#[test] fn missing_vec_fields_default() { /* JSON without "notes"/"skipped" parses */ }
#[test] fn fmt_date_has_no_leading_zero() { assert_eq!(fmt_date(ymd(2026,12,2)), "2 Dec 2026"); }
#[test] fn parse_dmy_cases() {
    assert_eq!(parse_dmy("16/9/2024"), Some(ymd(2024,9,16)));
    assert_eq!(parse_dmy("31/06/2025"), None);
    assert_eq!(parse_dmy("Not Done"), None);
}
```

- [ ] **Step 3: Run `cargo test --lib`.** Expected: compile failure or FAIL.
- [ ] **Step 4: Implement the types and functions above.**
- [ ] **Step 5: Run `cargo test --lib`.** Expected: all PASS.
- [ ] **Step 6: Commit** `feat: scaffold crate, data model and date helpers`

---

### Task 2: Scheduling (`schedule.rs`)

**Interfaces:**
- Consumes: `Rule`, `Stage`, `Record`, `DueOverride`, `Settings`, `OverrideSource` (Task 1).
- Produces:
  - `pub fn ordinal(n: u32) -> String`: "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "111th".
  - `pub fn stage_at(rule: &Rule, idx: u32) -> &Stage`: indexes past the end return the last stage if it repeats.
  - `pub fn stage_label(rule: &Rule, idx: u32) -> String`: for an `ordinal` repeating stage at position `r`, idx `r + k` → `"{ordinal(k+1)} {label}"`.
  - `pub struct PairState<'a> { pub records: Vec<&'a Record>, pub skipped: Vec<u32>, pub override_: Option<&'a DueOverride>, pub assigned_on: Option<NaiveDate> }`
  - `pub enum DueSource { Rule, Excel, Manual, None }`
  - `pub struct DueInfo { pub next_stage_index: u32, pub next_stage_label: String, pub due_on: Option<NaiveDate>, pub source: DueSource, pub last_completed_on: Option<NaiveDate> }`
  - `pub fn next_due(rule: &Rule, s: &PairState, settings: &Settings) -> DueInfo`
  - `pub enum DueStatus { Overdue, DueSoon, OnTrack, NotStarted }` (Copy, Eq, Ord in that order)
  - `pub fn classify(due: Option<NaiveDate>, today: NaiveDate, due_soon_days: i64) -> DueStatus`
  - `pub fn completion_variance(rule: &Rule, s: &PairState, record_id: &str) -> Option<i64>`: days late (+) or early (−) against the due date that completion answered. `None` for the first completion.
  - `pub fn variance_tag(days: i64, tolerance_days: i64) -> Option<String>`: `"late by 47 days"` or `"early by 40 days"`; `None` when `|days| <= tolerance`. Use "1 day" for the singular.

`next_due` algorithm (the tests do not fully determine it):
1. latest = the record with max `(completed_on, stage_index)`.
2. If there is none: next = 0. If 0 is in `skipped`, advance as in step 3. due = `assigned_on + initial_due_days` when both are `Some`; otherwise `None`.
3. Otherwise: next = latest.stage_index + 1. While next is in `skipped`, next += 1. due = `latest.completed_on.checked_add_months(Months::new(stage_at(next).interval_months))`. chrono clamps to the end of the month.
4. If `override_` is `Some`, it replaces `due_on`, and `source` becomes Excel or Manual.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn labels() { let r = Rule::default_global();
  assert_eq!(stage_label(&r,0),"Initial"); assert_eq!(stage_label(&r,1),"Semi-annual");
  assert_eq!(stage_label(&r,2),"1st Annual"); assert_eq!(stage_label(&r,3),"2nd Annual");
  assert_eq!(stage_label(&r,13),"12th Annual"); assert_eq!(ordinal(22),"22nd"); }
#[test] fn xin_yi_semi_then_annual() { // Initial 2 May 2024, Semi 2 Nov 2024
  let i = next_due(&r, &pair(&[(0,ymd(2024,5,2)),(1,ymd(2024,11,2))]), &Settings::default());
  assert_eq!(i.next_stage_label,"1st Annual"); assert_eq!(i.due_on, Some(ymd(2025,11,2))); }
#[test] fn semi_due_six_months_after_initial() { /* (0, 2024-05-02) → due 2024-11-02, stage 1 */ }
#[test] fn skipped_semi_counts_from_initial() { // Initial 2 Jul 2024, semi skipped
  /* skipped=[1] → next 2 "1st Annual", due 2025-07-02 */ }
#[test] fn month_end_clamps() { /* (0, 2024-08-31) → 2025-02-28; (0, 2023-08-31) → 2024-02-29 */ }
#[test] fn late_completion_counts_from_actual_date() { /* semi done 2025-01-20 (due was 2024-11-02) → annual due 2026-01-20 */ }
#[test] fn no_records_initial_pending() { /* due None, label "Initial", source None */ }
#[test] fn initial_due_days_setting() { /* assigned_on 2026-01-01, initial_due_days Some(30) → due 2026-01-31 */ }
#[test] fn override_wins() { /* Excel override 2027-03-24 → due 2027-03-24, source Excel */ }
#[test] fn classify_boundaries() { let t = ymd(2026,11,14);
  assert_eq!(classify(Some(ymd(2026,11,13)),t,30), DueStatus::Overdue);
  assert_eq!(classify(Some(t),t,30), DueStatus::DueSoon);
  assert_eq!(classify(Some(ymd(2026,12,14)),t,30), DueStatus::DueSoon);
  assert_eq!(classify(Some(ymd(2026,12,15)),t,30), DueStatus::OnTrack);
  assert_eq!(classify(None,t,30), DueStatus::NotStarted); }
#[test] fn variance() { /* semi done 2024-12-19 vs due 2024-11-02 → Some(47); tag(47,30)="late by 47 days"; tag(20,30)=None; tag(-40,30)="early by 40 days" */ }
```

- [ ] **Step 2: Run `cargo test --lib schedule`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --lib schedule`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: competency schedule calculation`

---

### Task 3: Snapshots and data folder (`snapshot.rs`)

**Interfaces:**
- Consumes: `Data`, `SCHEMA_VERSION`, `fmt_date`.
- Produces:
  - `pub struct DataDir { pub root: PathBuf }`
    - Methods: `snapshots()`, `archive()` (`snapshots\archive`), `conflicts()` (`snapshots\conflicts`), `lock_path()` (`edit.lock`), `import_report()` (`import-report.txt`).
    - `ensure(&self) -> io::Result<()>`: creates the folders.
  - `pub fn resolve_data_dir(exe_dir: &Path) -> DataDir`
  - `pub fn sanitize_user(user: &str) -> String`: replaces `\ / : * ? " < > |` and control characters with `_`, trims spaces and dots, and returns `"unknown"` if empty.
  - `pub fn snapshot_file_name(date: NaiveDate, user: &str) -> String`
  - `pub fn save_snapshot(dir: &DataDir, data: &Data, file_name: &str) -> io::Result<PathBuf>`: writes `<name>.tmp` in the same folder, `sync_all`, then `std::fs::rename` over the target (this replaces it on Windows).
  - `pub struct Loaded { pub data: Data, pub path: PathBuf, pub corrupt: Vec<PathBuf>, pub read_only_newer_schema: bool }`
  - `pub fn load_latest(dir: &DataDir) -> io::Result<Option<Loaded>>`: parses every `*.json` directly in `snapshots()` (not subfolders, not `.tmp`) and picks the max `saved_at`. Files that fail to parse go in `corrupt`. `read_only_newer_schema = data.schema_version > SCHEMA_VERSION`; for such files deserialize leniently, since unknown fields are ignored by default.
  - `pub fn archive_old(dir: &DataDir, keep_days: i64, today: NaiveDate) -> io::Result<usize>`: moves snapshots whose `saved_at` is older than `keep_days` into `archive()`, never the newest one.

- [ ] **Step 1: Write failing tests**
  - `file_name_format`: `snapshot_file_name(ymd(2026,11,14),"jtan") == "14 Nov 2026 - jtan.json"`.
  - `file_name_sanitises_username`: `"a:b*c"` → `"14 Nov 2026 - a_b_c.json"`; `""` → `"... - unknown.json"`.
  - `save_then_load_latest_picks_newest_saved_at`: two files where the one with the *older* modified time has the newer `saved_at`; that one is loaded.
  - `corrupt_and_tmp_ignored`: a truncated `x.json` and a `y.json.tmp` give `corrupt == [x.json]`, and the valid file still loads.
  - `same_user_same_day_overwrites`: saving twice gives exactly one file.
  - `newer_schema_opens_read_only`: `schema_version: 99` gives `read_only_newer_schema == true`.
  - `resolve_data_dir_default_and_config`: no config → `<exe>\CompetencyData`; config `{"data_dir":"..\\Shared"}` → `<exe>\..\Shared`.
  - `archive_keeps_newest`.
- [ ] **Step 2: Run `cargo test --lib snapshot`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --lib snapshot`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: snapshot storage with atomic writes`

---

### Task 4: Edit lock and identity (`lock.rs`, `identity.rs`)

**Interfaces:**
- Produces (`identity.rs`):
  - `pub fn windows_username() -> String`
  - `pub fn machine_name() -> String`: `COMPUTERNAME`, falling back to `"unknown-pc"`.
- Produces (`lock.rs`):
  - `pub const HEARTBEAT_SECS: i64 = 30; pub const STALE_SECS: i64 = 120;`
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct LockInfo { pub user: String, pub machine: String, pub pid: u32, pub acquired_at: Ts, pub heartbeat_at: Ts }`
  - `pub fn same_owner(a: &LockInfo, b: &LockInfo) -> bool`: user, machine and pid are all equal.
  - `pub enum Acquire { Acquired, TookOverStale(LockInfo), HeldBy(LockInfo) }`
  - `pub fn is_stale(info: &LockInfo, now: Ts) -> bool`: `now - heartbeat_at > STALE_SECS`. A future heartbeat is never stale.
  - `pub fn read_lock(path: &Path) -> io::Result<Option<LockInfo>>`: an unparseable file counts as a stale lock, so the caller treats it as takeover-able.
  - `pub fn try_acquire(path: &Path, me: &LockInfo, now: Ts) -> io::Result<Acquire>`: `OpenOptions::new().write(true).create_new(true)`. If the file exists and is stale or unparseable: remove it, retry `create_new` once, and return `TookOverStale(previous)`. If the retry loses the race, re-read and return `HeldBy`. If the existing lock is our own (`same_owner`), return `Acquired`.
  - `pub fn heartbeat(path: &Path, me: &LockInfo, now: Ts) -> io::Result<bool>`: `false` if we are not the owner. Otherwise rewrite `heartbeat_at` via tmp + rename.
  - `pub fn is_owner(path: &Path, me: &LockInfo) -> io::Result<bool>`
  - `pub fn release(path: &Path, me: &LockInfo) -> io::Result<()>`: removes the file only if we own it.

- [ ] **Step 1: Write failing tests**
  - `acquire_when_free`.
  - `second_owner_gets_held_by` (different pid).
  - `stale_lock_taken_over` (heartbeat 121 s old → `TookOverStale`).
  - `fresh_lock_not_taken` (heartbeat 119 s old → `HeldBy`).
  - `future_heartbeat_is_not_stale` (heartbeat now + 5 min → not stale).
  - `corrupt_lock_taken_over`.
  - `heartbeat_updates_and_rejects_non_owner`.
  - `release_only_by_owner`: a non-owner release leaves the file in place.
  - `username_non_empty`.
- [ ] **Step 2: Run `cargo test --lib lock identity`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Use `windows-sys` `GetUserNameW` as the fallback, behind `#[cfg(windows)]`.
- [ ] **Step 4: Run the same command.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: single-editor lock file and Windows identity`

---

### Task 5: Mutations with audit (`ops.rs`)

**Interfaces:**
- Consumes: `model::*`, `schedule::stage_label`, `dates::fmt_date`.
- Produces:
  - `pub struct Actor { pub user: String, pub now: Ts }`, with `Actor::today(&self) -> NaiveDate`.
  - `#[derive(thiserror::Error, Debug, PartialEq)] pub enum OpError { NotFound(&'static str), FutureDate, EmptyName, HasRecords, NotRemoved, ConfirmMismatch, IsDefaultRule }`
  - Every function takes `(d: &mut Data, a: &Actor, ...)`, returns `Result<_, OpError>`, and on success appends exactly one `AuditEntry` (`by = a.user`, refs filled in, `detail` in plain English as in §8.4).
  - Completions and notes:
    - `add_completion(staff_id, test_id, stage_index: u32, completed_on: NaiveDate, comment: &str) -> Result<Id>`: `FutureDate` if `completed_on > a.today()`. Stores `stage_label`. **Removes any `DueOverride` for the pair.**
      - Action `record.add`. Detail: `Marked {test} {stage} complete for {name} on {d MMM yyyy}`.
    - `edit_record(record_id, completed_on, stage_index, comment)`: `record.edit`.
    - `delete_record(record_id)`: `record.delete`.
    - `confirm_records(ids: &[Id]) -> Result<usize>`: one audit entry, `record.confirm`.
    - `add_delay_note(staff_id, test_id, for_stage_index, note: &str, expected_on: Option<NaiveDate>) -> Result<Id>`: `delay.add`.
    - `delete_delay_note(id)`: `delay.delete`.
    - `set_skipped(staff_id, test_id, stage_index, skipped: bool)`: `stage.skip` / `stage.unskip`.
    - `set_due_override(staff_id, test_id, due_on: Option<NaiveDate>)`: Manual source; `None` removes the override. Action `due.override`.
  - Staff:
    - `add_staff(name, title, joined: Option<NaiveDate>) -> Result<Id>`: `EmptyName` if the trimmed name is empty.
    - `update_staff(id, name, title, joined)`.
    - `set_staff_status(id, StaffStatus)`.
    - `delete_staff_permanently(id, confirm_text: &str)`: `NotRemoved` unless the status is Removed; `ConfirmMismatch` unless `confirm_text.trim() == name`. Removes the person's records, skipped stages, overrides, delay notes and notes.
    - `set_assignment(staff_id, test_id, state: Option<AssignState>)`: `since = a.today()` when newly assigned.
    - `assign_section(staff_id, section_id, assigned: bool)`: every non-retired test in the section.
  - Sections:
    - `add_section(name) -> Result<Id>`, `rename_section`.
    - `move_section(id, delta: i32)`: swap with a neighbour; `order` values are renumbered 0..n.
    - `set_section_retired`.
    - `delete_section(id)`: `HasRecords` if any of its tests has records.
  - Tests:
    - `add_test(section_id, name) -> Result<Id>`, `rename_test`.
    - `move_test(test_id, delta)`: within its section.
    - `set_test_retired`.
    - `delete_test`: `HasRecords` guard.
    - `set_test_rule(test_id, rule_id: Option<Id>)`.
  - Rules and settings:
    - `add_rule(name, stages: Vec<Stage>) -> Result<Id>`, `update_rule(id, name, stages)`.
    - `delete_rule(id)`: `IsDefaultRule` guard; tests using the rule get `rule_id = None`.
    - `update_settings(s: Settings)`: `settings.update`, with changed fields listed in `detail`.

- [ ] **Step 1: Write failing tests** (with a helper building a `Data` with one section, two tests and one staff member):
  - `add_completion_records_user_and_audits`: `by == "jtan"`; the audit has `action == "record.add"` and `refs["staff_id"]`.
  - `future_completion_rejected`: `Err(FutureDate)`; record count unchanged; no audit entry.
  - `completion_clears_override`.
  - `delete_staff_requires_removed_and_name`.
  - `delete_test_blocked_with_records`.
  - `move_section_reorders`: [A,B,C] with B moved −1 gives [B,A,C], orders 0,1,2.
  - `delete_rule_resets_tests`.
  - `assign_section_assigns_all_live_tests`.
  - `confirm_records_clears_flag`.
- [ ] **Step 2: Run `cargo test --lib ops`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --lib ops`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: audited data operations`

---

### Task 6: Queries (`query.rs`)

**Interfaces:**
- Consumes: `schedule::{PairState, next_due, classify, DueInfo, DueStatus}`, `model::*`.
- Produces:
  - `pub struct Index<'a>`, with `Index::build(data: &'a Data) -> Index<'a>` (groups records, skipped stages, overrides, delay notes and notes by `(staff_id, test_id)` in one pass) and `fn pair(&self, staff_id: &str, test_id: &str) -> PairState<'a>`. `assigned_on` comes from `Staff.assignments[test].since`.
  - `pub struct DueItem { pub staff_id: Id, pub staff_name: String, pub title: String, pub section_id: Id, pub section_name: String, pub test_id: Id, pub test_name: String, pub info: DueInfo, pub status: DueStatus, pub delay_note: Option<DelayNote> }`. `delay_note` is the latest note whose `for_stage_index == info.next_stage_index`.
  - `pub fn due_items(data: &Data, idx: &Index, today: NaiveDate) -> Vec<DueItem>`
    - Includes: Active staff × tests with `AssignState::Assigned`, where neither the test nor its section is retired.
    - Sort order: status (Overdue, DueSoon, OnTrack, NotStarted), then `due_on` ascending, then staff name.
  - `pub struct Counts { pub overdue: usize, pub due_soon: usize, pub on_track: usize, pub delayed: usize }`, with `pub fn counts(items: &[DueItem]) -> Counts`. `delayed` = has a delay note and the status is Overdue or DueSoon.
  - `pub enum StatusFilter { All, Overdue, DueSoon, Delayed, OnTrack, NotStarted }`
  - `#[derive(Default)] pub struct DueFilter { pub section: Option<Id>, pub test: Option<Id>, pub title: Option<String>, pub status: StatusFilter /*All*/, pub text: String }`
  - `pub fn filter_items<'a>(items: &'a [DueItem], f: &DueFilter) -> Vec<&'a DueItem>`: `text` is a case-insensitive substring match over staff, test and section names.
  - `pub fn group_by_person<'a>(items: &[&'a DueItem]) -> Vec<(Id, Vec<&'a DueItem>)>` and `group_by_test`, each preserving input order.
  - `pub enum HistoryEntry<'a> { Completion(&'a Record), Delay(&'a DelayNote), Note(&'a Note) }`, with `fn date(&self) -> NaiveDate` (a completion uses `completed_on`; the others use `at.date_naive()`).
  - `pub const HISTORY_DEFAULT: usize = 4;`
  - `pub fn history<'a>(data: &'a Data, staff_id: &str, test_id: &str) -> Vec<HistoryEntry<'a>>`: newest first.
  - `pub fn split_history<'a, 'b>(h: &'b [HistoryEntry<'a>], expanded: bool) -> (&'b [HistoryEntry<'a>], usize /*hidden count*/)`
  - `pub enum SearchHit { Staff(Id), Test { section_id: Id, test_id: Id }, Section(Id) }`
  - `pub fn search(data: &Data, q: &str, limit: usize) -> Vec<(SearchHit, String /*label*/)>`: uses `SkimMatcherV2` and excludes non-Active staff.
  - `#[derive(Default)] pub struct StaffFilter { pub status: Option<StaffStatus> /*None=Active*/, pub section: Option<Id>, pub title: Option<String>, pub text: String }`, with `pub fn filter_staff<'a>(data: &'a Data, f: &StaffFilter) -> Vec<&'a Staff>` (sorted by name).
  - `#[derive(Default)] pub struct AuditFilter { pub user: Option<String>, pub from: Option<NaiveDate>, pub to: Option<NaiveDate>, pub section: Option<Id>, pub staff: Option<Id>, pub action_prefix: Option<String>, pub text: String }`, with `pub fn filter_audit<'a>(data: &'a Data, f: &AuditFilter) -> Vec<&'a AuditEntry>` (newest first).
  - `pub fn distinct_titles(data: &Data) -> Vec<String>` and `pub fn distinct_audit_users(data: &Data) -> Vec<String>`, for filter dropdowns.

- [ ] **Step 1: Write failing tests**
  - `excludes_hidden_removed_retired_and_na`.
  - `sorted_by_urgency`.
  - `counts_and_delayed`.
  - `filter_by_section_status_text`.
  - `history_newest_first_and_split`: six completions → shown 4, hidden 2; `expanded` → shown 6, hidden 0.
  - `search_finds_staff_and_tests_fuzzy`: "cel dyn" finds "Cell-DYN Ruby".
  - `audit_filter_by_user_and_range`.
- [ ] **Step 2: Run `cargo test --lib query`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --lib query`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: due lists, filters, history and search`

---

### Task 7: Session state machine (`session.rs`)

**Interfaces:**
- Consumes: `snapshot::*`, `lock::*`, `ops::Actor`, `Data`.
- Produces:
  - Constants: `pub const AUTOSAVE_DEBOUNCE: Duration = 3s; POLL_EVERY: Duration = 10s; WARNING_BEFORE: Duration = 60s;`.
  - `pub enum Mode { Editing, Viewing { holder: Option<LockInfo> } }`. `holder: None` means editing is available.
  - `pub enum LeaveReason { Inactivity, Manual, Conflict }`
  - `pub enum SessionEvent { InactivityWarning { secs_left: u64 }, LeftEditMode(LeaveReason), Saved, SaveFailed(String), Reloaded, EditingAvailable, Conflict(PathBuf), TookOverStale(LockInfo) }`
  - `pub struct Session` (private fields), with:
    - `Session::open(dir: DataDir, user: &str, machine: &str, pid: u32, now: Ts, inst: Instant) -> io::Result<Session>`: `dir.ensure()`, then `load_latest`, otherwise `Data::new_empty`. Then `try_acquire`, unless the data is a newer schema, in which case it is always Viewing.
    - `fn mode(&self) -> &Mode`, `fn is_editing(&self) -> bool`.
    - `fn data(&self) -> &Data`, `fn revision(&self) -> u64` (bumped on every mutation and reload; the UI caches by it).
    - `fn user(&self) -> &str`.
    - `fn save_error(&self) -> Option<&str>`, `fn read_only_reason(&self) -> Option<&str>`, `fn corrupt_files(&self) -> &[PathBuf]`.
    - `fn note_activity(&mut self, inst: Instant)`.
    - `fn mutate<R>(&mut self, now: Ts, f: impl FnOnce(&mut Data, &Actor) -> R) -> Option<R>`: `None` unless Editing. Marks dirty and bumps the revision.
    - `fn replace_data(&mut self, now: Ts, data: Data) -> bool`: used by import; Editing only; saves immediately.
    - `fn tick(&mut self, now: Ts, inst: Instant) -> Vec<SessionEvent>`
    - `fn start_editing(&mut self, now: Ts, inst: Instant) -> io::Result<Acquire>`
    - `fn stop_editing(&mut self, now: Ts) -> io::Result<()>`
    - `fn save_now(&mut self, now: Ts) -> Result<PathBuf, String>`
    - `fn on_exit(&mut self, now: Ts)`
    - `fn inactivity_timeout(&self) -> Duration` (from settings).
- `tick` rules, in order:
  1. **Editing:**
     - Heartbeat every `HEARTBEAT_SECS`. If the heartbeat returns `false`, treat it as a conflict (step 1c).
     - If dirty and the debounce has elapsed, or a previous save failed and 10 s have passed: call `save_now`.
     - **`save_now` steps:**
       - a. Check `is_owner`.
       - b. Set `saved_at = now`, `saved_by = user`.
       - c. If not the owner: write to `conflicts()/"{file stem} - conflict {HHmmss}.json"`, switch to Viewing, and emit `Conflict` and `LeftEditMode(Conflict)`.
       - d. Otherwise save to `snapshot_file_name(now.date, user)`.
       - On an I/O error, keep the dirty flag, set `save_error`, emit `SaveFailed`, and stay Editing.
     - Inactivity: idle ≥ timeout − 60 s → `InactivityWarning`. Idle ≥ timeout → save, release, Viewing, `LeftEditMode(Inactivity)`.
  2. **Viewing** (every `POLL_EVERY`):
     - `read_lock`. A missing or stale lock gives `holder = None` and emits `EditingAvailable` once per transition.
     - `load_latest`. If its `saved_at` is newer than the current data's, replace the data and emit `Reloaded`.
     - Viewers never auto-acquire after open.
  3. **Newer-schema data:** never saves and never acquires the lock.
- [ ] **Step 1: Write failing tests** (tempdir; drive time by passing `inst + Duration` and `now + chrono::Duration`):
  - `opens_editing_when_free_and_viewing_when_held`.
  - `two_sessions_only_one_edits`: same dir, pids 1 and 2, opened back to back.
  - `autosave_after_debounce_writes_named_file`: a file `"{d MMM yyyy} - jtan.json"` exists after 3 s and not before.
  - `inactivity_warns_then_releases`: warning at 9 min; at 10 min Viewing, lock file gone, data saved.
  - `activity_resets_timer`.
  - `viewer_reloads_newer_snapshot_and_sees_available`: editor session A saves and stops; viewer B gets `Reloaded` and `EditingAvailable` after 10 s.
  - `stale_lock_takeover_on_open_emits_event`.
  - `conflict_when_lock_stolen`: overwrite the lock with another owner, mutate, tick 3 s → `Conflict`, a file in `conflicts/`, the snapshot unchanged.
  - `save_failure_keeps_editing_and_retries`: replace `snapshots` with a *file* of the same name so writes fail → `SaveFailed`, still Editing, `save_error` is `Some`. Restore the directory and tick 10 s → `Saved`.
  - `newer_schema_never_saves`: start Viewing; `mutate` returns `None`; `start_editing` does not create a lock.
  - `exit_saves_and_releases`.
- [ ] **Step 2: Run `cargo test --lib session`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --lib session`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: edit/view session with autosave, lock heartbeat and inactivity`

---

### Task 8: Excel parsing (`import/excel.rs`)

**Interfaces:**
- Produces:
  - `pub const SHEETS: [&str; 6] = ["Chemistry & Immunology","Hematology","Urinalysis","Molecular Biology","Microbiology","Serology"];`
  - `#[derive(Clone, Debug, PartialEq)] pub enum Cell { Empty, Date(NaiveDate), Text(String) }`, with `pub fn to_cell(v: &calamine::Data) -> Cell`. DateTime becomes Date; trimmed strings become `Text`, or `Date` via `parse_dmy`; numbers become Text.
  - `pub struct RawTest { pub col: usize, pub header: String /*whitespace-collapsed*/, pub tick_col: Option<usize>, pub section_hint: String }`
  - `pub struct StageCell { pub cell: Cell, pub ticked: bool }`
  - `pub struct RawBlock { pub row: usize, pub name: String, pub title: String, pub joined: Option<NaiveDate>, pub stages: [Vec<StageCell>; 4] /* [Initial, Semi, Annual, NextDue][test_idx] */, pub remarks: Vec<String> }`
  - `pub struct RawSheet { pub name: String, pub tests: Vec<RawTest>, pub blocks: Vec<RawBlock> }`
  - `pub fn parse_grid(sheet: &str, grid: &[Vec<Cell>]) -> RawSheet`
  - `pub fn read_workbook(path: &Path) -> Result<Vec<RawSheet>, ImportError>`: `calamine::open_workbook_auto`, only the `SHEETS`. `ImportError` (thiserror) is `Open(String)` or `MissingSheet(String)`.
- Parsing rules (from the workbook's structure):
  - **Header search:** look in rows 0–2 for the cells "Name", "Position", "Joined date" and "Competency Type" (each matched after collapsing whitespace, case-insensitive, by `starts_with`), plus "Remark" (`starts_with`).
  - **Test columns:** non-empty row-2 cells strictly between the competency-type column and the remarks column, excluding "Joined date".
  - **`tick_col`:** `col + 1` if that column's row-2 cell is empty, it lies before remarks, and any row below has `Text("a")` there.
  - **`section_hint`:** the nearest row-0 cell at or left of `col` whose text contains "Test System", with " Test System" stripped and trimmed.
  - **Blocks:** start where the competency-type column equals "Initial" (case-insensitive, trimmed). Rows +1..+3 are Semi, Annual and Next Due.
    - The name and title come from the start row. A merged cell is read at its top-left (calamine gives empty for the other cells, so read the start row).
    - Skip names "Buffer Cells" and "Buffer Row".
    - `remarks` = the non-empty remark-column texts over the 4 rows.
    - `ticked` = `Text("a")` in `tick_col` on the same row.
- [ ] **Step 1: Write failing tests** with hand-built grids:
  - `parses_tests_ticks_and_blocks`: two tests with ticks, one block → `stages[0][0] == StageCell{Date(2020-03-19), ticked: true}`.
  - `test_without_tick_column`: three adjacent TaqPath-style headers → each has `tick_col == None`.
  - `joined_date_in_header_row3`: the Molecular/Micro layout.
  - `section_hint_from_group_header`: the Hematology grid with a "Chemistry & Immunology Test System" group header → the first test's hint is "Chemistry & Immunology".
  - `skips_buffer_rows`.
  - `#[ignore] real_workbook_smoke`: reads the path in env var `TRACKER_XLSM`; asserts 6 sheets, Serology has 14 tests, Chemistry has ≥ 25 blocks.
- [ ] **Step 2: Run `cargo test --lib import::excel`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --lib import::excel`, then `$env:TRACKER_XLSM="TRAINING & COMPETENCY_ Master Tracking.xlsm"; cargo test --lib real_workbook_smoke -- --ignored`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: parse Excel competency workbook`

---

### Task 9: Import mapping (`import/mapping.rs`)

**Interfaces:**
- Consumes: `RawSheet`/`RawBlock`/`Cell` (Task 8), `schedule::next_due`, `model::*`.
- Produces:
  - `pub fn normalize_name(s: &str) -> String`: lowercase, whitespace collapsed, trimmed.
  - `pub fn name_aliases(s: &str) -> Vec<String>`: the normalised full name, the part outside brackets, and each part inside brackets. `"Christine (Li Yun)"` → `["christine (li yun)", "christine", "li yun"]`.
  - `pub struct MergeGroup { pub name: String, pub members: Vec<(String /*sheet*/, usize /*row*/, String /*raw name*/)>, pub joined: Option<NaiveDate>, pub title: String, pub conflicts: Vec<String> }`
  - `pub fn propose_merges(sheets: &[RawSheet]) -> Vec<MergeGroup>`
    - Union-find over blocks that share any alias. Same-tab duplicates merge too.
    - `name` = the shortest bracket-free raw name.
    - `joined` = the earliest date; `title` = the most frequent title.
    - `conflicts` lists the differing joined dates and titles.
  - `pub fn split_group(groups: &mut Vec<MergeGroup>, idx: usize)`: every member becomes its own group.
  - `pub struct ImportOptions { pub user: String, pub now: Ts, pub tolerance_days: i64 }`
  - `#[derive(Default)] pub struct ImportReport { pub sheets: usize, pub staff: usize, pub records: usize, pub unconfirmed: usize, pub planned: usize, pub notes: usize, pub excel_overrides: Vec<String>, pub excel_earlier: Vec<String>, pub flagged_stage: Vec<String>, pub merges: Vec<String> }`, with `fn to_text(&self) -> String`.
  - `pub struct ImportPlan { pub data: Data, pub report: ImportReport }`
  - `pub fn build_import(sheets: &[RawSheet], groups: &[MergeGroup], opts: &ImportOptions) -> ImportPlan`
- `build_import` rules (§12, decided):
  1. **Sections:** one per sheet, in `SHEETS` order, named as the sheet.
  2. **Tests:**
     - Ordered by column.
     - Duplicate names within a section get " (2)" and " (3)".
     - A test whose `section_hint` names a *different* sheet maps into that section's test with the same normalised name (created there if missing).
     - Records for the same `(staff, test, completed_on)` are deduplicated, preferring ticked ones.
  3. **Cells in stage rows 0–2** (stage index 0, 1 and the annual index):
     - **Ticked date:** record `imported: true`, `by: "excel-import"`.
     - **Unticked date ≤ today, or any date in a test with `tick_col == None`:** record with `unconfirmed: true`.
     - **Unticked future date:** planned. A `DueOverride{source: Excel}` with that date; counts in `planned`.
     - **`-`, `NA`, `N/A`:** a `Skipped` entry for that stage. But if all three stage rows are one of these, set the assignment to `Na` and add no Skipped entries.
     - **`Not Done`, `Not done`, or empty:** nothing.
     - **Any other text:** a `Note` for `(staff, test)` with text `"{stage label}: {text}"`.
  4. **Annual index:** 2 (1st Annual) by default.
     - If a block remark matches `(\d+)\s*(st|nd|rd|th)` and one of its other tokens (length ≥ 2, excluding `annual`, `done`, `on`) is a case-insensitive prefix of a word in the test header, use `ordinal + 1` for that test.
     - If an ordinal remark exists but matches no test, push "{name} – {sheet}: check annual numbering" to `flagged_stage`.
  5. **Remarks:** each becomes a `Note` with `section_id`.
     - Remarks or cells matching `not assigned to`, `no longer in`, `no longer working in` (case-insensitive) → unassign that section's tests for the person. Records are kept.
     - `left the company`, `no longer working at` → status Removed.
  6. **Assignment:** Assigned (`since` = joined, else today) if any Date appears in the test column across all 4 rows, unless it was unassigned in step 5 or set to Na in step 3.
  7. **Next-due row (row 3) Date:** compute `next_due` from the imported state.
     - If the Excel date is more than `tolerance_days` *later* than the computed date (or nothing was computed): set a `DueOverride{Excel}`, keeping the later of this and any planned override, and push to `excel_overrides`.
     - If it is more than `tolerance_days` *earlier*: push to `excel_earlier`.
     - Within tolerance: nothing.
  8. **Audit:** one entry, `import.excel`, with the summary counts.
- [ ] **Step 1: Write failing tests** with hand-built `RawSheet`s:
  - `aliases_merge_bracket_names`: "Shawn", "Shawn (Weixiang)" and "Weixiang" form one group named "Shawn". "Christine (Li Yun)" and "Li Yun" form one group.
  - `same_tab_duplicates_merge`.
  - `merge_conflicts_listed_earliest_joined`.
  - `ticked_date_is_confirmed_record`.
  - `unticked_past_date_is_unconfirmed`.
  - `tickless_column_is_unconfirmed`.
  - `unticked_future_date_is_planned_override`.
  - `dash_in_one_row_skips_stage`.
  - `all_dash_is_na`.
  - `other_text_becomes_note`.
  - `not_assigned_remark_unassigns_keeps_records`.
  - `left_company_removes_staff`.
  - `annual_ordinal_from_remark`: remark "RET 2nd Annual DONE on 27 Mar 2024" with header "Reticulocyte Count" → stage index 3, label "2nd Annual".
  - `excel_due_within_tolerance_ignored`: computed 2025-07-18, Excel 2025-08-10 → no override.
  - `excel_due_later_beyond_tolerance_overrides`: Excel 2027-03-24 vs computed 2026-03-07 → override + report line.
  - `excel_due_earlier_reported_only`.
  - `cross_section_column_merges_into_chemistry`.
  - `duplicate_headers_suffixed`.
  - `#[ignore] real_workbook_import`: `TRACKER_XLSM`, auto-merge everything; asserts staff count < the sum of blocks, records > 500, and `report.to_text()` is non-empty. Write the report to the scratch output for manual reading.
- [ ] **Step 2: Run `cargo test --lib import::mapping`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the unit tests, then the ignored real-workbook test.** Expected: PASS. Skim the report text for nonsense such as obviously wrong merges.
- [ ] **Step 5: Commit** `feat: map Excel workbook into tracker data`

---

### Task 10: Export and preferences (`export.rs`, `prefs.rs`)

**Interfaces:**
- Produces:
  - `pub fn export_status(data: &Data, today: NaiveDate, path: &Path) -> Result<(), rust_xlsxwriter::XlsxError>`
    - Sheet "Status", columns: Section, Test system, Staff, Title, Next stage, Due, Status, Delay note.
    - Rows come from `due_items` (all statuses). Due is an Excel date in `d mmm yyyy` format.
  - `pub fn export_history(data: &Data, path: &Path) -> Result<(), XlsxError>`
    - Sheet "History", columns: Section, Test system, Staff, Stage, Completed on, Comment, Unconfirmed, By, Recorded at.
  - `#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)] pub enum Theme { #[default] Light, Dark }`
  - `#[derive(Serialize, Deserialize, Clone, Default)] pub struct Prefs { pub theme: Theme, pub sidebar_collapsed: bool }`
  - `pub fn prefs_path() -> Option<PathBuf>`: `%APPDATA%\CompetencyTracker\prefs.json`.
  - `pub fn load_prefs_from(p: &Path) -> Prefs`: default on any error.
  - `pub fn save_prefs_to(p: &Path, prefs: &Prefs) -> io::Result<()>`
- [ ] **Step 1: Write failing tests**
  - `export_status_writes_file`: the file exists and is non-empty, and reading it back with calamine gives the header row and the row count.
  - `export_history_rows`.
  - `prefs_default_light_and_roundtrip`.
- [ ] **Step 2: Run `cargo test --lib export prefs`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the same command.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: xlsx export and per-user prefs`

---

### Task 11: App shell, theme, launch (`main.rs`, `ui/theme.rs`, `ui/widgets.rs`, `ui/app.rs`)

**Files:** also `build.rs`, `assets/`.

**Interfaces:**
- Consumes: `Session` (Task 7), `Prefs`/`Theme` (Task 10), `query` (Task 6).
- Produces:
  - `ui::theme`:
    - `pub struct Palette { pub bg, sidebar, card, border, text, muted, accent, green_bg, green_fg, yellow_bg, yellow_fg, red_bg, red_fg, grey_bg, grey_fg: egui::Color32 }`
    - `pub fn palette(t: Theme) -> Palette`. Base colours come from Global Constraints. Pills:
      - Light: green `#E3EFE0`/`#2F6B3A`, yellow `#F6EBC8`/`#7A5A0B`, red `#F6DEDA`/`#9B2C1F`, grey `#EDEBE4`/`#73726C`.
      - Dark: green `#2C3B2D`/`#9CCB9F`, yellow `#3F3622`/`#E3C077`, red `#45292A`/`#F0A69B`, grey `#3A3935`/`#A6A39A`.
    - `pub fn install_fonts(ctx: &egui::Context)`: Inter as Proportional; `FontFamily::Name("serif")` = Source Serif 4; phosphor icons merged in.
    - `pub fn apply(ctx: &egui::Context, t: Theme)`: corner radius 10 on widgets and windows, 1 px borders, soft window shadow, spacing 8/12.
    - `pub fn heading(ui, text)`: serif, 24 pt.
  - `ui::widgets`:
    - `pub fn due_pill(ui: &mut egui::Ui, pal: &Palette, info: &DueInfo, status: DueStatus, delayed: bool) -> egui::Response`
      - Fully rounded. Text is `fmt_date(due)`, or "Initial pending" when there is no due date.
      - A trailing "· from Excel" or "· manual" for overrides; a ⏸ marker when `delayed`.
      - Hover tooltip: next stage label and status.
    - `pub fn stat_card(ui, pal, label: &str, value: usize, selected: bool) -> egui::Response`
    - `pub fn chip_select<T: PartialEq + Clone>(ui, label: &str, current: &mut Option<T>, options: &[(T, String)])`: a pill-styled dropdown.
    - `pub fn search_box(ui, text: &mut String, hint: &str) -> egui::Response`
    - `pub fn empty_state(ui, title: &str, body: &str)`
  - `ui::app`:
    - `pub enum Page { Summary, Sections, Personnel, Audit, Settings }`
    - `pub struct TrackerApp`, with `pub fn new(cc: &eframe::CreationContext, session: Session, prefs: Prefs, prefs_path: Option<PathBuf>) -> Self` and `pub fn new_for_test(session: Session) -> Self` (no prefs file, light theme).
    - `pub struct Toast { text: String, until: Instant }`
    - `pub enum Modal { Confirm { title: String, body: String, action: PendingAction }, InactivityCountdown }`
    - `pub(crate) struct Ctx<'a> { pub session: &'a mut Session, pub pal: &'a Palette, pub today: NaiveDate, pub now: Ts, pub toasts: &'a mut Vec<Toast>, pub open_detail: &'a mut Option<(Id, Id)> }`: the per-frame context passed to page functions.
    - Each page exposes `pub(crate) fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &mut <Page>State)`.
- Behaviour:
  - **Each frame:**
    - Any input event calls `session.note_activity`.
    - `session.tick(now, Instant::now())`. Events become toasts or banners: `Saved` is a subtle toast; `SaveFailed` sets a persistent red banner; `InactivityWarning` opens a countdown modal with a **Stay** button that calls `note_activity`; `EditingAvailable` changes the banner.
    - `ctx.request_repaint_after(1s)`.
  - **Sidebar (left, collapsible):**
    - "Competency Tracker" in serif.
    - Search box: Ctrl+K focuses it. A results popover uses `query::search`; picking a result navigates.
    - Nav items with phosphor icons; the active item has an accent-tinted background.
    - Bottom: user chip `username · Editing` (accent dot) or `· Viewing`, with Start/Stop editing, and a sun/moon toggle that saves prefs.
  - **Top banner (only when relevant):**
    - "Viewing — {user} is editing (since HH:mm)".
    - "Editing is available — Start editing".
    - "Not saved — retrying".
    - "Read-only: data saved by a newer version".
    - Corrupt-file warning.
  - **Read-only:** while not editing, every mutating control is wrapped in `ui.add_enabled(cx.session.is_editing(), …)`.
  - **Close:** `on_exit` calls `session.on_exit(now)`.
  - **`main.rs`:**
    - `#![windows_subsystem = "windows"]`.
    - Resolve the exe dir via `std::env::current_exe`, then `resolve_data_dir` and `Session::open`. A failure shows a native `rfd::MessageDialog` with the error and exits.
    - `NativeOptions`: 1280×800, min 960×600, app icon.
    - `renderer = Wgpu`. If `run_native` returns `Err`, retry once with `Renderer::Glow`.
  - **`build.rs`:** embed `assets/app.ico` via the `winresource` build-dependency.
- [ ] **Step 1: Ask the user to approve the font downloads (see Global Constraints), place the files in `assets/fonts/`, and create `assets/app.ico`** (a simple monogram generated locally, e.g. with a small script; no download).
- [ ] **Step 2: Write failing smoke tests in `tests/ui_smoke.rs`** using `egui_kittest::Harness` around `TrackerApp::new_for_test(session_on_tempdir)`:
  - `shows_sidebar_nav`: labels "Summary", "Sections", "Personnel", "Audit log" and "Settings" exist.
  - `viewer_banner_when_lock_held`: a lock pre-written by another pid → text contains "is editing".
  - `theme_toggle_switches_dark`: click the toggle → `ctx.style().visuals.dark_mode`.
- [ ] **Step 3: Run `cargo test --test ui_smoke`.** Expected: FAIL.
- [ ] **Step 4: Implement the shell with placeholder page bodies.**
- [ ] **Step 5: Run `cargo test --test ui_smoke`, then `cargo run`.** Expected: tests PASS. The window opens in light mode with the cream background, sidebar and serif title; the toggle switches to dark.
- [ ] **Step 6: Commit** `feat: app shell, theme and launch`

---

### Task 12: Summary page (`ui/summary.rs`)

**Interfaces:**
- Consumes: `query::{Index, due_items, counts, filter_items, group_by_person, group_by_test, DueFilter, StatusFilter}`, `widgets::*`.
- Produces: `pub(crate) struct SummaryState { pub filter: DueFilter, pub group: Grouping /*Person|Test*/, cache: Option<(u64 /*revision*/, NaiveDate, Vec<DueItem>)> }`
- Behaviour (§8.1):
  - Four stat cards; clicking one sets `filter.status`, and clicking again resets it to All.
  - A filter row of chips: section, test system (limited to the chosen section), title, search box.
  - A segmented Person/Test toggle.
  - List as cards. Person grouping: name + title header, rows of test · section · stage · pill. Test grouping: test · section header, rows of staff.
  - Clicking a row sets `open_detail`.
  - Recompute `due_items` only when the revision or date changes.
  - Empty state: "Everyone is up to date" when nothing is due. When there's no data at all: "No data yet — import your Excel tracker from Settings".
- [ ] **Step 1: Add a failing smoke test** `summary_shows_overdue_count_and_row`: seed data with one overdue item for "Agnes" → "Overdue" card shows "1", and "Agnes" appears.
- [ ] **Step 2: Run `cargo test --test ui_smoke summary`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the test (PASS), then `cargo run` against data imported in Task 9's ignored test** (copy the snapshot into a scratch `CompetencyData`). Check the layout by eye in both themes.
- [ ] **Step 5: Commit** `feat: summary page`

---

### Task 13: Sections grid and detail panel (`ui/sections.rs`, `ui/detail.rs`)

**Interfaces:**
- Consumes: `Index`, `schedule::*`, `query::{history, split_history, HISTORY_DEFAULT}`, `ops::{add_completion, edit_record, delete_record, add_delay_note, delete_delay_note, set_skipped, set_assignment, set_due_override}`.
- Produces:
  - `pub(crate) struct SectionsState { pub section: Option<Id>, pub status: StatusFilter, pub test: Option<Id>, pub title: Option<String>, pub text: String, pub sort_by_test: Option<Id> }`
  - `pub(crate) struct DetailState { pub expanded: bool, pub form: DetailForm }`, where `DetailForm` is `None`, `Complete { date: NaiveDate, stage: u32, comment: String }`, `Delay { note: String, expected: Option<NaiveDate> }`, or `EditRecord { id, date, stage, comment }`.
- Behaviour:
  - **Sections (§8.2):**
    - A segmented pill row of non-retired sections in order.
    - `egui_extras::TableBuilder` with a sticky header and the first column frozen (render names in a separate left `ScrollArea` synced to the table's vertical offset, or use `TableBuilder` with `.columns` plus horizontal scrolling and a pinned name column, whichever the egui version supports).
    - Cells:
      - Assigned: pill + small stage label.
      - `Na`: "N/A".
      - Not assigned: faint "—". Clicking a "—" cell in edit mode offers "Assign".
    - Header click sorts by that test's due date.
  - **Detail panel (§8.6):**
    - A right `SidePanel`, about 420 px, animated open.
    - Header: name, test, section, pill.
    - History: `split_history(expanded)`, then a "Show N older entries" link. Each entry: stage, date, comment, "by user · fmt timestamp", an "unconfirmed – from Excel" tag, and a `variance_tag` when `Some`. Edit and delete icons appear in edit mode.
    - Buttons (edit mode only): **Mark complete**, **Add delay note**, **Skip stage**, **Set due date**, **Mark N/A / Unassign**.
    - **Mark complete** defaults: date = today, stage = `info.next_stage_index`.
    - `OpError::FutureDate` shows inline red text: "Completion date can't be in the future".
- [ ] **Step 1: Add failing smoke tests**
  - `grid_shows_pill_and_opens_detail`.
  - `detail_shows_four_then_more`: 6 completions → "Show 2 older entries" visible; after clicking, all 6 are listed.
  - `mark_complete_disabled_when_viewing`.
  - `mark_complete_adds_record`: in edit mode, fill in and save → `session.data().records` grows by one with `by == user`.
- [ ] **Step 2: Run `cargo test --test ui_smoke`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests (PASS), then `cargo run` with the Serology section (14 columns)** to check horizontal scrolling and the frozen names.
- [ ] **Step 5: Commit** `feat: section grid and competency detail panel`

---

### Task 14: Personnel (`ui/personnel.rs`)

**Interfaces:**
- Consumes: `query::{filter_staff, StaffFilter, distinct_titles}`, `ops::{add_staff, update_staff, set_staff_status, delete_staff_permanently, set_assignment, assign_section}`.
- Produces: `pub(crate) struct PersonnelState { pub filter: StaffFilter, pub editing: Option<StaffForm>, pub profile: Option<Id> }`
- Behaviour (§8.3):
  - Status segmented control: Active / Hidden / Removed.
  - Table: name, title, joined, sections (chips), next due (the earliest pill across the person's items).
  - Row menu: Edit, Hide/Unhide, Remove/Restore, and "Delete permanently" (Removed only). Delete opens a modal with "Type {name} to confirm".
  - Editor modal: name, title, joined date picker. Assignments as a collapsible section → test checklist with an "assign whole section" checkbox and an N/A option per test.
  - Profile: all of the person's assignments grouped by section, each with a pill; clicking one opens the detail panel.
- [ ] **Step 1: Add failing smoke tests**
  - `hide_staff_removes_from_summary`.
  - `permanent_delete_requires_typed_name`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: personnel management`

---

### Task 15: Audit log and Settings (`ui/audit.rs`, `ui/settings.rs`)

**Interfaces:**
- Consumes: `query::{filter_audit, AuditFilter, distinct_audit_users}`; `ops` section, test, rule and settings functions; `export::*`; `snapshot::archive_old`; `rfd::FileDialog`.
- Produces: `pub(crate) struct AuditState { pub filter: AuditFilter }` and `pub(crate) struct SettingsState { pub tab: SettingsTab /*Sections|Schedule|General|Import*/, … }`
- Behaviour:
  - **Audit (§8.4):** a virtualised list (`ScrollArea::show_rows`) of timestamp · user · detail. Filter chips: user, date range (two date pickers), section, staff, action type (record / delay / staff / section / settings / import). Search box.
  - **Settings, Sections & tests:** sections list with rename-inline, ↑/↓, retire toggle and delete (disabled with a tooltip when it has records). Expanding a section shows its tests the same way, plus a rule dropdown ("Global" or a named rule).
  - **Settings, Schedule:** edit the global rule's stages (label, interval months, repeats/ordinal on the last stage). Create, rename and delete named rules. A live preview line, e.g. "Initial → +6 mo Semi-annual → +12 mo 1st Annual → every 12 mo".
  - **Settings, General:** number fields for due-soon days, variance tolerance days, inactivity minutes, initial-due days (optional), archive-after days (optional), with an "Archive now" button. Shows the data folder path (read-only text and an "Open folder" button). Changing the folder is done by editing `tracker.config.json`; the app explains this with a one-line hint rather than offering a UI.
  - **Settings, Import:** a button to launch the wizard (Task 16) and the **unconfirmed review list**: a table of unconfirmed records filtered by section, with checkboxes and Confirm selected / Delete selected / Confirm all (Confirm all asks first). Export buttons for status and history (save dialogs).
- [ ] **Step 1: Add failing smoke tests**
  - `rename_section_reflects_in_nav`.
  - `audit_filter_by_user`.
  - `confirm_all_unconfirmed`.
  - `settings_change_recomputes_pills`: `due_soon_days` 30 → 60 turns a 45-day item yellow.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them.** Expected: PASS.
- [ ] **Step 5: Commit** `feat: audit log and settings`

---

### Task 16: Import wizard (`ui/import_wizard.rs`)

**Interfaces:**
- Consumes: `import::excel::read_workbook`, `import::mapping::{propose_merges, split_group, build_import, ImportOptions}`, `Session::replace_data`, `DataDir::import_report`.
- Produces: `pub(crate) struct ImportWizard { step: WizardStep /*PickFile|ReviewMerges|Preview|Done*/, sheets: Vec<RawSheet>, groups: Vec<MergeGroup>, plan: Option<ImportPlan>, error: Option<String> }`
- Behaviour (§12):
  1. **Pick file:** `rfd` open dialog filtered to `.xlsm`/`.xlsx`. Parse errors show inline.
  2. **Review merges:**
     - Groups with more than one member or with conflicts first, then singles.
     - Each group shows members (sheet · raw name) and editable name, joined date and title (prefilled), with conflicts in yellow. A "Split" button calls `split_group`.
  3. **Preview:** report counts. If current data has staff or records, a warning: "This replaces the current data. The previous snapshot stays in the snapshots folder."
  4. **Import:** `replace_data` (edit mode required). Write `report.to_text()` to `import-report.txt`. Show Done with "Open report" and "Review unconfirmed" (navigates to Settings → Import).
- [ ] **Step 1: Add a failing smoke test** `wizard_preview_and_import_from_raw`: inject `sheets` directly (bypassing the file dialog via a `pub(crate) fn load_sheets(&mut self, sheets)`), confirm merges, import → session data has the staff and `import-report.txt` exists.
- [ ] **Step 2: Run it.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run it (PASS), then `cargo run` and import the real workbook end to end.** Spot-check three people against the spreadsheet: Xin Yi (Micro), Frankie (Hematology), Agnes (Serology).
- [ ] **Step 5: Commit** `feat: Excel import wizard`

---

### Task 17: Release build and field verification

- [ ] **Step 1: Run `cargo clippy --all-targets -- -D warnings` and `cargo test`.** Expected: clean, all PASS (ignored tests excluded).
- [ ] **Step 2: Run `cargo build --release`.** Expected: `target\release\competency-tracker.exe`, ideally under 20 MB.
- [ ] **Step 3: Check that the static CRT took effect.** Run `dumpbin /dependents target\release\competency-tracker.exe` if available. Expected: no `VCRUNTIME140.dll` / `MSVCP140.dll`.
- [ ] **Step 4: Field checklist.** The user runs the exe from the real shared folder. Record each result:
  - [ ] Opens with no admin prompt; `CompetencyData\` is created next to the exe.
  - [ ] PC 1 opens → Editing. PC 2 opens → Viewing with "{user} is editing".
  - [ ] An edit on PC 1 appears on PC 2 within about 15 s.
  - [ ] PC 1 left idle: a warning at 9 min, Viewing at 10 min. PC 2 shows "Editing is available".
  - [ ] Kill PC 1's process from Task Manager while it's editing. After about 2 min, a fresh open on PC 2 gets Editing.
  - [ ] Closing the app writes `d MMM yyyy - username.json`.
  - [ ] Unplug the network while editing → red "Not saved" banner. Reconnect → the save succeeds.
  - [ ] Run inside an RDP session or a VM without a GPU → the window renders (wgpu WARP or the glow fallback).
  - [ ] Light mode is the default; dark mode persists for that user after a restart.
- [ ] **Step 5: Commit** `chore: release build verified`
