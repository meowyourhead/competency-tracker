# Competency Tracker — Design

Date: 14 Nov 2026
Status: Draft for approval

## 1. Intent

The lab currently tracks training and competency in `TRAINING & COMPETENCY_ Master Tracking.xlsm`, which has one tab per section. The Excel file has several problems:

- Several people open it at once from a shared folder.
- Each new annual overwrites the previous annual date, so history only survives in free-text remarks ("ESR 2nd Annual DONE on 07 May 2023").
- Staff are listed separately on each tab, with spelling drift ("Shawn" / "Shawn (Weixiang)" / "Weixiang").
- There is no record of who changed what.

The replacement is a **single, standalone Rust `.exe`** that sits in the shared network folder and runs without install or admin rights. Anyone can open it to view. One person at a time can edit, and every change is attributed to that person's Windows username. At a glance it shows who is due for which competency. The look is minimalist and modern, modelled closely on the Claude chat interface.

## 2. Goals

- Replace the six visible section tabs with a structured app. Import all existing data from those tabs, but **not** from the `Staff` tab.
- Keep full competency history per person per test system: Initial, Semi-annual, 1st Annual, 2nd Annual, and so on.
- Calculate the next due date automatically from configurable schedule rules.
- Show colour-coded due pills: green = more than 30 days away, yellow = due within 30 days, red = overdue.
- Provide a summary that shows who is due and which tests are due.
- Make sections, test systems and staff fully configurable.
- Support comments on completions and delay notes on due items.
- Allow one editor at a time with auto-acquire on open, auto-release after 10 minutes idle, and read-only viewing for everyone else.
- Store data as JSON snapshots in a data folder next to the exe. On exit, save to a file named `d MMM YYYY - username.json`.
- Record the acting Windows user on every change.
- Provide filters and search throughout.
- Offer light mode (default) and dark mode with a toggle.

## 3. Non-goals

- No web or mobile access, and no server or database.
- No login or passwords. The Windows username is trusted as identity.
- No Claude or Anthropic logo or name. The look should be similar, not branded.
- No import from the hidden tabs `Comp Assessment Master Tracking` and `1Molecular Biology`. They are stale drafts full of `#REF!`.
- No email or notification sending.
- No multi-editor merging. The single-editor lock is the concurrency model.

## 4. Anti-goals (failure even if it "works")

- **Data loss.** Examples: a crash or network blip losing a session's edits, two editors overwriting each other, or a corrupt half-written JSON becoming "latest".
- **Lock jams.** A crashed editor's lock must not block everyone until someone deletes a file by hand.
- **Clutter.** Adding features must not make the UI busy, and the 4-row-per-person spreadsheet density must not be reproduced.
- **Needing admin rights, an installer, or any runtime** (WebView, .NET, VC++ redistributables beyond what Windows ships).

## 5. Constraints

- **Stack:** Rust, with **egui/eframe** for the GUI. The user chose this for speed and stability, as a pure Rust build. One `.exe`, statically linked (`+crt-static`), with fonts and icons embedded.
- **Location:** the exe runs from a UNC or mapped network path. All writes go to the data folder beside the exe, so no AppData is needed for shared data.
- **Machines:** Windows 10/11 lab PCs, possibly over RDP or in VMs with weak GPU drivers.
- **Identity:** the Windows username comes from the `USERNAME` environment variable, with the `GetUserNameW` API as a fallback.
- **Dates:** displayed as `d MMM yyyy` (for example `2 Dec 2026`), matching the workbook.

## 6. Approaches considered

| Approach | Trade-off | Outcome |
|---|---|---|
| **egui / eframe (pure Rust)** | No runtime dependencies and a fast, small exe that is robust on locked-down PCs. The look is styled by hand, with plainer shadows, text rendering and animation than a browser engine. | **Chosen by user** for speed and stability. |
| Tauri (Rust + HTML/CSS via WebView2) | Could match the Claude chat look almost exactly. It depends on the WebView2 runtime being present. | Rejected: the user wants pure Rust. |
| Slint | Sits between the two, but adds licensing terms and a smaller ecosystem without solving either trade-off. | Dropped. |

