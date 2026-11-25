//! The report as a workbook: one sheet per part, real dates and numbers,
//! filters on the long tables, and set up to print on A4 landscape.

use std::path::Path;

use chrono::{Datelike, NaiveDate, Timelike};
use rust_xlsxwriter::{
    Color, DocProperties, ExcelDateTime, Format, FormatAlign, FormatBorder, Url, Workbook,
    Worksheet, XlsxError,
};

use super::{tint_rgb, Block, Cell, Document, Sheet, Table, Tinted, STANDINGS, TITLE};

struct Styles {
    title: Format,
    intro: Format,
    subhead: Format,
    head: Format,
    body: Format,
    date: Format,
    stamp: Format,
    num: Format,
    percent: Format,
    sign: Format,
}

impl Styles {
    fn new() -> Styles {
        let body = Format::new()
            .set_border(FormatBorder::Thin)
            .set_border_color(Color::RGB(0xD9D6CC))
            .set_align(FormatAlign::Top)
            .set_text_wrap();
        let left = body.clone().set_align(FormatAlign::Left);
        Styles {
            title: Format::new().set_bold().set_font_size(16),
            intro: Format::new().set_font_color(Color::RGB(0x73726C)),
            subhead: Format::new().set_bold().set_font_size(12),
            head: body
                .clone()
                .set_bold()
                .set_background_color(Color::RGB(0xF0EEE6)),
            date: left.clone().set_num_format("d mmm yyyy"),
            stamp: left.set_num_format("d mmm yyyy hh:mm"),
            num: body.clone().set_num_format("0"),
            percent: body.clone().set_num_format("0%"),
            sign: Format::new().set_border_bottom(FormatBorder::Thin),
            body,
        }
    }
}

/// Excel refuses cells longer than 32,767 characters (in UTF-16).
fn clip(s: &str) -> String {
    s.chars().take(16_000).collect()
}

/// Dates Excel can't hold (before 1900) come back as `None`.
fn excel_date(d: NaiveDate) -> Option<ExcelDateTime> {
    let year = u16::try_from(d.year()).ok().filter(|y| *y >= 1900)?;
    ExcelDateTime::from_ymd(year, d.month() as u8, d.day() as u8).ok()
}

/// Writes one cell; `total` sets it like a heading.
fn put(
    ws: &mut Worksheet,
    st: &Styles,
    (r, c): (u32, u16),
    t: &Tinted,
    total: bool,
) -> Result<(), XlsxError> {
    let tint = |f: &Format| match t.tint {
        _ if total => f
            .clone()
            .set_bold()
            .set_background_color(Color::RGB(0xF0EEE6)),
        Some(s) => {
            let (bg, fg) = tint_rgb(s);
            f.clone()
                .set_background_color(Color::RGB(bg))
                .set_font_color(Color::RGB(fg))
        }
        None => f.clone(),
    };
    let dated = match &t.cell {
        Cell::Date(Some(d)) => excel_date(*d).map(|d| (d, &st.date)),
        Cell::Stamp(at) => {
            let l = at.naive_local();
            excel_date(l.date())
                .and_then(|d| d.and_hms(l.hour() as u16, l.minute() as u8, 0).ok())
                .map(|d| (d, &st.stamp))
        }
        _ => None,
    };
    if let Some((dt, f)) = dated {
        ws.write_datetime_with_format(r, c, &dt, &tint(f))?;
        return Ok(());
    }
    match &t.cell {
        Cell::Num(n) => ws.write_number_with_format(r, c, *n as f64, &tint(&st.num))?,
        Cell::Percent(Some(p)) => ws.write_number_with_format(r, c, *p, &tint(&st.percent))?,
        other => match other.words() {
            w if w.is_empty() => ws.write_blank(r, c, &tint(&st.body))?,
            w => ws.write_string_with_format(r, c, clip(&w), &tint(&st.body))?,
        },
    };
    Ok(())
}

