//! Cell fill colours of an `.xlsx`/`.xlsm` workbook (calamine doesn't read
//! them). The tracker's macro colours every unticked date that is due: that
//! colour is how the workbook says "not done yet".

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// The macro's "past due" and "upcoming" fills, then the two its older
/// version used (still on cells it hasn't recoloured since).
const DUE_FILLS: [&str; 4] = ["FF99CC", "FDE2CC", "FF8A80", "FFFF8D"];

/// Sheet name → the (row, column) of each cell with one of the due fills,
/// both counted from 0.
pub type DueFills = HashMap<String, HashSet<(usize, usize)>>;

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| a.value.replace("&amp;", "&"))
}

/// Calls `f(name, element)` for every opening tag and `f(name, None)` for every
/// closing one; an empty element is both.
fn each(xml: &str, mut f: impl FnMut(&str, Option<&BytesStart>)) -> Result<(), String> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                f(&name, Some(&e));
            }
            Event::Empty(e) => {
                let name = e.local_name().as_ref().to_owned();
                f(&name, Some(&e));
                f(&name, None);
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_owned();
                f(&name, None);
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// "AB12" → (row 11, column 27).
fn cell_ref(s: &str) -> Option<(usize, usize)> {
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    let mut col = 0usize;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    let row: usize = digits.parse().ok()?;
    Some((row.checked_sub(1)?, col.checked_sub(1)?))
}

/// Which cell styles (indexes into `cellXfs`) have a due fill.
fn due_styles(styles: &str) -> Result<HashSet<usize>, String> {
    let mut fills: Vec<bool> = Vec::new();
    let mut due = HashSet::new();
    let (mut in_fills, mut in_xfs, mut solid) = (false, false, false);
    let mut xf = 0usize;
    each(styles, |name, start| match (name, start) {
        ("fills", s) => in_fills = s.is_some(),
        ("cellXfs", s) => in_xfs = s.is_some(),
        ("fill", Some(_)) if in_fills => fills.push(false),
        ("patternFill", Some(e)) if in_fills => {
            solid = attr(e, "patternType").as_deref() == Some("solid");
        }
        ("fgColor", Some(e)) if in_fills && solid => {
            let rgb = attr(e, "rgb").unwrap_or_default().to_uppercase();
            // "AARRGGBB"
            let hit = rgb.len() >= 6 && DUE_FILLS.contains(&&rgb[rgb.len() - 6..]);
            if let Some(f) = fills.last_mut() {
                *f = hit;
            }
        }
        ("xf", Some(e)) if in_xfs => {
            let id = attr(e, "fillId").and_then(|v| v.parse::<usize>().ok());
            if id.is_some_and(|i| fills.get(i) == Some(&true)) {
                due.insert(xf);
            }
            xf += 1;
        }
        _ => {}
    })?;
    Ok(due)
}

pub fn read_due_fills(path: &Path) -> Result<DueFills, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut text = |name: &str| -> Result<String, String> {
        let mut s = String::new();
        zip.by_name(name)
            .map_err(|e| format!("{name}: {e}"))?
            .read_to_string(&mut s)
            .map_err(|e| format!("{name}: {e}"))?;
        Ok(s)
    };
    let due = due_styles(&text("xl/styles.xml")?)?;
    let mut targets: HashMap<String, String> = HashMap::new();
    each(&text("xl/_rels/workbook.xml.rels")?, |name, start| {
        if let ("Relationship", Some(e)) = (name, start) {
            if let (Some(id), Some(t)) = (attr(e, "Id"), attr(e, "Target")) {
                let t = match t.strip_prefix('/') {
                    Some(abs) => abs.to_owned(),
                    None => format!("xl/{t}"),
                };
                targets.insert(id, t);
            }
        }
    })?;
    let mut sheets: Vec<(String, String)> = Vec::new();
    each(&text("xl/workbook.xml")?, |name, start| {
        if let ("sheet", Some(e)) = (name, start) {
            let target = attr(e, "id").and_then(|id| targets.get(&id).cloned());
            if let (Some(n), Some(t)) = (attr(e, "name"), target) {
                sheets.push((n, t));
            }
        }
    })?;
    let mut out = DueFills::new();
    for (name, target) in sheets {
        let mut cells = HashSet::new();
        each(&text(&target)?, |tag, start| {
            if let ("c", Some(e)) = (tag, start) {
                let style = attr(e, "s").and_then(|s| s.parse::<usize>().ok());
                if style.is_some_and(|s| due.contains(&s)) {
                    cells.extend(attr(e, "r").as_deref().and_then(cell_ref));
                }
            }
        })?;
        out.insert(name, cells);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{Color, Format, Workbook};

    #[test]
    fn cell_refs() {
        assert_eq!(cell_ref("A1"), Some((0, 0)));
        assert_eq!(cell_ref("AB12"), Some((11, 27)));
        assert_eq!(cell_ref("12"), None);
    }

    #[test]
    fn finds_the_macro_colours_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.xlsx");
        let mut wb = Workbook::new();
        let ws = wb
            .add_worksheet()
            .set_name("Chemistry & Immunology")
            .unwrap();
        for (row, rgb) in [(0, 0xFF99CC), (1, 0xFDE2CC), (2, 0x808080)] {
            let f = Format::new().set_background_color(Color::RGB(rgb));
            ws.write_string_with_format(row, 2, "x", &f).unwrap();
        }
        ws.write_string(3, 2, "plain").unwrap();
        wb.save(&path).unwrap();
        let fills = read_due_fills(&path).unwrap();
        assert_eq!(
            fills["Chemistry & Immunology"],
            HashSet::from([(0, 2), (1, 2)])
        );
    }
}
