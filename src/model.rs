use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub type Id = String;
pub type Ts = chrono::DateTime<chrono::FixedOffset>;

pub const SCHEMA_VERSION: u32 = 1;

/// `"{prefix}-{nanoid(10)}"`.
pub fn new_id(prefix: &str) -> Id {
    format!("{prefix}-{}", nanoid::nanoid!(10))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub due_soon_days: i64,
    pub variance_tolerance_days: i64,
    /// Idle time before the "Still editing?" prompt opens.
    pub inactivity_minutes: i64,
    /// How long the prompt waits for "Continue editing" before editing stops.
    pub inactivity_grace_minutes: i64,
    pub initial_due_days: Option<i64>,
    pub archive_after_days: Option<i64>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            due_soon_days: 30,
            variance_tolerance_days: 30,
            inactivity_minutes: 15,
            inactivity_grace_minutes: 15,
            initial_due_days: None,
            archive_after_days: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub label: String,
    pub interval_months: Option<u32>,
    pub repeats: bool,
    pub ordinal: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: Id,
    pub name: String,
    pub stages: Vec<Stage>,
}

impl Rule {
    pub fn default_global() -> Rule {
        Rule {
            id: "r-global".into(),
            name: "Global".into(),
            stages: vec![
                Stage {
                    label: "Initial".into(),
                    interval_months: None,
                    repeats: false,
                    ordinal: false,
                },
                Stage {
                    label: "Semi-annual".into(),
                    interval_months: Some(6),
                    repeats: false,
                    ordinal: false,
                },
                Stage {
                    label: "Annual".into(),
                    interval_months: Some(12),
                    repeats: true,
                    ordinal: true,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestSystem {
    pub id: Id,
    pub name: String,
    pub order: u32,
    pub retired: bool,
    pub rule_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: Id,
    pub name: String,
    pub order: u32,
    pub retired: bool,
    pub tests: Vec<TestSystem>,
    /// `"#RRGGBB"`; `None` uses the default colour for the section's position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Staff ids in the order the workbook lists them: the section's default
    /// order. Anyone not listed comes after, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub staff_order: Vec<Id>,
}

/// The colours sections get by position until one is picked in Settings.
/// Mid-tone, so they read as tints on light and dark backgrounds alike.
pub const SECTION_COLORS: [u32; 8] = [
    0x3E8E7E, // teal
    0x5B6CC9, // indigo
    0xC98B2B, // ochre
    0xB8527A, // rose
    0x6E8F3A, // moss
    0x3D7EA6, // lake
    0x8A5CB0, // plum
    0xB0623A, // rust
];

/// Parses `"#RRGGBB"` (or `"RRGGBB"`).
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok())?
}

pub fn fmt_hex_color(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0xFF_FFFF)
}

impl Section {
    /// The section's colour as `0xRRGGBB`: its own, else the default for
    /// `position` (its place among the sorted sections).
    pub fn color_rgb(&self, position: usize) -> u32 {
        self.color
            .as_deref()
            .and_then(parse_hex_color)
            .unwrap_or(SECTION_COLORS[position % SECTION_COLORS.len()])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StaffStatus {
    Active,
    Hidden,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssignState {
    Assigned,
    Na,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub state: AssignState,
    pub since: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Staff {
    pub id: Id,
    pub name: String,
    pub title: String,
    pub joined: Option<NaiveDate>,
    pub status: StaffStatus,
    /// Keyed by test id.
    pub assignments: BTreeMap<Id, Assignment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: Id,
    pub staff_id: Id,
    pub test_id: Id,
    pub stage_index: u32,
    pub stage_label: String,
    pub completed_on: NaiveDate,
    pub comment: String,
    pub by: String,
    pub at: Ts,
    pub imported: bool,
    pub unconfirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub staff_id: Id,
    pub test_id: Id,
    pub stage_index: u32,
    pub by: String,
    pub at: Ts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverrideSource {
    Excel,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DueOverride {
    pub staff_id: Id,
    pub test_id: Id,
    pub due_on: NaiveDate,
    pub source: OverrideSource,
    pub by: String,
    pub at: Ts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelayNote {
    pub id: Id,
    pub staff_id: Id,
    pub test_id: Id,
    pub for_stage_index: u32,
    pub note: String,
    pub expected_on: Option<NaiveDate>,
    pub by: String,
    pub at: Ts,
}

/// Imported free text (?12); shown in history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: Id,
    pub staff_id: Id,
    pub test_id: Option<Id>,
    pub section_id: Option<Id>,
    pub text: String,
    pub by: String,
    pub at: Ts,
}

/// `refs` keys used: `staff_id`, `test_id`, `section_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: Ts,
    pub by: String,
    pub action: String,
    pub detail: String,
    pub refs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub schema_version: u32,
    pub saved_at: Ts,
    pub saved_by: String,
    pub settings: Settings,
    #[serde(default)]
    pub rules: Vec<Rule>,
    pub default_rule_id: Id,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub staff: Vec<Staff>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
    #[serde(default)]
    pub due_overrides: Vec<DueOverride>,
    #[serde(default)]
    pub delay_notes: Vec<DelayNote>,
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub audit: Vec<AuditEntry>,
}

impl Data {
    pub fn new_empty(user: &str, now: Ts) -> Data {
        let rule = Rule::default_global();
        let sections = [
            "Chemistry",
            "Haematology",
            "Urinalysis",
            "Molecular",
            "Microbiology",
            "Serology",
        ]
        .iter()
        .enumerate()
        .map(|(i, name)| Section {
            id: new_id("s"),
            name: (*name).into(),
            order: i as u32,
            retired: false,
            tests: vec![],
            color: None,
            staff_order: vec![],
        })
        .collect();
        Data {
            schema_version: SCHEMA_VERSION,
            saved_at: now,
            saved_by: user.into(),
            settings: Settings::default(),
            default_rule_id: rule.id.clone(),
            rules: vec![rule],
            sections,
            staff: vec![],
            records: vec![],
            skipped: vec![],
            due_overrides: vec![],
            delay_notes: vec![],
            notes: vec![],
            audit: vec![],
        }
    }

    /// Normalises hand-edited or older files so the default rule always resolves.
    pub fn ensure_valid(&mut self) {
        if !self.rules.iter().any(|r| r.id == self.default_rule_id) {
            if !self.rules.iter().any(|r| r.id == "r-global") {
                self.rules.push(Rule::default_global());
            }
            self.default_rule_id = "r-global".into();
        }
    }

    pub fn staff(&self, id: &str) -> Option<&Staff> {
        self.staff.iter().find(|s| s.id == id)
    }

    pub fn staff_mut(&mut self, id: &str) -> Option<&mut Staff> {
        self.staff.iter_mut().find(|s| s.id == id)
    }

    pub fn test(&self, test_id: &str) -> Option<(&Section, &TestSystem)> {
        self.sections
            .iter()
            .find_map(|s| s.tests.iter().find(|t| t.id == test_id).map(|t| (s, t)))
    }

    pub fn section(&self, id: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// The test's own rule, falling back to the default rule.
    pub fn rule_for_test(&self, test_id: &str) -> &Rule {
        let wanted = self
            .test(test_id)
            .and_then(|(_, t)| t.rule_id.as_deref())
            .unwrap_or(&self.default_rule_id);
        self.rules
            .iter()
            .find(|r| r.id == wanted)
            .or_else(|| self.rules.iter().find(|r| r.id == self.default_rule_id))
            .expect("default rule must exist")
    }

    pub fn sections_sorted(&self) -> Vec<&Section> {
        let mut v: Vec<&Section> = self.sections.iter().collect();
        v.sort_by_key(|s| s.order);
        v
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_ts() -> Ts {
        chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 14, 9, 30, 0)
            .unwrap()
    }

    #[test]
    fn data_roundtrips_json() {
        let d = Data::new_empty("jtan", sample_ts());
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Data>(&s).unwrap(), d);
    }

    #[test]
    fn new_empty_has_global_rule_and_six_sections() {
        let d = Data::new_empty("jtan", sample_ts());
        assert_eq!(d.default_rule_id, "r-global");
        assert_eq!(
            d.sections_sorted()
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Chemistry",
                "Haematology",
                "Urinalysis",
                "Molecular",
                "Microbiology",
                "Serology"
            ]
        );
        assert_eq!(d.rules[0].stages[1].interval_months, Some(6));
        assert!(d.rules[0].stages[2].repeats && d.rules[0].stages[2].ordinal);
    }

    #[test]
    fn missing_vec_fields_default() {
        let d = Data::new_empty("jtan", sample_ts());
        let mut v = serde_json::to_value(&d).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("notes");
        o.remove("skipped");
        let back: Data = serde_json::from_value(v).unwrap();
        assert!(back.notes.is_empty() && back.skipped.is_empty());
    }

    #[test]
    fn lookups_and_rule_fallback() {
        let mut d = Data::new_empty("jtan", sample_ts());
        d.rules.push(Rule {
            id: "r-x".into(),
            name: "X".into(),
            stages: vec![],
        });
        d.sections[0].tests.push(TestSystem {
            id: "t-1".into(),
            name: "A".into(),
            order: 0,
            retired: false,
            rule_id: None,
        });
        d.sections[0].tests.push(TestSystem {
            id: "t-2".into(),
            name: "B".into(),
            order: 1,
            retired: false,
            rule_id: Some("r-x".into()),
        });
        let (sec, t) = d.test("t-1").unwrap();
        assert_eq!((sec.name.as_str(), t.name.as_str()), ("Chemistry", "A"));
        assert_eq!(d.rule_for_test("t-1").id, "r-global");
        assert_eq!(d.rule_for_test("t-2").id, "r-x");
        assert_eq!(d.rule_for_test("missing").id, "r-global");
        assert!(d.test("nope").is_none());
        let sid = d.sections[2].id.clone();
        assert_eq!(d.section(&sid).unwrap().name, "Urinalysis");
    }

    #[test]
    fn new_id_has_prefix_and_is_unique() {
        let a = new_id("s");
        assert!(a.starts_with("s-") && a.len() == 12);
        assert_ne!(a, new_id("s"));
    }

    #[test]
    fn settings_defaults() {
        let s = Settings::default();
        assert_eq!(
            (
                s.due_soon_days,
                s.variance_tolerance_days,
                s.inactivity_minutes
            ),
            (30, 30, 15)
        );
        assert_eq!(s.inactivity_grace_minutes, 15);
        assert_eq!((s.initial_due_days, s.archive_after_days), (None, None));
    }

    #[test]
    fn settings_missing_fields_use_defaults() {
        let s: Settings = serde_json::from_str(r#"{"due_soon_days": 45}"#).unwrap();
        assert_eq!(s.due_soon_days, 45);
        assert_eq!(s.variance_tolerance_days, 30);
        assert_eq!(s.inactivity_minutes, 15);
        assert_eq!(s.inactivity_grace_minutes, 15);
    }

    #[test]
    fn section_colors_parse_and_fall_back() {
        assert_eq!(parse_hex_color("#3E8E7E"), Some(0x3E8E7E));
        assert_eq!(parse_hex_color("3e8e7e"), Some(0x3E8E7E));
        assert_eq!(parse_hex_color("#xyz"), None);
        assert_eq!(fmt_hex_color(0x0A0B0C), "#0A0B0C");
        let mut d = Data::new_empty("jtan", sample_ts());
        assert_eq!(d.sections[1].color_rgb(1), SECTION_COLORS[1]);
        assert_eq!(d.sections[0].color_rgb(9), SECTION_COLORS[1]);
        d.sections[0].color = Some("#112233".into());
        assert_eq!(d.sections[0].color_rgb(0), 0x112233);
    }

    #[test]
    fn ensure_valid_restores_default_rule() {
        let mut d = Data::new_empty("jtan", sample_ts());
        d.rules.clear();
        d.default_rule_id = "r-gone".into();
        d.ensure_valid();
        assert_eq!(d.default_rule_id, "r-global");
        assert_eq!(d.rules.len(), 1);
        assert_eq!(d.rule_for_test("any").id, "r-global");

        let mut d = Data::new_empty("jtan", sample_ts());
        d.rules.push(Rule {
            id: "r-x".into(),
            name: "X".into(),
            stages: vec![],
        });
        d.default_rule_id = "r-x".into();
        d.ensure_valid();
        assert_eq!(d.default_rule_id, "r-x");
        assert_eq!(d.rules.len(), 2);

        let mut d = Data::new_empty("jtan", sample_ts());
        d.default_rule_id = "r-gone".into();
        d.ensure_valid();
        assert_eq!((d.default_rule_id.as_str(), d.rules.len()), ("r-global", 1));
    }

    #[test]
    fn enums_serialize_lowercase() {
        assert_eq!(
            serde_json::to_string(&StaffStatus::Hidden).unwrap(),
            "\"hidden\""
        );
        assert_eq!(serde_json::to_string(&AssignState::Na).unwrap(), "\"na\"");
    }
}
