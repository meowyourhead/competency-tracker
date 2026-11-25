//! The report as a PDF: A4 landscape, a part per section with bookmarks,
//! headings repeated on every page, and the app's own fonts embedded (so the
//! text can be searched and copied). Letters those fonts lack come from
//! Windows' fonts.
//!
//! Pages are laid out first, as drawing steps, so that page numbers are known
//! by the time they are drawn.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use krilla::color::rgb;
use krilla::destination::XyzDestination;
use krilla::geom::{PathBuilder, Point, Rect};
use krilla::metadata::Metadata;
use krilla::num::NormalizedF32;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::surface::Surface;
use krilla::text::{Font, TextDirection};

use super::{tint_rgb, Block, Cell, Document, Part, Sheet, Table, Tinted, STANDINGS, TITLE};

const PAGE_W: f32 = 841.89;
const PAGE_H: f32 = 595.28;
const MARGIN: f32 = 34.0;
const WIDTH: f32 = PAGE_W - 2.0 * MARGIN;
/// Where a page's content ends; the footer is below.
const BOTTOM: f32 = PAGE_H - 44.0;

const INK: u32 = 0x1F1E1D;
const MUTED: u32 = 0x73726C;
const RULE: u32 = 0xD9D6CC;
const HEAD: u32 = 0xF0EEE6;

const PAD: f32 = 3.0;
const LEADING: f32 = 1.3;
/// Points per character of column width, when there is room.
const PER_CHAR: f32 = 4.7;
/// A table wider than this (in characters) that may split does.
const SPLIT_AT: f64 = 190.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Regular,
    Medium,
    Serif,
}

struct Loaded {
    font: Font,
    /// How far each letter it has advances, in ems.
    advance: HashMap<char, f32>,
}

impl Loaded {
    fn new(bytes: Vec<u8>, letters: &BTreeSet<char>) -> Option<Loaded> {
        let face = ttf_parser::Face::parse(&bytes, 0).ok()?;
        let em = f32::from(face.units_per_em());
        let advance = letters
            .iter()
            .filter_map(|c| {
                let g = face.glyph_index(*c)?;
                Some((*c, f32::from(face.glyph_hor_advance(g)?) / em))
            })
            .collect();
        Some(Loaded {
            font: Font::new(bytes.into(), 0)?,
            advance,
        })
    }
}

struct Fonts {
    /// Regular, medium, serif; then Windows' own, for letters those lack.
    all: Vec<Loaded>,
}

/// Windows fonts tried in turn for letters ours lack (Chinese, Korean,
/// Indic scripts, symbols).
const FALLBACK: [&str; 8] = [
    "segoeui.ttf",
    "msyh.ttc",
    "simsun.ttc",
    "malgun.ttf",
    "Nirmala.ttc",
    "Nirmala.ttf",
    "seguisym.ttf",
    "arial.ttf",
];

impl Fonts {
    fn load(letters: &BTreeSet<char>) -> Result<Fonts, String> {
        let ours: [&[u8]; 3] = [
            include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
            include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
            include_bytes!("../../assets/fonts/SourceSerif4-Semibold.ttf"),
        ];
        let mut all = Vec::new();
        for bytes in ours {
            all.push(Loaded::new(bytes.to_vec(), letters).ok_or("The report's fonts didn't load")?);
        }
        let mut missing: BTreeSet<char> = letters
            .iter()
            .filter(|c| !all[0].advance.contains_key(c))
            .copied()
            .collect();
        let dir = std::env::var_os("WINDIR").map_or(PathBuf::from("C:\\Windows"), PathBuf::from);
        for name in FALLBACK {
            if missing.is_empty() {
                break;
            }
            let Ok(bytes) = std::fs::read(dir.join("Fonts").join(name)) else {
                continue;
            };
            let Some(f) = Loaded::new(bytes, &missing) else {
                continue;
            };
            if !f.advance.is_empty() {
                missing.retain(|c| !f.advance.contains_key(c));
                all.push(f);
            }
        }
        Ok(Fonts { all })
    }

