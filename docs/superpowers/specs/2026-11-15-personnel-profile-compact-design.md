# Compact personnel profile + section colours everywhere

## Intent
The per-person profile (Personnel → click a name) wastes space: one full-width
card per section, stacked, with each test's name at the far left and its pill
at the far right. Make it compact, clean and easy to scan, and make section
colours (already set in Settings, used only on the Sections page) show
consistently across the app, including the History page.

## Goals
- Two profile views with a toggle: **Cards** (section cards in a responsive
  grid) and **Table** (one compact table).
- Header additions: **status strip**, **next-up line**, **last completed date**
  per test.
- Section colour shown wherever a section is named.

## Non-goals
- Redesigning the detail side panel (only its subtitle gets the section chip).
- Per-section progress, recent activity, per-person export (offered, not chosen).

## Anti-goals
- Adding clutter: colour is a dot + soft tint, never loud fills; text stays readable in light and dark.

## Approaches considered
- **A. Section cards in a grid** – keeps grouping, cuts scroll.
- **B. One compact table** – densest, best for scanning.
- **C. Cap width at ~700px** – least work, leaves the page long and half empty.
User picked **A and B with a toggle**.

## Design
### Profile header
- Back link; name with Edit on the right; muted meta line (title · joined · status) — unchanged.
- **Status strip**: four small clickable chips — Overdue, Due soon, Up to date,
  Not done — each with its status colour and the count of this person's
  *assigned* tests. Clicking one filters both views to that status; clicking it
  again clears. Zero counts show dimmed. *(My call: the filter is not saved.)*
- **View toggle** (`Cards | Table`) on the same row, right-aligned, using the
  existing `segmented` widget. *(My call: remembered in prefs.json.)*
- **Next up**: one line naming the earliest-due assigned test — test · section
  chip · stage — and how far away / overdue. Clicking it opens the detail panel.
  Hidden when nothing has a due date.

### Cards view
- Cards flow in a grid: as many columns as fit at a ~320px minimum card width
  (max 3), row-major, cards top-aligned per row.
- Each card: a 3px top edge in the section colour, dot + section name, then one
  row per test: name (truncates, full on hover) and pill next to it within the
  card; a muted second line "Last done 4 Nov 2025" (or "Never done").
- N/A tests: muted "N/A", no last-done line. Hidden while a status filter is on.

### Table view
- One card holding a grid: Section | Test | Last done | Next due.
- Rows grouped by section order; the section chip appears on the first row of
  each group. *(My call)* Clicking the "Next due" header sorts all rows by
  soonest due instead (chip then shows on every row); clicking again restores grouping.
- Clicking a test name or pill opens the detail panel, as today.

### Section colours across the app
Shared helpers (in `widgets.rs`): look up a section's colour by id (position in
`sections_sorted`, via `Section::color_rgb`) and draw a **section chip**
(soft tint of the colour + dot + name). Applied in:
- Personnel list: Sections column chips use each section's tint instead of grey.
- Profile: card top edge/dot; table section chips; next-up line.
- Detail panel: subtitle shows the section chip.
- Summary: dot before each name in "By section"; register Sections column as coloured chips.
- History page: each row shows the section chip when the entry's `refs`
  carry a `section_id` that still exists.
- Staff editor: dot beside each section heading.
Tint strengths follow the Sections page (≈0.08 fill, solid dot).

## Constraints
- egui; reuse `due_pill`, `segmented`, `status_colors`, theme `Palette`.
- History rows are fixed-height (44px) virtualised; the chip must fit on the first line.
- After the change: release build and copy `competency-tracker.exe` into the project root.

## Left to the builder
- Exact spacing, chip sizes, breakpoints within the stated limits.
- How the grid computes column count at different display scales.
