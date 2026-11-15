# Competency Tracker - field checklist

This is a short guide for setting the app up on the lab's shared folder and checking that everything works. Tick each box as you go and note anything odd.

## 1. Installing

1. Copy `competency-tracker.exe` into the shared folder where the team will use it. No installer and no administrator rights are needed.
2. Double-click it. The first time, the app asks whether to create a data folder called `CompetencyData` next to the exe. Choose OK (Cancel closes the app and creates nothing).
3. Optional: to keep the data somewhere else, put a small file called `tracker.config.json` next to the exe containing:
   `{"data_dir": "D:\\Some\\Other\\Folder"}`
   A path that is not a full path is read relative to the exe's folder.

## 2. Bringing in the Excel workbook

1. Open the app and go to Settings, then Import & export, then "Import from Excel...".
2. Pick the workbook (`TRAINING & COMPETENCY_ Master Tracking.xlsm`). Your original file is never changed.
3. The app keeps a backup of anything that was there before the import in `snapshots\archive`, and writes a summary to `import-report.txt`.

## 3. Where your data lives

Everything is inside the `CompetencyData` folder:

- `snapshots` - the saved data files, named like `2 Dec 2026 - jtan.json` (date and user).
- `snapshots\archive` - older snapshots that were tidied away, and the backups taken before each Excel import.
- `snapshots\conflicts` - edits that were kept aside because someone else took over the edit lock (for example after a long network drop) before they could be saved. Nothing is merged: open the file and re-enter anything that is missing.
- `edit.lock` - shows who is currently editing. It goes away when they close the app.
- `import-report.txt` - what the last Excel import did.

If a save fails while you close the app, it offers Retry or Quit anyway. Only after "Quit anyway" does it write a rescue copy of your unsaved work, in `%LOCALAPPDATA%\CompetencyTracker\rescue` on your own PC.

## 4. Checks to tick off

### Starting up
- [ ] The app opens with no administrator prompt, and `CompetencyData` appears next to the exe.
- [ ] The first-run folder question shows up, and choosing Cancel creates nothing.
- [ ] Light mode is the default. Switch to dark, close and reopen: dark is remembered for that user.
- [ ] Light and dark both look right (text readable, nothing washed out).

### Two people at once
- [ ] PC 1 opens the app and is Editing.
- [ ] PC 2 opens it and is Viewing, with a message that PC 1's user is editing.
- [ ] An edit on PC 1 shows up on PC 2 within about 15 seconds.
- [ ] Leave PC 1 idle: a warning appears at 9 minutes, and at 10 minutes it switches to Viewing. PC 2 then shows "Editing is available".
- [ ] While PC 1 is editing, end its process in Task Manager. After about 2 minutes, opening the app fresh on PC 2 gives Editing.
- [ ] While PC 1 is editing, minimise its window for 3 minutes. PC 2 (restart it if needed) stays Viewing; restore PC 1 and it is still Editing.
- [ ] Set PC 2's clock 5 minutes ahead and open the app while PC 1 is editing: PC 2 must stay Viewing and must not take over. Set the clock back afterwards.

### Saving
- [ ] Closing the app writes a file named like `d MMM yyyy - username.json` in `snapshots`.
- [ ] Unplug the network while editing: a red "Not saved" banner appears. Reconnect: the save goes through.
- [ ] Make a change, unplug the network and close the app: it asks Retry or Quit anyway. Reconnect and choose Retry: it saves and closes. Repeat, but choose Quit anyway: it says where the rescue copy was written.

### Display
- [ ] Run it inside a remote desktop session or a virtual machine with no graphics card: the window still draws properly.
- [ ] The Serology grid (14 columns) scrolls sideways while the names stay in place on the left.

### The real workbook import
- [ ] Spot-check Xin Yi (Microbiology), Frankie (Hematology) and Agnes (Serology) against the spreadsheet: same tests, dates and due dates.
- [ ] Review the "unconfirmed - from Excel" list in Settings, then Import & export, and confirm or correct the entries.

## 5. If something looks wrong

Write down what you did, what you expected and what happened, and keep a copy of the `CompetencyData` folder (especially `import-report.txt` and `conflicts`) before changing anything.