    /// Which font has `c`, and its advance in ems. `None`: none has it.
    fn find(&self, face: Face, c: char) -> Option<(usize, f32)> {
        let first = match face {
            Face::Regular => 0,
            Face::Medium => 1,
            Face::Serif => 2,
        };
        std::iter::once(first)
            .chain((0..self.all.len()).filter(|i| *i != first))
            .find_map(|i| Some((i, *self.all[i].advance.get(&c)?)))
    }

    fn width(&self, face: Face, size: f32, s: &str) -> f32 {
        s.chars()
            .map(|c| self.find(face, c).map_or(0.5, |f| f.1))
            .sum::<f32>()
            * size
    }

    /// `s` in stretches of one font. Letters no font has become "?".
    fn runs(&self, face: Face, s: &str) -> Vec<(usize, String)> {
        let mut out: Vec<(usize, String)> = Vec::new();
        for c in s.chars() {
            let (i, c) = match self.find(face, c) {
                Some((i, _)) => (i, c),
                None => (self.find(face, '?').map_or(0, |f| f.0), '?'),
            };
            match out.last_mut() {
                Some((j, t)) if *j == i => t.push(c),
                _ => out.push((i, c.to_string())),
            }
        }
        out
    }

    /// `s` in lines at most `w` wide, broken at spaces where possible.
    fn wrap(&self, face: Face, size: f32, s: &str, w: f32) -> Vec<String> {
        let mut out = Vec::new();
        for para in clean(s).split('\n') {
            let mut line = String::new();
            let mut x = 0.0;
            for word in para.split_inclusive(' ') {
                let bare = self.width(face, size, word.trim_end());
                if x > 0.0 && x + bare > w {
                    out.push(std::mem::take(&mut line).trim_end().to_owned());
                    x = 0.0;
                }
                if bare <= w {
                    line.push_str(word);
                    x += self.width(face, size, word);
                    continue;
                }
                // A word wider than the column is broken anywhere.
                for c in word.chars() {
                    let cw = self.width(face, size, c.encode_utf8(&mut [0; 4]));
                    if x > 0.0 && x + cw > w && c != ' ' {
                        out.push(std::mem::take(&mut line));
                        x = 0.0;
                    }
                    line.push(c);
                    x += cw;
                }
            }
            out.push(line.trim_end().to_owned());
        }
        out
    }
}

/// Line breaks as `\n`, and no other control characters.
fn clean(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\r' => '\n',
            '\n' => '\n',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

enum Step {
    Box {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<u32>,
    },
    Line {
        x: f32,
        y: f32,
        w: f32,
    },
    /// `y` is the baseline.
    Text {
        x: f32,
        y: f32,
        face: Face,
        size: f32,
        color: u32,
        text: String,
    },
    /// The page the `sheet`th part starts on, ending at `right`.
    PageOf {
        sheet: usize,
        right: f32,
        y: f32,
    },
}

struct Layout<'a> {
    fonts: &'a Fonts,
    /// Each page's steps, and the part it belongs to.
    pages: Vec<(Vec<Step>, usize)>,
    /// The page each part starts on.
    starts: Vec<usize>,
    y: f32,
}

