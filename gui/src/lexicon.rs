//! The Lexicon: ache's in-app guide book, presented by Lana.
//!
//! Content is DATA, not UI strings: `data/lexicon.tsv` carries every
//! entry with a section, a key, a title, a source class, and a body.
//! The test suite validates coverage mechanically — every device
//! family has a family entry and a lesson entry, every kink in the
//! estate canon has an entry, the sensitivity chapter exists, and
//! every entry carries a known source class — so the book's coverage
//! is a gate result, not a claim.
//!
//! Sourcing discipline (the commission's rule): manufacturer
//! documentation and official guides ground device facts and all
//! safety content; the buttplug platform documentation grounds the
//! actuator model; reputable educator material grounds general
//! technique framing; the estate corpus (Kama programme tagging and
//! LANA-TOYS, MoonDeep track canon) grounds the kink mappings —
//! D-1144 quarantine posture: estate-corpus technique is adapted and
//! provenance-labelled, never presented as licensed. Anything with
//! no source is written as plainly-labelled general technique or
//! omitted. No invented specifications, no fabricated citations.

use std::fmt;

/// The embedded lexicon table.
pub const LEXICON_TSV: &str = include_str!("../data/lexicon.tsv");

/// Fields per lexicon row: id, section, key, title, source, body.
pub const FIELD_COUNT: usize = 6;

/// The source class of an entry — shown in the UI on every entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceClass {
    /// Manufacturer documentation / official product guides.
    Manufacturer,
    /// Reputable sex-education sources.
    Educator,
    /// The estate's own studied corpora (provenance labelled).
    Estate,
    /// Authored from the device's config data (buttplug platform
    /// documentation + the pinned device config).
    DeviceData,
}

impl SourceClass {
    fn parse(text: &str) -> Option<SourceClass> {
        match text {
            "manufacturer" => Some(SourceClass::Manufacturer),
            "educator" => Some(SourceClass::Educator),
            "estate" => Some(SourceClass::Estate),
            "device-data" => Some(SourceClass::DeviceData),
            _ => Option::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SourceClass::Manufacturer => "manufacturer documentation",
            SourceClass::Educator => "educator source",
            SourceClass::Estate => "estate corpus (provenance labelled)",
            SourceClass::DeviceData => "authored from device data",
        }
    }
}

/// One lexicon entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexiconEntry {
    pub id: String,
    /// family | kink | combo | lesson | sensitivity
    pub section: String,
    /// The family id, kink key, combo id, or chapter key.
    pub key: String,
    pub title: String,
    pub source: SourceClass,
    /// Body text; `\n` in the file is a real newline here.
    pub body: String,
}

/// Why the lexicon could not be parsed.
#[derive(Debug)]
pub enum LexiconError {
    FieldCount { line: usize, found: usize },
    UnknownSource { line: usize, text: String },
    DuplicateId { line: usize, id: String },
}

impl fmt::Display for LexiconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexiconError::FieldCount { line, found } => {
                write!(f, "line {line}: {found} fields, expected {FIELD_COUNT}")
            }
            LexiconError::UnknownSource { line, text } => {
                write!(f, "line {line}: unknown source class {text:?}")
            }
            LexiconError::DuplicateId { line, id } => {
                write!(f, "line {line}: duplicate entry id {id:?}")
            }
        }
    }
}

impl std::error::Error for LexiconError {}

/// Parses the lexicon TSV. First violation aborts — a partially
/// loaded book is never published.
pub fn parse_lexicon(tsv: &str) -> Result<Vec<LexiconEntry>, LexiconError> {
    let mut entries: Vec<LexiconEntry> = Vec::new();
    for (index, line) in tsv.lines().enumerate() {
        let line_no = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != FIELD_COUNT {
            return Err(LexiconError::FieldCount {
                line: line_no,
                found: fields.len(),
            });
        }
        let source = SourceClass::parse(fields[4]).ok_or_else(|| LexiconError::UnknownSource {
            line: line_no,
            text: fields[4].to_owned(),
        })?;
        if entries.iter().any(|e| e.id == fields[0]) {
            return Err(LexiconError::DuplicateId {
                line: line_no,
                id: fields[0].to_owned(),
            });
        }
        entries.push(LexiconEntry {
            id: fields[0].to_owned(),
            section: fields[1].to_owned(),
            key: fields[2].to_owned(),
            title: fields[3].to_owned(),
            source,
            body: fields[5].replace("\\n", "\n"),
        });
    }
    Ok(entries)
}