## 7. Look and feel (decided)

The UI should feel like Claude chat, but without its logo or name.

- **Layout:** a collapsible **left sidebar** for navigation, and a calm main content column with generous whitespace.
- **Sidebar:**
  - At the top: the app name ("Competency Tracker") and the global search field.
  - Nav items: Summary, Sections, Personnel, Audit log, Settings.
  - At the bottom: the user chip (Windows username, plus an edit/view status badge) and the light/dark toggle.
- **Palette (starting values; tune by eye):**
  - Light (default): background `#FAF9F5`, sidebar `#F0EEE6`, cards `#FFFFFF`, borders `#E8E6DC`, text `#1F1E1D`, muted text `#73726C`, accent (terracotta) `#C96442`.
  - Dark: background `#262624`, sidebar `#1F1E1D`, cards `#30302E`, borders `#3E3E38`, text `#FAF9F5`, muted text `#A6A39A`, accent `#D97757`.
- **Typography:** an open-licence serif for page titles and headings (for example Source Serif 4) and a clean sans for UI and body text (for example Inter), both embedded. Claude's own fonts are proprietary.
- **Shapes:** rounded corners (about 8–12 px) on cards, inputs and buttons. Thin 1 px borders and subtle shadows. Pill-shaped tags and dropdowns.
- **Due pills:** fully rounded, with a muted tinted fill and darker text in the matching hue. The palette has a variant for each theme.
  - Green: on track.
  - Yellow: due within 30 days.
  - Red: overdue.
  - Grey: not started, or no due date.
  - A small 💬 or ⏸ marker shows that a delay note is attached.
- **Theme choice:** light is the default. The toggle is remembered **per user, per PC** in `%APPDATA%\CompetencyTracker\prefs.json`. This needs no admin rights.
- **Overlays:** dialogs are centred modal cards with a dimmed backdrop. Toasts appear bottom-centre for confirmations ("Saved").

## 8. Screens

### 8.1 Summary (home)

- **Stat cards:** Overdue, Due within 30 days, On track, Delayed (with note). Clicking a card filters the list below it.
- **Due list:** sorted by urgency (most overdue first). Each row shows: staff name · title · section · test system · next stage · due pill · delay-note indicator.
- **Group-by toggle:** *Person* (one card per person, listing their due tests) or *Test system* (one card per test, listing the staff due).
- **Filters:** section, test system, title/role, status (overdue / due soon / delayed / all), and a search box.
- Clicking a row opens the **competency detail panel** (8.6).

### 8.2 Sections

- **Section tabs:** a pill or segment control lists the sections in their configured order.
- **Grid:** rows are the active staff assigned to the section. Columns are its test systems. Each cell shows the **next-due pill** and a small stage label (for example "2nd Annual").
- **Cell states:** unassigned cells show a faint "—". Cells marked "N/A" show "N/A".
- Clicking a cell opens the detail panel.
- **Filters:** status, test system, title, and a search by name. Each column can be sorted by due date.
- The grid uses a sticky header and a sticky name column, so it stays readable with 14+ test columns (Serology has 14).

### 8.3 Personnel

- **Master staff list:** name, title, joined date, status, sections assigned, and next due.
- **Add / edit staff:** name, title, joined date, and assignments as a checklist of sections → test systems, with "assign whole section".
- **Status values:**
  - **Active:** shown everywhere.
  - **Hidden:** hidden from every view and count, with data kept. Restorable from a "Hidden" filter.
  - **Removed:** for staff who left. Excluded from views and counts, data kept for audit, listed under a "Removed" filter, and restorable.
  - **Delete permanently:** only from the Removed filter. It needs a typed confirmation and is recorded in the audit log.