impl Layout<'_> {
    fn new_page(&mut self) {
        let sheet = self.starts.len() - 1;
        self.pages.push((Vec::new(), sheet));
        self.y = MARGIN;
    }

    fn step(&mut self, s: Step) {
        self.pages.last_mut().expect("a page is open").0.push(s);
    }

    /// Starts a new page unless `h` fits on this one.
    fn room(&mut self, h: f32) {
        if self.y + h > BOTTOM {
            self.new_page();
        }
    }

    /// One line, its top at the current place; returns nothing, moves nothing.
    fn line(&mut self, x: f32, top: f32, face: Face, size: f32, color: u32, text: &str) {
        self.step(Step::Text {
            x,
            y: top + size * 0.97,
            face,
            size,
            color,
            text: text.to_owned(),
        });
    }

    fn paragraph(&mut self, face: Face, size: f32, color: u32, text: &str) {
        for l in self.fonts.wrap(face, size, text, WIDTH) {
            self.room(size * LEADING);
            let top = self.y;
            self.line(MARGIN, top, face, size, color, &l);
            self.y += size * LEADING;
        }
    }

    fn sheet(&mut self, doc: &Document, s: &Sheet) {
        // The schedule is short: it follows on when there is room.
        if s.part == Part::Schedule && !self.pages.is_empty() && self.y + 170.0 < BOTTOM {
            self.starts.push(self.pages.len() - 1);
            self.y += 16.0;
        } else {
            self.starts.push(self.pages.len());
            self.new_page();
        }
        self.paragraph(Face::Serif, 20.0, INK, s.title);
        if !s.intro.is_empty() {
            self.y += 2.0;
            self.paragraph(Face::Regular, 9.0, MUTED, &s.intro);
        }
        self.y += 8.0;
        for b in &s.blocks {
            match b {
                Block::Heading(h) => {
                    self.y += 6.0;
                    // With the top of what it heads.
                    self.room(80.0);
                    self.paragraph(Face::Medium, 11.0, INK, h);
                    self.y += 3.0;
                }
                Block::Facts(facts) => {
                    for (label, value) in facts {
                        for (i, l) in self
                            .fonts
                            .wrap(Face::Regular, 9.0, value, WIDTH - 120.0)
                            .into_iter()
                            .enumerate()
                        {
                            self.room(13.0);
                            let top = self.y;
                            if i == 0 {
                                self.line(MARGIN, top, Face::Regular, 9.0, MUTED, label);
                            }
                            self.line(MARGIN + 120.0, top, Face::Regular, 9.0, INK, &l);
                            self.y += 13.0;
                        }
                    }
                    self.y += 4.0;
                }
                Block::Table(t) => self.table(t),
                Block::Key => self.key(),
                Block::Contents => {
                    for (i, other) in doc.sheets.iter().enumerate() {
                        if other.part == s.part {
                            continue;
                        }
                        self.room(14.0);
                        let top = self.y;
                        let (name, what) = (other.part.label(), other.part.about());
                        self.line(MARGIN, top, Face::Medium, 9.0, INK, name);
                        self.line(MARGIN + 120.0, top, Face::Regular, 9.0, MUTED, what);
                        self.step(Step::PageOf {
                            sheet: i,
                            right: MARGIN + 520.0,
                            y: top + 8.7,
                        });
                        self.y += 14.0;
                    }
                    self.y += 4.0;
                }
                Block::SignOff(roles) => {
                    for role in roles {
                        self.room(27.0);
                        let (top, base) = (self.y + 11.0, self.y + 22.0);
                        let mut x = MARGIN;
                        for (label, gap, w) in [
                            (*role, 64.0, 190.0),
                            ("Signature", 50.0, 190.0),
                            ("Date", 28.0, 110.0),
                        ] {
                            self.line(x, top, Face::Regular, 9.0, MUTED, label);
                            self.step(Step::Line {
                                x: x + gap,
                                y: base,
                                w,
                            });
                            x += gap + w + 16.0;
                        }
                        self.y += 27.0;
                    }
                }
            }
        }
    }

    /// What each tint means, along one line.
    fn key(&mut self) {
        let mut x = MARGIN;
        for s in STANDINGS {
            let w = self.fonts.width(Face::Regular, 8.0, s.label()) + 16.0;
            let (fill, color) = tint_rgb(s);
            let y = self.y;
            self.step(Step::Box {
                x,
                y,
                w,
                h: 15.0,
                fill: Some(fill),
            });
            self.line(x + 8.0, y + 3.2, Face::Regular, 8.0, color, s.label());
            x += w + 6.0;
        }
        self.y += 20.0;
    }

    fn table(&mut self, t: &Table) {
        let total: f64 = t.heads.iter().map(|(_, w)| w).sum();
        if !t.split || total <= SPLIT_AT {
            // A grouped table's first column is its bands.
            let first = usize::from(t.grouped);
            self.columns(t, &(first..t.heads.len()).collect::<Vec<_>>());
            return;
        }
        // The first column again beside each lot that fits.
        let mut lot = vec![0];
        let mut used = t.heads[0].1;
        for c in 1..t.heads.len() {
            if used + t.heads[c].1 > SPLIT_AT && lot.len() > 1 {
                self.columns(t, &lot);
                lot.truncate(1);
                used = t.heads[0].1;
            }
            lot.push(c);
            used += t.heads[c].1;
        }
        self.columns(t, &lot);
    }

    /// The table's columns `cols`, its headings repeated on every page.
    fn columns(&mut self, t: &Table, cols: &[usize]) {
        let total: f64 = cols.iter().map(|c| t.heads[*c].1).sum();
        let per = PER_CHAR.min(WIDTH / total as f32);
        let size = if per < 4.2 { 6.8 } else { 7.6 };
        let fonts = self.fonts;
        // No narrower than the longest word of its heading.
        let least: Vec<f32> = cols
            .iter()
            .map(|c| {
                t.heads[*c]
                    .0
                    .split_whitespace()
                    .map(|w| fonts.width(Face::Medium, size, w))
                    .fold(0.0, f32::max)
                    + 2.0 * PAD
                    + 0.5
            })
            .collect();
        let widths = fit(
            cols.iter().map(|c| t.heads[*c].1 as f32 * per).collect(),
            &least,
        );
        let wrap = |face, s: &str, w: f32| fonts.wrap(face, size, s, w - 2.0 * PAD);
        let head: Vec<Vec<String>> = cols
            .iter()
            .zip(&widths)
            .map(|(c, w)| wrap(Face::Medium, &t.heads[*c].0, *w))
            .collect();
        let step = size * LEADING;
        let lines = |cells: &[Vec<String>]| cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let tall = |n: usize| n as f32 * step + 2.0 * PAD;
        let head_h = tall(lines(&head));
        let band_h = if t.grouped { size + 10.0 } else { 0.0 };
        let draw_head = |l: &mut Self| {
            l.row(
                &head,
                &widths,
                &[],
                Face::Medium,
                size,
                (0, lines(&head)),
                Some(HEAD),
            );
        };
        let full: f32 = widths.iter().sum();
        // The name over a group's rows, again on every page they run on to.
        let draw_band = |l: &mut Self, name: &str, after: &str| {
            let y = l.y;
            l.step(Step::Box {
                x: MARGIN,
                y,
                w: full,
                h: band_h,
                fill: None,
            });
            let top = y + (band_h - size * LEADING) / 2.0;
            l.line(MARGIN + PAD, top, Face::Medium, size + 0.8, INK, name);
            let x = MARGIN + PAD + l.fonts.width(Face::Medium, size + 0.8, name) + 5.0;
            l.line(x, top, Face::Regular, size + 0.8, MUTED, after);
            l.y += band_h;
        };
        let mut groups: HashMap<String, usize> = HashMap::new();
        if t.grouped {
            for row in &t.rows {
                let name = row.first().map(|c| c.cell.words()).unwrap_or_default();
                *groups.entry(name).or_default() += 1;
            }
        }
        let mut group: Option<String> = None;
        // The headings never end a page alone.
        self.room(head_h + band_h + tall(2));
        draw_head(self);
        if t.rows.is_empty() {
            let top = self.y + PAD;
            self.line(MARGIN + PAD, top, Face::Regular, size, MUTED, "None");
            self.y += tall(1);
        }
        for (k, row) in t.rows.iter().enumerate() {
            if t.grouped {
                let name = row.first().map(|c| c.cell.words()).unwrap_or_default();
                if group.as_ref() != Some(&name) {
                    if self.y + band_h + tall(2) > BOTTOM {
                        self.new_page();
                        draw_head(self);
                    }
                    let count = groups.get(&name).copied().unwrap_or(0).to_string();
                    draw_band(self, &name, &count);
                    group = Some(name);
                }
            }
            let total = t.total && k + 1 == t.rows.len();
            let face = if total { Face::Medium } else { Face::Regular };
            let cells: Vec<Vec<String>> = cols
                .iter()
                .zip(&widths)
                .map(|(c, w)| {
                    let words = row.get(*c).map(|t| t.cell.words()).unwrap_or_default();
                    wrap(face, &words, *w)
                })
                .collect();
            let tints: Vec<Option<&Tinted>> = cols.iter().map(|c| row.get(*c)).collect();
            let n = lines(&cells);
            let page = BOTTOM - MARGIN - head_h - band_h;
            let mut from = 0;
            while from < n {
                let left = n - from;
                if self.y + tall(left) > BOTTOM && (tall(left) <= page || self.y + tall(1) > BOTTOM)
                {
                    self.new_page();
                    draw_head(self);
                    if let Some(name) = &group {
                        draw_band(self, name, "continued");
                    }
                }
                // A row taller than a page runs on to the next.
                let fit = ((BOTTOM - self.y - 2.0 * PAD) / step).floor().max(1.0) as usize;
                let to = n.min(from + fit);
                self.row(
                    &cells,
                    &widths,
                    &tints,
                    face,
                    size,
                    (from, to),
                    total.then_some(HEAD),
                );
                from = to;
            }
        }
        self.y += 10.0;
    }

    /// Lines `from..to` of a row's cells, boxed.
    #[allow(clippy::too_many_arguments)]
    fn row(
        &mut self,
        cells: &[Vec<String>],
        widths: &[f32],
        tints: &[Option<&Tinted>],
        face: Face,
        size: f32,
        (from, to): (usize, usize),
        fill: Option<u32>,
    ) {
        let h = (to - from) as f32 * size * LEADING + 2.0 * PAD;
        let y = self.y;
        let mut x = MARGIN;
        for (i, (cell, w)) in cells.iter().zip(widths).enumerate() {
            let tint = tints.get(i).copied().flatten();
            let (bg, fg) = match tint.and_then(|t| t.tint).map(tint_rgb) {
                Some((bg, fg)) => (Some(bg), fg),
                None => (fill, INK),
            };
            self.step(Step::Box {
                x,
                y,
                w: *w,
                h,
                fill: bg,
            });
            let right = matches!(tint.map(|t| &t.cell), Some(Cell::Num(_) | Cell::Percent(_)));
            for (k, l) in cell.iter().skip(from).take(to - from).enumerate() {
                if l.is_empty() {
                    continue;
                }
                let at = if right {
                    x + w - PAD - self.fonts.width(face, size, l)
                } else {
                    x + PAD
                };
                let top = y + PAD + k as f32 * size * LEADING;
                self.line(at, top, face, size, fg, l);
            }
            x += w;
        }
        self.y += h;
    }
}