/// The kink keys of the estate canon the lexicon must cover: the
/// Kama programme's toy-applicability classes and figures, the
/// MoonDeep track canon, and the technique kinks the guide maps.
pub const CANON_KINKS: &[&str] = &[
    "wearable-handsfree",
    "worn-by-command",
    "rider-led",
    "from-behind",
    "dual-stimulation",
    "oral-substitution",
    "edging",
    "shared-device",
    "dairy",
    "slave-tpe",
    "brainwashed-trance",
    "bondage-held",
    "breeding",
    "power-intensity",
    "training-discipline",
    "fullness",
];

/// Coverage gaps in the book: canon kinks without an entry, and
/// families without a family entry or a lesson. Run at startup (a
/// book that silently lost coverage must fail loudly, like the
/// catalogue's sortedness guard) and asserted empty by the tests.
pub fn coverage_gaps(entries: &[LexiconEntry], families: &[String]) -> Vec<String> {
    let mut gaps = Vec::new();
    for kink in CANON_KINKS {
        if !entries
            .iter()
            .any(|e| e.section == "kink" && e.key == *kink)
        {
            gaps.push(format!("kink without entry: {kink}"));
        }
    }
    for family in families {
        if !entries
            .iter()
            .any(|e| e.section == "family" && e.key == *family)
        {
            gaps.push(format!("family without entry: {family}"));
        }
        if !entries
            .iter()
            .any(|e| e.section == "lesson" && e.key == *family)
        {
            gaps.push(format!("family without lesson: {family}"));
        }
    }
    gaps
}

/// Case-insensitive substring search over title, key, and body.
pub fn search(entries: &[LexiconEntry], query: &str) -> Vec<usize> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.title.to_lowercase().contains(&needle)
                || entry.key.to_lowercase().contains(&needle)
                || entry.body.to_lowercase().contains(&needle)
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{FEATURES_TSV, parse_features};

    fn book() -> Vec<LexiconEntry> {
        parse_lexicon(LEXICON_TSV).expect("embedded lexicon must parse")
    }

    #[test]
    fn every_family_has_a_family_entry_and_a_lesson() {
        let entries = book();
        let features = parse_features(FEATURES_TSV).unwrap();
        let mut families: Vec<&str> = features.iter().map(|f| f.family.as_str()).collect();
        families.sort();
        families.dedup();
        for family in families {
            assert!(
                entries
                    .iter()
                    .any(|e| e.section == "family" && e.key == family),
                "no family entry for {family}"
            );
            assert!(
                entries
                    .iter()
                    .any(|e| e.section == "lesson" && e.key == family),
                "no lesson entry for {family}"
            );
        }
    }

    #[test]
    fn every_canon_kink_has_an_entry_with_a_learning_path() {
        let entries = book();
        for kink in CANON_KINKS {
            let entry = entries
                .iter()
                .find(|e| e.section == "kink" && e.key == *kink)
                .unwrap_or_else(|| panic!("no kink entry for {kink}"));
            assert!(
                entry.body.contains("Learning path"),
                "kink entry {kink} lacks its teaching progression"
            );
        }
    }

    #[test]
    fn sensitivity_chapter_covers_the_progression() {
        let entries = book();
        for key in ["beginner", "intermediate", "advanced"] {
            assert!(
                entries
                    .iter()
                    .any(|e| e.section == "sensitivity" && e.key == key),
                "sensitivity chapter missing {key}"
            );
        }
    }

    #[test]
    fn scenarios_respect_the_commissions_limits() {
        // Scenario prescriptions are stated in the body with a
        // "Rig:" line listing the toys; the test parses those lines
        // and enforces <=3 of a kind and <=10 total, like the rig.
        let entries = book();
        let combos: Vec<&LexiconEntry> = entries.iter().filter(|e| e.section == "combo").collect();
        assert!(combos.len() >= 6, "only {} scenarios", combos.len());
        for entry in combos {
            let rig_line = entry
                .body
                .lines()
                .find(|l| l.starts_with("Rig:"))
                .unwrap_or_else(|| panic!("scenario {} has no Rig line", entry.id));
            let kinds: Vec<&str> = rig_line
                .trim_start_matches("Rig:")
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            assert!(
                kinds.len() <= 10,
                "scenario {} prescribes >10 toys",
                entry.id
            );
            let mut sorted = kinds.clone();
            sorted.sort();
            for window in sorted.windows(4) {
                assert!(
                    window[0] != window[3],
                    "scenario {} prescribes >3 of {}",
                    entry.id,
                    window[0]
                );
            }
        }
    }

    #[test]
    fn search_finds_entries() {
        let entries = book();
        assert!(!search(&entries, "wand").is_empty());
        assert_eq!(search(&entries, "").len(), entries.len());
        assert!(search(&entries, "zzz-no-such-term").is_empty());
    }
}
