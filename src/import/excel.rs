use std::collections::HashSet;
use std::path::Path;

use calamine::{open_workbook_auto, Data, Reader};
use chrono::NaiveDate;

use crate::dates::{fmt_date, parse_dmy};
use crate::import::fills::read_due_fills;

pub const SHEETS: [&str; 6] = [
    "Chemistry & Immunology",
    "Hematology",
    "Urinalysis",
    "Molecular Biology",
    "Microbiology",
    "Serology",
];

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("cannot open workbook: {0}")]
    Open(String),
    #[error("workbook has no sheet named {0:?}")]
    MissingSheet(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Empty,
    Date(NaiveDate),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawTest {
    pub col: usize,
    pub header: String,
    pub tick_col: Option<usize>,
    pub section_hint: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StageCell {
    pub cell: Cell,
    pub ticked: bool,
    /// Coloured by the workbook's macro: due, not done (unless ticked).
    pub due: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawBlock {
    pub row: usize,
    pub name: String,
    pub title: String,
    pub joined: Option<NaiveDate>,
    /// `[Initial, Semi, Annual, NextDue][test_idx]`
    pub stages: [Vec<StageCell>; 4],
    pub remarks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawSheet {
    pub name: String,
    pub tests: Vec<RawTest>,
    pub blocks: Vec<RawBlock>,
}

fn text_cell(s: &str) -> Cell {
    let s = s.trim();
    if s.is_empty() {
        Cell::Empty
    } else if let Some(d) = parse_dmy(s) {
        Cell::Date(d)
    } else {
        Cell::Text(s.to_string())
    }
}

fn number_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

pub fn to_cell(v: &Data) -> Cell {
    match v {
        Data::Empty | Data::Error(_) => Cell::Empty,
        Data::String(s) => text_cell(s),
        Data::Int(i) => Cell::Text(i.to_string()),
        Data::Float(f) => Cell::Text(number_text(*f)),
        Data::Bool(b) => Cell::Text(b.to_string()),
        Data::DateTime(dt) => {
            let (y, m, d, ..) = dt.to_ymd_hms_milli();
            match NaiveDate::from_ymd_opt(i32::from(y), u32::from(m), u32::from(d)) {
                Some(date) => Cell::Date(date),
                None => Cell::Text(number_text(dt.as_f64())),
            }
        }
        Data::DateTimeIso(s) => {
            match s
                .get(..10)
                .and_then(|p| NaiveDate::parse_from_str(p, "%Y-%m-%d").ok())
            {
                Some(date) => Cell::Date(date),
                None => text_cell(s),
            }
        }
        Data::DurationIso(s) => text_cell(s),
    }
}

static EMPTY: Cell = Cell::Empty;

fn at(grid: &[Vec<Cell>], row: usize, col: usize) -> &Cell {
    grid.get(row).and_then(|r| r.get(col)).unwrap_or(&EMPTY)
}

/// Cell text with whitespace collapsed to single spaces; "" for empty cells.
fn collapsed(cell: &Cell) -> String {
    let raw = match cell {
        Cell::Empty => return String::new(),
        Cell::Text(s) => s.clone(),
        Cell::Date(d) => fmt_date(*d),
    };
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First cell in rows 0..3 (row-major) whose collapsed, lowercased text starts with `prefix`.
fn find_header(grid: &[Vec<Cell>], prefix: &str) -> Option<usize> {
    (0..3).find_map(|r| {
        let row = grid.get(r)?;
        (0..row.len()).find(|&c| collapsed(&row[c]).to_lowercase().starts_with(prefix))
    })
}

fn is_text_a(cell: &Cell) -> bool {
    matches!(cell, Cell::Text(s) if s == "a")
}

fn section_hint(grid: &[Vec<Cell>], col: usize) -> String {
    for c in (0..=col).rev() {
        let t = collapsed(at(grid, 0, c));
        if let Some(i) = t.to_lowercase().find("test system") {
            return t[..i].trim().to_string();
        }
    }
    String::new()
}

pub fn parse_grid(sheet: &str, grid: &[Vec<Cell>]) -> RawSheet {
    let mut out = RawSheet {
        name: sheet.to_string(),
        tests: Vec::new(),
        blocks: Vec::new(),
    };
    let width = grid.iter().map(Vec::len).max().unwrap_or(0);
    let (Some(comp_col), Some(name_col)) = (
        find_header(grid, "competency type"),
        find_header(grid, "name"),
    ) else {
        return out;
    };
    let pos_col = find_header(grid, "position");
    let joined_col = find_header(grid, "joined date");
    let remark_col = find_header(grid, "remark");
    let tests_end = remark_col.filter(|&r| r > comp_col).unwrap_or(width);

    for col in comp_col + 1..tests_end {
        let header = collapsed(at(grid, 2, col));
        if header.is_empty() || header.to_lowercase().starts_with("joined date") {
            continue;
        }
        let tc = col + 1;
        let tick_col = (tc < tests_end
            && matches!(at(grid, 2, tc), Cell::Empty)
            && (3..grid.len()).any(|r| is_text_a(at(grid, r, tc))))
        .then_some(tc);
        out.tests.push(RawTest {
            col,
            header,
            tick_col,
            section_hint: section_hint(grid, col),
        });
    }

    for row in 3..grid.len() {
        if !collapsed(at(grid, row, comp_col)).eq_ignore_ascii_case("initial") {
            continue;
        }
        let name = collapsed(at(grid, row, name_col));
        if name.eq_ignore_ascii_case("buffer cells") || name.eq_ignore_ascii_case("buffer row") {
            continue;
        }
        let title = pos_col
            .map(|c| collapsed(at(grid, row, c)))
            .unwrap_or_default();
        let joined = joined_col.and_then(|c| match at(grid, row, c) {
            Cell::Date(d) => Some(*d),
            _ => None,
        });
        let stages: [Vec<StageCell>; 4] = std::array::from_fn(|k| {
            out.tests
                .iter()
                .map(|t| StageCell {
                    cell: at(grid, row + k, t.col).clone(),
                    ticked: t
                        .tick_col
                        .is_some_and(|tc| is_text_a(at(grid, row + k, tc))),
                    due: false,
                })
                .collect()
        });
        let remarks = remark_col
            .map(|rc| {
                (0..4)
                    .map(|k| collapsed(at(grid, row + k, rc)))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        out.blocks.push(RawBlock {
            row,
            name,
            title,
            joined,
            stages,
            remarks,
        });
    }
    out
}

/// Marks the dates whose cell has the macro's colour and no tick.
fn mark_due(sheet: &mut RawSheet, cells: &HashSet<(usize, usize)>) {
    for b in &mut sheet.blocks {
        for (k, row) in b.stages.iter_mut().enumerate() {
            for (t, c) in sheet.tests.iter().zip(row) {
                c.due = !c.ticked
                    && matches!(c.cell, Cell::Date(_))
                    && cells.contains(&(b.row + k, t.col));
            }
        }
    }
}

pub fn read_workbook(path: &Path) -> Result<Vec<RawSheet>, ImportError> {
    let mut wb = open_workbook_auto(path).map_err(|e| ImportError::Open(e.to_string()))?;
    let names = wb.sheet_names();
    let mut sheets = Vec::with_capacity(SHEETS.len());
    for name in SHEETS {
        if !names.iter().any(|n| n == name) {
            return Err(ImportError::MissingSheet(name.to_string()));
        }
        let range = wb
            .worksheet_range(name)
            .map_err(|e| ImportError::Open(e.to_string()))?;
        // `Range` starts at the first used cell; pad so grid indices are absolute.
        let (r0, c0) = range
            .start()
            .map_or((0, 0), |(r, c)| (r as usize, c as usize));
        let mut grid: Vec<Vec<Cell>> = vec![Vec::new(); r0];
        for row in range.rows() {
            let mut cells = vec![Cell::Empty; c0];
            cells.extend(row.iter().map(to_cell));
            grid.push(cells);
        }
        sheets.push(parse_grid(name, &grid));
    }
    let fills = read_due_fills(path).map_err(ImportError::Open)?;
    for s in &mut sheets {
        if let Some(cells) = fills.get(&s.name) {
            mark_due(s, cells);
        }
    }
    Ok(sheets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{ExcelDateTime, ExcelDateTimeType};

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Build a grid from strings ("" = empty), converting each through `to_cell`.
    fn grid(rows: &[&[&str]]) -> Vec<Vec<Cell>> {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|s| to_cell(&Data::String((*s).to_string())))
                    .collect()
            })
            .collect()
    }

    fn text(s: &str) -> Cell {
        Cell::Text(s.to_string())
    }

    fn stage(cell: Cell, ticked: bool) -> StageCell {
        StageCell {
            cell,
            ticked,
            due: false,
        }
    }

    // Columns: 0 No, 1 Name, 2 Position, 3 Joined date, 4 Competency Type,
    // 5 Test A, 6 tick, 7 Test B, 8 tick, 9 Remark(s), 10 Updated
    fn basic_grid() -> Vec<Vec<Cell>> {
        grid(&[
            &[
                "No",
                "Name",
                "Position",
                "Joined date",
                "Competency\n Type",
                "Serology Test System",
                "",
                "",
                "",
                "Remark(s)",
                "Updated",
            ],
            &[],
            &["", "", "", "", "", "Test\n A", "", "Test B", "", "", ""],
            &[
                "1",
                "Alice Tan",
                "Senior MLT",
                "01/02/2019",
                "Initial",
                "19/03/2020",
                "a",
                "Not Done",
                "",
                "needs retrain",
                "",
            ],
            &[
                "",
                "",
                "",
                "",
                "Semi-annual",
                "20/09/2020",
                "",
                "",
                "",
                "",
                "",
            ],
            &[
                "",
                "",
                "",
                "",
                "Annual",
                "21/03/2021",
                "a",
                "",
                "",
                "second remark",
                "",
            ],
            &[
                "",
                "",
                "",
                "",
                "Next Annual Due",
                "21/03/2022",
                "",
                "",
                "",
                "",
                "",
            ],
        ])
    }

    #[test]
    fn to_cell_conversions() {
        assert_eq!(to_cell(&Data::Empty), Cell::Empty);
        assert_eq!(to_cell(&Data::String("   ".into())), Cell::Empty);
        assert_eq!(
            to_cell(&Data::String(" Not Done ".into())),
            text("Not Done")
        );
        assert_eq!(
            to_cell(&Data::String(" 16/9/2024 ".into())),
            Cell::Date(ymd(2024, 9, 16))
        );
        assert_eq!(to_cell(&Data::String("1/2/25".into())), text("1/2/25"));
        assert_eq!(to_cell(&Data::Float(12.0)), text("12"));
        assert_eq!(to_cell(&Data::Float(12.5)), text("12.5"));
        assert_eq!(to_cell(&Data::Int(7)), text("7"));
        // 43831 = 2020-01-01 in the 1900 system, so 43910 = 2020-03-20.
        let dt = ExcelDateTime::new(43910.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(to_cell(&Data::DateTime(dt)), Cell::Date(ymd(2020, 3, 20)));
    }

    #[test]
    fn parses_tests_ticks_and_blocks() {
        let sheet = parse_grid("Serology", &basic_grid());
        assert_eq!(sheet.name, "Serology");
        assert_eq!(sheet.tests.len(), 2);
        assert_eq!(sheet.tests[0].header, "Test A");
        assert_eq!(sheet.tests[0].col, 5);
        assert_eq!(sheet.tests[0].tick_col, Some(6));
        assert_eq!(sheet.tests[0].section_hint, "Serology");
        assert_eq!(sheet.tests[1].header, "Test B");
        assert_eq!(sheet.tests[1].tick_col, None, "no 'a' under Test B");

        assert_eq!(sheet.blocks.len(), 1);
        let b = &sheet.blocks[0];
        assert_eq!(b.row, 3);
        assert_eq!(b.name, "Alice Tan");
        assert_eq!(b.title, "Senior MLT");
        assert_eq!(b.joined, Some(ymd(2019, 2, 1)));
        assert_eq!(b.stages[0][0], stage(Cell::Date(ymd(2020, 3, 19)), true));
        assert_eq!(b.stages[0][1], stage(text("Not Done"), false));
        assert_eq!(b.stages[1][0], stage(Cell::Date(ymd(2020, 9, 20)), false));
        assert_eq!(b.stages[2][0], stage(Cell::Date(ymd(2021, 3, 21)), true));
        assert_eq!(b.stages[3][0], stage(Cell::Date(ymd(2022, 3, 21)), false));
        assert_eq!(
            b.remarks,
            vec!["needs retrain".to_string(), "second remark".to_string()]
        );
    }

    #[test]
    fn coloured_unticked_dates_are_due() {
        let mut sheet = parse_grid("Serology", &basic_grid());
        // Initial (ticked), Semi-annual and Next Annual Due under Test A, and
        // "Not Done" under Test B.
        let cells = HashSet::from([(3, 5), (4, 5), (6, 5), (3, 7)]);
        mark_due(&mut sheet, &cells);
        let due = |k: usize, t: usize| sheet.blocks[0].stages[k][t].due;
        assert!(due(1, 0) && due(3, 0));
        assert!(!due(0, 0) && !due(2, 0) && !due(0, 1));
    }

    #[test]
    fn test_without_tick_column() {
        // 3 adjacent TaqPath-style columns, no tick columns, then Remark(s).
        let g = grid(&[
            &[
                "Name",
                "Position",
                "Competency Type",
                "Molecular Biology Test System",
                "",
                "",
                "Remark(s)",
            ],
            &[],
            &[
                "",
                "",
                "",
                "SARS-Cov-2 detection by TaqPath COVID-19",
                "SARS-Cov-2 detection by TaqPath COVID-19",
                "SARS-Cov-2 detection by TaqPath COVID-19",
                "",
            ],
            &[
                "Bob", "MLT", "Initial", "1/1/2021", "2/1/2021", "3/1/2021", "",
            ],
            &["", "", "Semi-annual", "", "", "", ""],
            &["", "", "Annual", "", "", "", ""],
            &["", "", "Next Annual Due", "", "", "", ""],
        ]);
        let sheet = parse_grid("Molecular Biology", &g);
        assert_eq!(sheet.tests.len(), 3);
        for t in &sheet.tests {
            assert_eq!(t.tick_col, None);
            assert_eq!(t.header, "SARS-Cov-2 detection by TaqPath COVID-19");
            assert_eq!(t.section_hint, "Molecular Biology");
        }
        assert_eq!(
            sheet.blocks[0].stages[0][2],
            stage(Cell::Date(ymd(2021, 1, 3)), false)
        );
    }

    #[test]
    fn joined_date_in_header_row3() {
        // Molecular/Micro layout: "Joined date" sits in row 3 (index 2), between
        // Competency Type and the tests; it must not become a test.
        let g = grid(&[
            &[
                "No",
                "Name",
                "Position",
                "Competency Type",
                "Microbiology Test System",
                "",
                "Remark(s)",
            ],
            &[],
            &["", "", "", "", "Joined date", "Gram stain", "", ""],
            &[
                "1",
                "Carol",
                "Scientist",
                "Initial",
                "05/06/2018",
                "7/7/2019",
                "",
                "",
            ],
            &["", "", "", "Semi-annual", "", "", "", ""],
            &["", "", "", "Annual", "", "", "", ""],
            &["", "", "", "Next Annual Due", "", "", "", ""],
        ]);
        let sheet = parse_grid("Microbiology", &g);
        assert_eq!(sheet.tests.len(), 1);
        assert_eq!(sheet.tests[0].header, "Gram stain");
        assert_eq!(sheet.blocks[0].joined, Some(ymd(2018, 6, 5)));
        assert_eq!(
            sheet.blocks[0].stages[0][0].cell,
            Cell::Date(ymd(2019, 7, 7))
        );
    }

    #[test]
    fn section_hint_from_group_header() {
        let g = grid(&[
            &[
                "Name",
                "Position",
                "Joined date",
                "Competency Type",
                "Chemistry & Immunology Test System",
                "",
                "Hematology Test System",
                "",
                "",
                "Remark(s)",
            ],
            &[],
            &["", "", "", "", "Abbott Architect", "", "CBC", "", "ESR", ""],
            &[
                "Dan", "MLT", "1/1/2015", "Initial", "1/1/2020", "a", "2/2/2020", "a", "3/3/2020",
                "",
            ],
            &["", "", "", "Semi-annual", "", "", "", "", "", ""],
            &["", "", "", "Annual", "", "", "", "", "", ""],
            &["", "", "", "Next Annual Due", "", "", "", "", "", ""],
        ]);
        let sheet = parse_grid("Hematology", &g);
        let hints: Vec<_> = sheet
            .tests
            .iter()
            .map(|t| t.section_hint.as_str())
            .collect();
        assert_eq!(
            hints,
            vec!["Chemistry & Immunology", "Hematology", "Hematology"]
        );
        assert_eq!(sheet.tests[0].tick_col, Some(5));
        assert_eq!(sheet.tests[2].tick_col, None);
    }

    #[test]
    fn skips_buffer_rows() {
        let g = grid(&[
            &[
                "Name",
                "Position",
                "Competency Type",
                "Serology Test System",
                "",
                "Remark(s)",
            ],
            &[],
            &["", "", "", "T1", "", ""],
            &["Buffer Cells", "", "Initial", "", "", ""],
            &["", "", "Semi-annual", "", "", ""],
            &["", "", "Annual", "", "", ""],
            &["", "", "Next Annual Due", "", "", ""],
            &["Eve", "MLT", " initial ", "1/1/2022", "", ""],
            &["", "", "Semi-annual", "", "", ""],
            &["", "", "Annual", "", "", ""],
            &["", "", "Next Annual Due", "", "", ""],
            &["Buffer Row", "", "Initial", "", "", ""],
            &["", "", "Semi-annual", "", "", ""],
            &["", "", "Annual", "", "", ""],
            &["", "", "Next Annual Due", "", "", ""],
        ]);
        let sheet = parse_grid("Serology", &g);
        assert_eq!(sheet.blocks.len(), 1);
        assert_eq!(sheet.blocks[0].name, "Eve");
        assert_eq!(sheet.blocks[0].row, 7);
    }

    #[test]
    #[ignore]
    fn real_workbook_smoke() {
        let path = std::env::var("TRACKER_XLSM").expect("set TRACKER_XLSM to the workbook path");
        let sheets = read_workbook(Path::new(&path)).unwrap();
        for s in &sheets {
            println!(
                "{:<24} tests={:<3} blocks={}",
                s.name,
                s.tests.len(),
                s.blocks.len()
            );
        }
        assert_eq!(sheets.len(), 6);
        let sero = sheets.iter().find(|s| s.name == "Serology").unwrap();
        assert_eq!(sero.tests.len(), 14);
        let chem = sheets
            .iter()
            .find(|s| s.name == "Chemistry & Immunology")
            .unwrap();
        assert!(
            chem.blocks.len() >= 25,
            "chem blocks = {}",
            chem.blocks.len()
        );
    }
}