/// Writes a table at `top`; returns the row after it.
fn put_table(ws: &mut Worksheet, st: &Styles, top: u32, t: &Table) -> Result<u32, XlsxError> {
    for (c, (h, _)) in t.heads.iter().enumerate() {
        ws.write_string_with_format(top, c as u16, clip(h), &st.head)?;
    }
    let mut r = top + 1;
    if t.rows.is_empty() {
        ws.write_string_with_format(r, 0, "None", &st.intro)?;
        return Ok(r + 1);
    }
    for (k, row) in t.rows.iter().enumerate() {
        let total = t.total && k + 1 == t.rows.len();
        for (c, cell) in row.iter().enumerate() {
            put(ws, st, (r, c as u16), cell, total)?;
        }
        let two = |t: &Tinted| matches!(&t.cell, Cell::Text(s) if s.contains('\n'));
        if t.split && row.iter().any(two) {
            ws.set_row_height(r, 31)?;
        }
        r += 1;
    }
    Ok(r)
}

fn put_sheet(wb: &mut Workbook, st: &Styles, doc: &Document, s: &Sheet) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(s.part.label())?;
    ws.set_landscape();
    ws.set_paper_size(9);
    ws.set_print_fit_to_pages(1, 0);
    ws.set_margins(0.4, 0.4, 0.6, 0.6, 0.3, 0.3);
    ws.set_screen_gridlines(false);
    // In footers "&" starts a code; a literal one is doubled.
    ws.set_footer(format!(
        "&L{}&C&A&RPage &P of &N",
        doc.stamp.replace('&', "&&")
    ));
    ws.write_string_with_format(0, 0, s.title, &st.title)?;
    ws.write_string_with_format(1, 0, &s.intro, &st.intro)?;
    let mut widths: Vec<f64> = Vec::new();
    for b in &s.blocks {
        if let Block::Table(t) = b {
            for (c, (_, w)) in t.heads.iter().enumerate() {
                if c < widths.len() {
                    widths[c] = widths[c].max(*w);
                } else {
                    widths.push(*w);
                }
            }
        }
    }
    for (c, w) in widths.iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }
    let mut r = 3;
    for b in &s.blocks {
        match b {
            Block::Heading(h) => {
                ws.write_string_with_format(r, 0, clip(h), &st.subhead)?;
                r += 1;
            }
            Block::Facts(facts) => {
                for (label, value) in facts {
                    ws.write_string_with_format(r, 0, *label, &st.intro)?;
                    ws.write_string(r, 1, clip(value))?;
                    r += 1;
                }
                r += 1;
            }
            Block::Table(t) => {
                let after = put_table(ws, st, r, t)?;
                if t.main {
                    ws.set_freeze_panes(r + 1, 0)?;
                    ws.set_repeat_rows(r, r)?;
                    if !t.rows.is_empty() {
                        ws.autofilter(r, 0, after - 1, t.heads.len() as u16 - 1)?;
                    }
                }
                r = after + 1;
            }
            Block::Key => {
                for (c, s) in STANDINGS.into_iter().enumerate() {
                    put(ws, st, (r, c as u16 + 1), &Cell::Standing(s).into(), false)?;
                }
                ws.set_freeze_panes(r + 1, 1)?;
                r += 2;
            }
            Block::Contents => {
                for other in doc.sheets.iter().filter(|o| o.part != s.part) {
                    let name = other.part.label();
                    let link = Url::new(format!("internal:'{name}'!A1")).set_text(name);
                    ws.write_url(r, 0, link)?;
                    ws.write_string_with_format(r, 1, other.part.about(), &st.intro)?;
                    r += 1;
                }
                r += 1;
            }
            Block::SignOff(roles) => {
                for role in roles {
                    ws.set_row_height(r, 26)?;
                    ws.write_string(r, 0, *role)?;
                    for c in 1..=2 {
                        ws.write_blank(r, c, &st.sign)?;
                    }
                    ws.write_string(r, 3, "Signature")?;
                    for c in 4..=6 {
                        ws.write_blank(r, c, &st.sign)?;
                    }
                    ws.write_string(r, 7, "Date")?;
                    for c in 8..=9 {
                        ws.write_blank(r, c, &st.sign)?;
                    }
                    r += 1;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn write(doc: &Document, path: &Path) -> Result<(), XlsxError> {
    let st = Styles::new();
    let mut wb = Workbook::new();
    wb.set_properties(
        &DocProperties::new()
            .set_title(TITLE)
            .set_author(clip(&doc.author)),
    );
    for s in &doc.sheets {
        put_sheet(&mut wb, &st, doc, s)?;
    }
    wb.save(path)
}