/// Widens columns to `least`, taking the room from those with some to spare.
fn fit(widths: Vec<f32>, least: &[f32]) -> Vec<f32> {
    let spare: f32 = widths
        .iter()
        .zip(least)
        .map(|(w, l)| (w - l).max(0.0))
        .sum();
    let short: f32 = widths
        .iter()
        .zip(least)
        .map(|(w, l)| (l - w).max(0.0))
        .sum();
    if spare < short {
        return widths;
    }
    widths
        .iter()
        .zip(least)
        .map(|(w, l)| {
            if w < l {
                *l
            } else if spare > 0.0 {
                w - (w - l) * short / spare
            } else {
                *w
            }
        })
        .collect()
}

fn paint(rgb_: u32) -> rgb::Color {
    rgb::Color::new((rgb_ >> 16) as u8, (rgb_ >> 8) as u8, rgb_ as u8)
}

fn fill(color: u32) -> Fill {
    Fill {
        paint: paint(color).into(),
        opacity: NormalizedF32::ONE,
        rule: Default::default(),
    }
}

fn hairline() -> Stroke {
    Stroke {
        paint: paint(RULE).into(),
        width: 0.5,
        ..Default::default()
    }
}

/// One line of text, in as many fonts as it needs. `y` is the baseline.
fn draw_text(
    s: &mut Surface,
    fonts: &Fonts,
    (x, y): (f32, f32),
    face: Face,
    size: f32,
    color: u32,
    text: &str,
) {
    s.set_stroke(None);
    s.set_fill(Some(fill(color)));
    let mut x = x;
    for (i, run) in fonts.runs(face, text) {
        s.draw_text(
            Point::from_xy(x, y),
            fonts.all[i].font.clone(),
            size,
            &run,
            false,
            TextDirection::Auto,
        );
        x += fonts.width(face, size, &run);
    }
}