- **Person profile:** opens from a row. It shows all their assignments across sections with pills, so one person can be reviewed in one place.
- **Filters:** status, section, title, and search.

### 8.4 Audit log

- One entry per change: timestamp · Windows user · action · a human-readable description (for example "Marked *Cell-DYN Ruby* 2nd Annual complete for Agnes on 12 Oct 2026").
- **Filters:** user, date range, section, staff, and action type. Search over the descriptions.

### 8.5 Settings

- **Sections and test systems:** add, rename, reorder (drag or ↑/↓), and retire. Retiring is preferred over deleting. Delete is allowed only when there are no records, and needs confirmation.
- **Schedule rules** (see 9):
  - Edit the global rule.
  - Create named rules.
  - Assign a rule override to specific test systems. The test list shows "Global" or the rule name.
- **General:** due-soon threshold (default 30 days), **date variance tolerance** (default 1 month; see 9), inactivity timeout (default 10 minutes), and data folder location (default `.\CompetencyData` next to the exe; stored in a small `tracker.config.json` beside the exe).
- **Import from Excel** (see 11).
- **Export:** to `.xlsx`, either current status or full history. This is useful for audits and inspections.

### 8.6 Competency detail panel (slide-in from the right)

The panel is for one staff member and one test system. It shows:

- **Header:** name, test system, section, the current next-due pill, and the stage it is for.
- **History:** the **4 most recent entries** by default, newest first. A "Show N older entries" link expands the rest. Each entry shows stage, completion date, comment, and "by `username` · timestamp". Entries can be edited or deleted (in edit mode), and every edit is audited.
- **Actions (edit mode only):**
  - **Mark complete:** a date picker (defaults to today), the stage (preselected as the next expected stage, but changeable), and an optional comment.
  - **Add delay note:** a reason, plus an optional expected completion date. It attaches to the current due item, is shown on the Summary, and stays in history once that stage is completed.
  - **Mark N/A / Not assigned** for this test.

## 9. Schedule rules (decided)

A **rule** is an ordered list of stages. Each stage has a label and an interval counted **from the previous stage's actual completion date**. The last stage repeats indefinitely while the staff member is active.

The default global rule comes from the workbook's own data. For example, Xin Yi in Microbiology: Initial 2 May 2024 → Semi-annual 2 Nov 2024 → Next Annual 2 Nov 2025.

| Stage | Due |
|---|---|
| Initial | — (see below) |
| Semi-annual | 6 months after Initial |
| 1st Annual | 12 months after Semi-annual |
| Nth Annual (repeats) | 12 months after previous annual |

Repeating annuals are labelled ordinally: 1st, 2nd, 3rd Annual, and so on.

