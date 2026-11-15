use std::path::Path;

use chrono::{Datelike, NaiveDate};
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook, XlsxError};

use crate::model::Data;
use crate::query::{due_items, Index};
use crate::schedule::DueStatus;

const DATE_FORMAT: &str = "d mmm yyyy";

enum Cell {
    Text(String),
    Date(Option<NaiveDate>),
}

fn text(s: impl Into<String>) -> Cell {
    Cell::Text(s.into())
}

fn write_sheet(
    name: &str,
    headers: &[&str],
    widths: &[f64],
    rows: Vec<Vec<Cell>>,
    path: &Path,
) -> Result<(), XlsxError> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    let bold = Format::new().set_bold();
    let date_fmt = Format::new().set_num_format(DATE_FORMAT);
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &bold)?;
        ws.set_column_width(c as u16, widths[c])?;
    }
    for (r, row) in rows.iter().enumerate() {
        let r = r as u32 + 1;
        for (c, cell) in row.iter().enumerate() {
            match cell {
                Cell::Text(s) => {
                    ws.write_string(r, c as u16, s)?;
                }
                Cell::Date(Some(d)) => {
                    let dt =
                        ExcelDateTime::from_ymd(d.year() as u16, d.month() as u8, d.day() as u8)?;
                    ws.write_datetime_with_format(r, c as u16, &dt, &date_fmt)?;
                }
                Cell::Date(None) => {}
            }
        }
    }
    ws.set_freeze_panes(1, 0)?;
    wb.save(path)
}

fn status_text(s: DueStatus) -> &'static str {
    match s {
        DueStatus::Overdue => "Overdue",
        DueStatus::DueSoon => "Due soon",
        DueStatus::OnTrack => "On track",
        DueStatus::NotStarted => "Not started",
    }
}

/// Current status of every assigned person/test pair, as shown on the Due list.
pub fn export_status(data: &Data, today: NaiveDate, path: &Path) -> Result<(), XlsxError> {
    let idx = Index::build(data);
    let rows = due_items(data, &idx, today)
        .into_iter()
        .map(|i| {
            vec![
                text(i.section_name),
                text(i.test_name),
                text(i.staff_name),
                text(i.title),
                text(i.info.next_stage_label),
                Cell::Date(i.info.due_on),
                text(status_text(i.status)),
                text(i.delay_note.map(|n| n.note).unwrap_or_default()),
            ]
        })
        .collect();
    write_sheet(
        "Status",
        &[
            "Section",
            "Test system",
            "Staff",
            "Title",
            "Next stage",
            "Due",
            "Status",
            "Delay note",
        ],
        &[16.0, 22.0, 22.0, 20.0, 16.0, 13.0, 12.0, 40.0],
        rows,
        path,
    )
}