fn draw(s: &mut Surface, fonts: &Fonts, step: &Step, starts: &[usize]) {
    match step {
        Step::Box {
            x,
            y,
            w,
            h,
            fill: bg,
        } => {
            let Some(rect) = Rect::from_xywh(*x, *y, *w, *h) else {
                return;
            };
            let mut p = PathBuilder::new();
            p.push_rect(rect);
            if let Some(p) = p.finish() {
                s.set_fill(bg.map(fill));
                s.set_stroke(Some(hairline()));
                s.draw_path(&p);
            }
        }
        Step::Line { x, y, w } => {
            let mut p = PathBuilder::new();
            p.move_to(*x, *y);
            p.line_to(x + w, *y);
            if let Some(p) = p.finish() {
                s.set_fill(None);
                s.set_stroke(Some(hairline()));
                s.draw_path(&p);
            }
        }
        Step::Text {
            x,
            y,
            face,
            size,
            color,
            text,
        } => draw_text(s, fonts, (*x, *y), *face, *size, *color, text),
        Step::PageOf { sheet, right, y } => {
            let n = (starts[*sheet] + 1).to_string();
            let x = right - fonts.width(Face::Regular, 9.0, &n);
            draw_text(s, fonts, (x, *y), Face::Regular, 9.0, INK, &n);
        }
    }
}