- **Scope of rules:** a rule can apply **globally**, or as an override **per test system**. Changing a rule recalculates due dates; it does not change history.
- **Next due** = the next stage after the latest completed stage, due on the latest completion date plus that stage's interval.
- **One rule for every test.** The user confirmed that all tests follow Initial → 6 months → yearly. The global rule applies everywhere by default. Per-test overrides exist in Settings, but none are created at import.
- **Skipped stages:** if a stage is marked skipped (for example a "-" in the workbook's Semi-annual row), the next stage counts from the last real completion. For example, 1st Annual is then due 12 months after Initial.
- **Manual due override:** a due item can carry a manually set due date, used for the Excel seed below. It is shown with a small "from Excel" or "manual" tag. Any later completion clears it, and the rules take over again.
- **Initial not done:** the cell shows a grey "Initial pending" pill. There is an optional setting, "Initial due N days after assignment" (off by default).
- **Out-of-order or late completions:** the user picks the stage when marking complete. The next due is always calculated from the most recent completion.
- **Date variance tolerance** (Settings, default **1 month**). The user's direction: competencies are rarely done exactly on their due date, and that is normal.
  - A completion dated within the tolerance of its due date (early or late) gets **no tag**.
  - Beyond the tolerance, the history entry shows a quiet "late by 47 days" or "early by 40 days" tag.
  - This is information only. The next due date still counts from the actual completion date.
  - The same tolerance governs the Excel import comparison (section 12).
- **Month arithmetic:** clamp to the end of the month (31 Aug + 6 months = 28/29 Feb).

## 10. Editing lock, saving, and data folder (decided)

### 10.1 Data folder layout (default `<exe dir>\CompetencyData\`)

```
CompetencyData\
  snapshots\
    14 Nov 2026 - jtan.json
    13 Nov 2026 - agnes.json
    ...
  edit.lock
```

### 10.2 Snapshots

- Each snapshot is a **complete** copy of the data. On startup and on refresh, the app loads the snapshot with the newest `saved_at` field inside the JSON. File modified times on network shares are not reliable enough.
- The user's required filename format is `d MMM yyyy - username.json`.
  - **Same user, same day:** the editor writes to and overwrites that day's file. This is safe because the newest snapshot is always a full superset.
  - **Different users** get different files.
- **Autosave:** while editing, the app writes to the current session's file a few seconds after each change, debounced at about 3 s. It also writes when edit mode ends (inactivity timeout or a manual stop) and on exit. This protects against crashes and network drops while still producing the named file on exit.
- **Atomic writes:** write to `<name>.json.tmp`, flush, then rename over the target. On load, ignore `.tmp` files and any JSON that fails to parse; fall back to the next-newest snapshot and show a warning.
- **Write failures** (for example, the network is down) show a persistent red banner ("Not saved — retrying"). The app keeps retrying and does not drop edit mode silently.
- **Retention:** keep all snapshots, since they are small. A Settings option, "Move snapshots older than N days to `snapshots\archive\`", is off by default.

### 10.3 Lock (`edit.lock`)

- **Acquire:** create the file with create-new semantics, so it fails if the file exists. The file holds the username, machine name, process ID, acquired time and heartbeat time.
- **Heartbeat:** the editor rewrites the heartbeat every 30 s. A lock whose heartbeat is **older than 2 minutes** is stale (crashed PC or lost network). The next person can take it over automatically, and the takeover is noted in the audit log.
- **On open:** if there is no lock, or only a stale one → acquire it and start in **edit mode**. Otherwise start in **view mode**, with a banner: "Viewing — *agnes* is editing (since 10:42)".
- **View mode:**
  - Every ~10 s the app checks for a newer snapshot and reloads it automatically, keeping the user's current screen and filters.
  - When the lock frees up, the banner changes to "Editing is available — **Start editing**". It does not auto-grab, so viewers aren't surprised.
- **Inactivity:**
  - After 9 minutes with no mouse or keyboard input in the app, a countdown toast appears: "You'll leave edit mode in 60 s — Stay".
  - At 10 minutes the app saves, releases the lock and switches to view mode.
  - The timeout is configurable.
- **Manual release:** a "Stop editing" button in the user chip.
- **Release on exit:** save, then delete the lock. The lock is released only if it still belongs to this user and process.
- **Safety check before every save:** re-read the lock. If someone else now owns it (for example after a stale takeover during a long network outage), **do not write**. Show a dialog and offer to export unsaved changes to a separate file in `snapshots\conflicts\`.

## 11. Data model (JSON)

```jsonc
{
  "schema_version": 1,
  "saved_at": "2026-11-14T15:02:11+08:00",
  "saved_by": "jtan",
  "settings": { "due_soon_days": 30, "variance_tolerance_days": 30, "inactivity_minutes": 10, "initial_due_days": null },
  "rules": [ { "id": "r-global", "name": "Global", "stages": [
      { "label": "Initial", "interval_months": null },
      { "label": "Semi-annual", "interval_months": 6 },
      { "label": "Annual", "interval_months": 12, "repeats": true, "ordinal": true } ] } ],
  "default_rule_id": "r-global",
  "sections": [ { "id": "s-chem", "name": "Chemistry & Immunology", "order": 0, "retired": false,
      "tests": [ { "id": "t-...", "name": "Abbott Architect System (c4100, i1000SR)", "order": 0,
                   "retired": false, "rule_id": null } ] } ],
  "staff": [ { "id": "p-...", "name": "Agnes", "title": "MT", "joined": "2020-05-18",
      "status": "active",                      // active | hidden | removed
      "assignments": { "t-...": "assigned" } }], // assigned | na   (absent = not assigned)
  "records": [ { "id": "c-...", "staff_id": "p-...", "test_id": "t-...",
      "stage_index": 3, "stage_label": "2nd Annual", "completed_on": "2025-07-18",
      "comment": "", "by": "jtan", "at": "2026-11-14T15:01:02+08:00",
      "imported": false, "unconfirmed": false } ],
  "skipped": [ { "staff_id": "p-...", "test_id": "t-...", "stage_index": 1, "by": "...", "at": "..." } ],
  "due_overrides": [ { "staff_id": "p-...", "test_id": "t-...", "due_on": "2027-03-24",
      "source": "excel",                       // excel | manual
      "by": "...", "at": "..." } ],
  "delay_notes": [ { "id": "d-...", "staff_id": "p-...", "test_id": "t-...",
      "for_stage_index": 4, "note": "On maternity leave", "expected_on": "2027-01-10",
      "by": "jtan", "at": "..." } ],
  "audit": [ { "at": "...", "by": "jtan", "action": "record.add", "detail": "...", "refs": {} } ]
}
```

- All IDs are stable random strings, so renaming a section, test or person never breaks links.
- `stage_index` counts from 0 = Initial. The label is stored as well, for readability and for history after a rule changes.

## 12. Excel import (decided)

Import is a one-time wizard under Settings → Import from Excel. It uses the `calamine` crate and reads the `.xlsm` read-only.

- **Tabs imported:** Chemistry & Immunology, Hematology, Urinalysis, Molecular Biology, Microbiology, Serology. These become the six sections, in that order, keeping the workbook's tab names. The user can rename them afterwards, for example to "Haematology". Skipped: Staff, and the two hidden tabs.
- **Layout per tab:**
  - Row 3 holds the test-system headers. Newlines in headers are collapsed to spaces.
  - Each staff member is a 4-row block: Initial / Semi-annual / Annual / Next Annual Due.
  - Columns are name, position and joined date. In Molecular and Micro the joined date sits in a different column; detect it by header text.
  - Each test column is followed by a tick column. The tick is a Marlett `a` and means "completed on that date".
  - Then come Remark(s) and Updated. The "Upcoming", "Due" and "Active personnel count" helper columns are ignored.
- **Cell mapping** (the unticked-date rules were confirmed by the user):
  - Date + tick → a completion record (`imported: true`, `by: "excel-import"`).
  - **Past** date without a tick (including columns that have no tick column, such as the SARS-CoV-2 TaqPath columns) → a completion record flagged `unconfirmed: true`, shown as "unconfirmed – from Excel". Settings → Import has a **bulk review list** where these can be confirmed or deleted, one at a time or all together.
  - **Future** date without a tick → a planned date. This becomes a manual due override for that stage, not a completion.
  - `Not Done` or empty → no record.
  - `-` in a single stage row → that stage is marked skipped for the person and test.
  - `-`, `NA` or `N/A` in **every** stage row → assignment `na`.
  - **Assignment:** a staff member is assigned to a test if any date appears in that test's column for them.
  - `Not assigned to …`, `No longer in … section`, `No longer working in …` → not assigned.
  - `Left the company`, `No longer working at Eurofins` → staff status `removed`.
  - Other text (for example `Alinity m Cert`, `CAP`, `TO RETRAIN`, `printed`) and the Remark(s) column → a comment on an imported note for that staff and test (or staff and section). This way no text is dropped.
  - **"Next Annual Due" row:** the builder check found that only about half of these dates match the rule calculation. The user confirmed all tests use the same pattern, so the gaps come from annuals that were typed over rather than from different schedules. Handling:
    - Calculate the due date from the imported records.
    - If the workbook's date is within the **variance tolerance** (default 1 month) of the calculated date, the calculated date stands, with no tag and no report entry.
    - If the workbook's date is **later by more than the tolerance**, use it as a manual due override ("from Excel"). It implies a newer completion that was never recorded, so staff the lab considers current don't show red on day one. The item is listed in the import report.
    - If the workbook's date is earlier by more than the tolerance, the calculated date stands, and the item is listed in the import report.
    - `=EDATE(x,36)` formulas in Urinalysis are read only as values; no 3-year rule is created.
- **Stages from the Annual row:** the Annual row holds only the latest annual. Import it as "1st Annual" by default. Where the remarks name an ordinal ("2nd Annual", "3rd e411 (07/8/2024)"), use that ordinal for the matching test when it can be matched confidently. Otherwise leave "1st Annual" and flag the item for review.
- **Placeholder rows:** skip rows named `Buffer Cells` and `Buffer Row`.
- **Duplicate headers:** Molecular has "SARS-CoV-2 detection by TaqPath" three times. Import them with "(2)" and "(3)" suffixes for the user to rename.
- **Cross-section columns:** the Hematology tab has an "Abbott Architect" column under the Chemistry header. Merge it into the Chemistry test of the same name. If both tabs have a record for the same date, keep one.
- **Staff merge:** names are matched across tabs, ignoring case, whitespace and bracketed aliases ("Christine (Li Yun)" ↔ "Li Yun"; "Shawn (Weixiang)" ↔ "Weixiang"). A **review screen** lists the proposed merges and unmatched names before anything is written. The user confirms or splits each one.
  - **Same name within one tab** (for example two "Shermaine" blocks in Urinalysis, two "Hannah" blocks in Serology) → **the same person**; their records are combined. The user confirmed there are no two people with the same name.
  - **Conflicting joined date or title across tabs:** use the earliest joined date and the most frequent title, and show the conflict on the review screen so the user can change it.
  - **"Not assigned to …" / "No longer in … section" remarks** on a block that has records → keep the records, but leave the person unassigned from that section going forward.
- **Result:** the first snapshot, plus an "Import report" (counts, flagged items and merges) saved as `CompetencyData\import-report.txt`.

## 13. Search and filters

- **Global search** (sidebar, Ctrl+K): fuzzy search over staff, test systems and sections. Picking a result jumps to that person's profile or that section grid.
- Each screen has the filters listed in section 8. Filter state is kept per screen during the session.

## 14. Technical choices (builder may refine)

- **Crates:** `eframe`/`egui` + `egui_extras` (tables, date picker), `serde`/`serde_json`, `chrono`, `calamine` (import), `rust_xlsxwriter` (export), `uuid` or `nanoid`, `windows-sys` (username, machine name), `fuzzy-matcher`.
- **Renderer:** eframe's **wgpu** backend (DX12, which falls back to the WARP software adapter on GPU-less RDP or VM sessions). This is preferred over glow/OpenGL, which can fail on such machines. Verify on an RDP session.
- **Release build:** `opt-level=3`, `lto`, `codegen-units=1`, `strip`, `panic=abort`, static CRT, Windows GUI subsystem (no console), and an embedded app icon.
- **Business logic** (scheduling, due status, import parsing, lock and snapshot handling) lives in a UI-free module with unit tests. The UI is a thin layer on top.

## 15. Left to the builder

- Exact spacing, sizes, colour tuning and icons, within section 7.
- Exact fuzzy-match algorithm and thresholds for the staff merge, as long as the review screen exists.
- Internal module layout and state management.
- The snapshot polling interval (about 10 s) and debounce timings, within the ranges given.