/// Every completion record, for audits and inspections.
pub fn export_history(data: &Data, path: &Path) -> Result<(), XlsxError> {
    let section_order = |test_id: &str| data.test(test_id).map(|(s, t)| (s.order, t.order));
    let staff_name = |id: &str| data.staff(id).map(|s| s.name.clone()).unwrap_or_default();
    let mut recs: Vec<_> = data.records.iter().collect();
    recs.sort_by(|a, b| {
        section_order(&a.test_id)
            .cmp(&section_order(&b.test_id))
            .then_with(|| {
                staff_name(&a.staff_id)
                    .to_lowercase()
                    .cmp(&staff_name(&b.staff_id).to_lowercase())
            })
            .then_with(|| a.completed_on.cmp(&b.completed_on))
    });
    let rows = recs
        .into_iter()
        .map(|r| {
            let (section, test) = data
                .test(&r.test_id)
                .map(|(s, t)| (s.name.clone(), t.name.clone()))
                .unwrap_or_default();
            vec![
                text(section),
                text(test),
                text(staff_name(&r.staff_id)),
                text(r.stage_label.clone()),
                Cell::Date(Some(r.completed_on)),
                text(r.comment.clone()),
                text(if r.unconfirmed { "Yes" } else { "" }),
                text(r.by.clone()),
                text(r.at.format("%-d %b %Y %H:%M").to_string()),
            ]
        })
        .collect();
    write_sheet(
        "History",
        &[
            "Section",
            "Test system",
            "Staff",
            "Stage",
            "Completed on",
            "Comment",
            "Unconfirmed",
            "By",
            "Recorded at",
        ],
        &[16.0, 22.0, 22.0, 16.0, 14.0, 40.0, 13.0, 14.0, 20.0],
        rows,
        path,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use calamine::{open_workbook, Reader, Xlsx};
    use chrono::TimeZone;

    fn ts() -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 14, 9, 5, 0)
            .unwrap()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn sample() -> Data {
        let mut data = Data::new_empty("jtan", ts());
        data.sections[0].tests.push(TestSystem {
            id: "t-chem".into(),
            name: "Analyser A".into(),
            order: 0,
            retired: false,
            rule_id: None,
        });
        data.sections[1].tests.push(TestSystem {
            id: "t-haem".into(),
            name: "Analyser B".into(),
            order: 0,
            retired: false,
            rule_id: None,
        });
        for (id, name) in [("p-1", "Zed"), ("p-2", "Amy")] {
            let mut assignments = std::collections::BTreeMap::new();
            for t in ["t-chem", "t-haem"] {
                assignments.insert(
                    t.to_string(),
                    Assignment {
                        state: AssignState::Assigned,
                        since: d(2025, 1, 1),
                    },
                );
            }
            data.staff.push(Staff {
                id: id.into(),
                name: name.into(),
                title: "Technologist".into(),
                joined: None,
                status: StaffStatus::Active,
                assignments,
            });
        }
        let rec = |id: &str,
                   staff: &str,
                   test: &str,
                   idx: u32,
                   label: &str,
                   on: NaiveDate,
                   unconfirmed: bool| Record {
            id: id.into(),
            staff_id: staff.into(),
            test_id: test.into(),
            stage_index: idx,
            stage_label: label.into(),
            completed_on: on,
            comment: "ok".into(),
            by: "jtan".into(),
            at: ts(),
            imported: false,
            unconfirmed,
        };
        // Deliberately out of order.
        data.records.push(rec(
            "r-1",
            "p-1",
            "t-haem",
            0,
            "Initial",
            d(2026, 1, 5),
            false,
        ));
        data.records.push(rec(
            "r-2",
            "p-2",
            "t-chem",
            1,
            "Semi-annual",
            d(2026, 6, 1),
            true,
        ));
        data.records.push(rec(
            "r-3",
            "p-2",
            "t-chem",
            0,
            "Initial",
            d(2025, 12, 1),
            false,
        ));
        data
    }

    fn read(path: &Path, sheet: &str) -> Vec<Vec<String>> {
        let mut wb: Xlsx<_> = open_workbook(path).unwrap();
        let range = wb.worksheet_range(sheet).unwrap();
        range
            .rows()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn export_status_writes_file() {
        let data = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.xlsx");
        export_status(&data, d(2026, 11, 14), &path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 0);

        let rows = read(&path, "Status");
        assert_eq!(
            rows[0],
            [
                "Section",
                "Test system",
                "Staff",
                "Title",
                "Next stage",
                "Due",
                "Status",
                "Delay note"
            ]
        );
        // 2 staff x 2 assigned tests.
        assert_eq!(rows.len(), 1 + 4);
        let statuses: Vec<&str> = rows[1..].iter().map(|r| r[6].as_str()).collect();
        assert!(statuses
            .iter()
            .all(|s| ["Overdue", "Due soon", "On track", "Not started"].contains(s)));
        // Zed / Analyser B: Initial done 5 Jan 2026 -> Semi-annual due 5 Jul 2026, overdue.
        let zed = rows
            .iter()
            .find(|r| r[2] == "Zed" && r[1] == "Analyser B")
            .unwrap();
        assert_eq!(
            (zed[4].as_str(), zed[6].as_str()),
            ("Semi-annual", "Overdue")
        );
        // Never-started pair has no due date and an empty delay note.
        let none = rows
            .iter()
            .find(|r| r[2] == "Zed" && r[1] == "Analyser A")
            .unwrap();
        assert_eq!(
            (none[5].as_str(), none[6].as_str(), none[7].as_str()),
            ("", "Not started", "")
        );
    }

    #[test]
    fn export_status_due_is_a_real_date() {
        let data = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.xlsx");
        export_status(&data, d(2026, 11, 14), &path).unwrap();
        let mut wb: Xlsx<_> = open_workbook(&path).unwrap();
        let range = wb.worksheet_range("Status").unwrap();
        let found = range
            .rows()
            .skip(1)
            .any(|r| matches!(r[5], calamine::Data::DateTime(_)));
        assert!(found, "Due should be an Excel date cell");
    }

    #[test]
    fn export_history_rows() {
        let data = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.xlsx");
        export_history(&data, &path).unwrap();

        let rows = read(&path, "History");
        assert_eq!(
            rows[0],
            [
                "Section",
                "Test system",
                "Staff",
                "Stage",
                "Completed on",
                "Comment",
                "Unconfirmed",
                "By",
                "Recorded at"
            ]
        );
        assert_eq!(rows.len(), 1 + 3);
        // Section order (Chemistry, Haematology), then completed_on.
        assert_eq!(
            (
                rows[1][0].as_str(),
                rows[1][3].as_str(),
                rows[1][6].as_str()
            ),
            ("Chemistry", "Initial", "")
        );
        assert_eq!(
            (rows[2][3].as_str(), rows[2][6].as_str()),
            ("Semi-annual", "Yes")
        );
        assert_eq!(
            (rows[3][0].as_str(), rows[3][2].as_str()),
            ("Haematology", "Zed")
        );
        assert_eq!(rows[1][8], "14 Nov 2026 09:05");
    }
}