/// Every letter the document uses.
fn letters(doc: &Document) -> BTreeSet<char> {
    let mut out: BTreeSet<char> = (' '..='~').collect();
    let mut add = |s: &str| out.extend(clean(s).chars().filter(|c| *c != '\n'));
    add(&doc.stamp);
    for p in Part::ALL {
        add(p.label());
        add(p.about());
    }
    for s in &doc.sheets {
        add(s.title);
        add(&s.intro);
        for b in &s.blocks {
            match b {
                Block::Heading(h) => add(h),
                Block::Facts(f) => f.iter().for_each(|(_, v)| add(v)),
                Block::Table(t) => {
                    t.heads.iter().for_each(|(h, _)| add(h));
                    t.rows.iter().flatten().for_each(|c| add(&c.cell.words()));
                }
                Block::Key | Block::Contents | Block::SignOff(_) => {}
            }
        }
    }
    out
}

/// The finished file's bytes and how many pages it has.
fn render(doc: &Document) -> Result<(Vec<u8>, usize), String> {
    let fonts = Fonts::load(&letters(doc))?;
    let mut l = Layout {
        fonts: &fonts,
        pages: Vec::new(),
        starts: Vec::new(),
        y: MARGIN,
    };
    for s in &doc.sheets {
        l.sheet(doc, s);
    }
    let Layout { pages, starts, .. } = l;

    let mut pdf = krilla::Document::new();
    let count = pages.len();
    let foot = |s: &mut Surface, x: f32, text: &str| {
        let at = (x, PAGE_H - 24.0);
        draw_text(s, &fonts, at, Face::Regular, 7.5, MUTED, text);
    };
    for (n, (steps, sheet)) in pages.iter().enumerate() {
        let settings = PageSettings::from_wh(PAGE_W, PAGE_H).ok_or("Bad page size")?;
        let mut page = pdf.start_page_with(settings);
        let mut s = page.surface();
        for step in steps {
            draw(&mut s, &fonts, step, &starts);
        }
        foot(&mut s, MARGIN, &doc.stamp);
        let name = doc.sheets[*sheet].part.label();
        let mid = (PAGE_W - fonts.width(Face::Regular, 7.5, name)) / 2.0;
        foot(&mut s, mid, name);
        let number = format!("Page {} of {count}", n + 1);
        let right = PAGE_W - MARGIN - fonts.width(Face::Regular, 7.5, &number);
        foot(&mut s, right, &number);
        s.finish();
        page.finish();
    }
    let mut outline = Outline::new();
    for (sheet, start) in doc.sheets.iter().zip(&starts) {
        let to = XyzDestination::new(*start, Point::from_xy(0.0, 0.0));
        outline.push_child(OutlineNode::new(sheet.part.label().to_owned(), to));
    }
    pdf.set_outline(outline);
    pdf.set_metadata(
        Metadata::new()
            .title(TITLE.to_owned())
            .authors(vec![doc.author.clone()])
            .language("en".to_owned()),
    );
    let bytes = pdf
        .finish()
        .map_err(|e| format!("Couldn't build the PDF: {e:?}"))?;
    Ok((bytes, count))
}

pub(super) fn write(doc: &Document, path: &Path) -> Result<(), String> {
    let (bytes, _) = render(doc)?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::{text, Standing};
    use super::*;

    fn table(rows: Vec<Vec<Cell>>) -> Table {
        Table::new(&[("Name", 30.0), ("Detail", 60.0)], rows)
    }

    /// A summary with contents, then `t` alone.
    fn doc(t: Table) -> Document {
        let first = vec![Block::Contents, Block::Key];
        Document {
            stamp: "Stamp".into(),
            author: "R&D".into(),
            sheets: vec![
                Sheet::new(Part::Summary, "Intro", first),
                Sheet::new(Part::Checks, "", vec![Block::Table(t)]),
            ],
        }
    }

    #[test]
    fn pages_follow_the_rows() {
        let (bytes, few) = render(&doc(table(vec![]))).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(few, 2);
        let rows = (0..200)
            .map(|i| vec![text(format!("P{i}")), Cell::Standing(Standing::Overdue)])
            .collect();
        let (_, many) = render(&doc(table(rows))).unwrap();
        assert!(many > 4, "{many}");
    }

    #[test]
    fn grouped_rows_and_totals_lay_out() {
        let rows = (0..200)
            .map(|i| vec![text(format!("Group {}", i / 90)), text(format!("Row {i}"))])
            .collect();
        let mut t = table(rows);
        t.grouped = true;
        t.total = true;
        let (_, n) = render(&doc(t)).unwrap();
        assert!(n > 4, "{n}");
    }

    #[test]
    fn a_row_taller_than_a_page_runs_on() {
        let long = "word ".repeat(6_000) + &"x".repeat(5_000);
        for grouped in [false, true] {
            let mut t = table(vec![vec![text("A"), text(&long)]]);
            t.grouped = grouped;
            let (_, n) = render(&doc(t)).unwrap();
            assert!(n > 4, "{n}");
        }
    }
    #[test]
    fn other_scripts_and_control_characters_are_fine() {
        let odd = "\u{9648}\u{4e3d} \u{0b85}\u{0bae} \u{1f600} a\tb\r\nc\u{7} \u{e000}";
        render(&doc(table(vec![vec![text(odd), text("")]]))).unwrap();
    }

    #[test]
    fn wrapping_keeps_every_word_within_the_width() {
        let words: BTreeSet<char> = (' '..='~').collect();
        let f = Fonts::load(&words).unwrap();
        let s = "The quick brown fox jumps over Supercalifragilisticexpialidocious";
        let lines = f.wrap(Face::Regular, 8.0, s, 60.0);
        for l in &lines {
            assert!(f.width(Face::Regular, 8.0, l) <= 60.0, "{l}");
        }
        assert_eq!(lines.concat().replace(' ', ""), s.replace(' ', ""));
    }

    #[test]
    fn columns_fit_their_headings_without_growing_the_table() {
        let w = fit(vec![20.0, 60.0, 100.0], &[30.0, 50.0, 40.0]);
        assert_eq!(w[0], 30.0);
        assert!((w.iter().sum::<f32>() - 180.0).abs() < 0.01);
        assert!(w[1] >= 50.0 && w[2] >= 40.0);
        // Nowhere to take it from: left alone.
        assert_eq!(fit(vec![10.0, 10.0], &[30.0, 10.0]), [10.0, 10.0]);
    }

    #[test]
    fn wide_tables_continue_below() {
        let mut t = Table::new(&[("Staff", 26.0)], vec![]);
        for i in 0..20 {
            t.heads.push((format!("T{i}"), 18.0));
        }
        t.rows = vec![(0..21).map(|_| text("x").into()).collect()];
        t.split = true;
        render(&doc(t)).unwrap();
    }
}
