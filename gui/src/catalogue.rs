//! Device catalogue: the Phase 0 ache x IoST classification as typed data.
//!
//! The catalogue of record is the Phase 0 deliverable pair `DIFF.md` +
//! `URLS.md` (471 devices). `tools/generate_catalogue.py` joins them into
//! `data/catalogue.tsv`, which this module embeds and parses at startup.
//!
//! Contract:
//! - Accepted input: the generated TSV only — comment lines starting with
//!   `#`, then one row per device with exactly [`FIELD_COUNT`] tab-separated
//!   fields. Anything else is a typed [`CatalogueError`], never a panic.
//! - Bounds: at most [`ROW_COUNT_MAX`] rows; every field is length-checked
//!   against [`FIELD_BYTES_MAX`] before allocation of the row's strings.
//! - Order: rows are brand-then-device, case-insensitive (IoST table order).
//!   [`is_sorted`] is the guard the test suite runs against the embedded
//!   data, so a bad regeneration fails `cargo test`, not the user.

use std::fmt;

/// The generated catalogue, embedded at compile time.
pub const CATALOGUE_TSV: &str = include_str!("../data/catalogue.tsv");

/// Fields per catalogue row: brand, device, type, bucket, untested,
/// link state, url.
pub const FIELD_COUNT: usize = 7;

/// Upper bound on catalogue rows accepted by the parser.
pub const ROW_COUNT_MAX: usize = 10_000;

/// Upper bound on one field's length in bytes (the longest real field is a
/// redirect URL of a few hundred bytes; this leaves generous headroom).
pub const FIELD_BYTES_MAX: usize = 4_096;

/// Support bucket from the Phase 0 diff: how ache covers the device today.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bucket {
    /// Already supported, named config evidence.
    A,
    /// Already supported via a protocol-generic (defaults/wildcard) config.
    AGeneric,
    /// Protocol exists in the pinned stack but ache surfaces no function.
    B,
    /// No protocol coverage anywhere yet.
    C,
}

impl Bucket {
    /// The label used in the data file and the UI.
    pub fn label(self) -> &'static str {
        match self {
            Bucket::A => "A",
            Bucket::AGeneric => "A-g",
            Bucket::B => "B",
            Bucket::C => "C",
        }
    }

    fn parse(text: &str) -> Option<Bucket> {
        match text {
            "A" => Some(Bucket::A),
            "A-g" => Some(Bucket::AGeneric),
            "B" => Some(Bucket::B),
            "C" => Some(Bucket::C),
            _ => None,
        }
    }
}

/// English-availability state of the device's IoST-listed link, as verified
/// by the Phase 0 fetch pass. `Unverified` means the site refused automated
/// fetches — the link is unjudged, not broken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkState {
    /// English available as served (verified by fetching).
    En,
    /// English available at a verified locale variant (none in Phase 0).
    EnVariant,
    /// Page fetched; no English version found.
    NonEn,
    /// Verified unreachable (HTTP 404/410 on both fetch passes).
    Dead,
    /// IoST lists no link for this device.
    NoLink,
    /// Site refused automated fetches; language state honestly unknown.
    Unverified,
}

impl LinkState {
    /// The label used in the data file and the UI.
    pub fn label(self) -> &'static str {
        match self {
            LinkState::En => "EN",
            LinkState::EnVariant => "EN-VARIANT",
            LinkState::NonEn => "NON-EN",
            LinkState::Dead => "DEAD",
            LinkState::NoLink => "NOLINK",
            LinkState::Unverified => "UNVERIFIED",
        }
    }

    fn parse(text: &str) -> Option<LinkState> {
        match text {
            "EN" => Some(LinkState::En),
            "EN-VARIANT" => Some(LinkState::EnVariant),
            "NON-EN" => Some(LinkState::NonEn),
            "DEAD" => Some(LinkState::Dead),
            "NOLINK" => Some(LinkState::NoLink),
            "UNVERIFIED" => Some(LinkState::Unverified),
            _ => None,
        }
    }
}

/// One catalogue row: a device, its support bucket, and its link state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Device {
    /// Manufacturer brand, e.g. "Lovense".
    pub brand: String,
    /// Device name within the brand, e.g. "Lush 3".
    pub name: String,
    /// IoST device type, e.g. "Love Egg" (may list several, comma-separated).
    pub device_type: String,
    /// Support bucket (see [`Bucket`]).
    pub bucket: Bucket,
    /// IoST flags upstream buttplug support for this device "(Untested)".
    pub untested: bool,
    /// English-availability state of the listed link.
    pub link_state: LinkState,
    /// The IoST-listed URL, verbatim. `None` iff `link_state` is
    /// [`LinkState::NoLink`] — a missing link is shown, never invented.
    pub url: Option<String>,
}

/// Why a catalogue could not be parsed. Expected failure on bad input;
/// internal inconsistencies in well-formed input are asserted instead.
#[derive(Debug)]
pub enum CatalogueError {
    /// A row did not have exactly [`FIELD_COUNT`] fields.
    FieldCount {
        /// 1-based line number in the data file.
        line: usize,
        /// Number of fields found.
        found: usize,
    },
    /// A field exceeded [`FIELD_BYTES_MAX`].
    FieldTooLong {
        /// 1-based line number in the data file.
        line: usize,
    },
    /// A bucket or link-state cell held an unknown label.
    UnknownLabel {
        /// 1-based line number in the data file.
        line: usize,
        /// The offending cell text (bounded by the field length check).
        text: String,
    },
    /// The untested cell was neither "0" nor "1".
    BadUntestedFlag {
        /// 1-based line number in the data file.
        line: usize,
    },
    /// A NOLINK row carried a URL, or a linked row carried none.
    LinkMismatch {
        /// 1-based line number in the data file.
        line: usize,
    },
    /// A URL did not start with http:// or https://.
    BadUrlScheme {
        /// 1-based line number in the data file.
        line: usize,
    },
    /// More than [`ROW_COUNT_MAX`] rows.
    TooManyRows,
}

impl fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogueError::FieldCount { line, found } => {
                write!(f, "line {line}: {found} fields, expected {FIELD_COUNT}")
            }
            CatalogueError::FieldTooLong { line } => {
                write!(f, "line {line}: field over {FIELD_BYTES_MAX} bytes")
            }
            CatalogueError::UnknownLabel { line, text } => {
                write!(f, "line {line}: unknown bucket/link-state label {text:?}")
            }
            CatalogueError::BadUntestedFlag { line } => {
                write!(f, "line {line}: untested flag is not 0 or 1")
            }
            CatalogueError::LinkMismatch { line } => {
                write!(f, "line {line}: URL presence disagrees with NOLINK state")
            }
            CatalogueError::BadUrlScheme { line } => {
                write!(f, "line {line}: URL is not http:// or https://")
            }
            CatalogueError::TooManyRows => {
                write!(f, "more than {ROW_COUNT_MAX} catalogue rows")
            }
        }
    }
}

impl std::error::Error for CatalogueError {}

/// Parses catalogue TSV into devices, in file order.
///
/// Comment lines (starting with `#`) and blank lines are skipped. Every
/// other line must satisfy the row contract described on the module; the
/// first violation aborts the whole parse with a typed error — a partially
/// loaded catalogue is never published.
pub fn parse_catalogue(tsv: &str) -> Result<Vec<Device>, CatalogueError> {
    let mut devices: Vec<Device> = Vec::new();
    for (index, line) in tsv.lines().enumerate() {
        let line_no = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if devices.len() >= ROW_COUNT_MAX {
            return Err(CatalogueError::TooManyRows);
        }
        devices.push(parse_row(line, line_no)?);
    }
    Ok(devices)
}

fn parse_row(line: &str, line_no: usize) -> Result<Device, CatalogueError> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != FIELD_COUNT {
        return Err(CatalogueError::FieldCount {
            line: line_no,
            found: fields.len(),
        });
    }
    for field in &fields {
        if field.len() > FIELD_BYTES_MAX {
            return Err(CatalogueError::FieldTooLong { line: line_no });
        }
    }
    let bucket = Bucket::parse(fields[3]).ok_or_else(|| CatalogueError::UnknownLabel {
        line: line_no,
        text: fields[3].to_owned(),
    })?;
    let link_state = LinkState::parse(fields[5]).ok_or_else(|| CatalogueError::UnknownLabel {
        line: line_no,
        text: fields[5].to_owned(),
    })?;
    let untested = match fields[4] {
        "0" => false,
        "1" => true,
        _ => return Err(CatalogueError::BadUntestedFlag { line: line_no }),
    };
    let url = if fields[6].is_empty() {
        None
    } else {
        Some(fields[6].to_owned())
    };
    if url.is_some() != (link_state != LinkState::NoLink) {
        return Err(CatalogueError::LinkMismatch { line: line_no });
    }
    if let Some(url_text) = &url
        && !url_text.starts_with("https://")
        && !url_text.starts_with("http://")
    {
        return Err(CatalogueError::BadUrlScheme { line: line_no });
    }
    Ok(Device {
        brand: fields[0].to_owned(),
        name: fields[1].to_owned(),
        device_type: fields[2].to_owned(),
        bucket,
        untested,
        link_state,
        url,
    })
}

/// The sort key for one device: brand, then device name, case-insensitive,
/// with the exact strings as tie-break (the generator's collation).
fn sort_key(device: &Device) -> (String, String, String, String) {
    (
        device.brand.to_lowercase(),
        device.name.to_lowercase(),
        device.brand.clone(),
        device.name.clone(),
    )
}

/// True iff `devices` is in catalogue order (see [`sort_key`]). This is the
/// sortedness guard: the test suite runs it against the embedded data.
pub fn is_sorted(devices: &[Device]) -> bool {
    devices
        .windows(2)
        .all(|pair| sort_key(&pair[0]) <= sort_key(&pair[1]))
}

/// True iff `device` matches a free-text catalogue search: case-insensitive
/// substring over brand, device name, and device type. An empty or
/// all-whitespace query matches everything.
pub fn matches_query(device: &Device, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    device.brand.to_lowercase().contains(&needle)
        || device.name.to_lowercase().contains(&needle)
        || device.device_type.to_lowercase().contains(&needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> Vec<Device> {
        parse_catalogue(CATALOGUE_TSV).expect("embedded catalogue must parse")
    }

    fn count_bucket(devices: &[Device], bucket: Bucket) -> usize {
        devices.iter().filter(|d| d.bucket == bucket).count()
    }

    fn count_state(devices: &[Device], state: LinkState) -> usize {
        devices.iter().filter(|d| d.link_state == state).count()
    }

    #[test]
    fn embedded_catalogue_has_471_devices() {
        assert_eq!(embedded().len(), 471);
    }

    #[test]
    fn sortedness_guard_holds_for_embedded_catalogue() {
        assert!(
            is_sorted(&embedded()),
            "catalogue is not in brand/device order"
        );
    }

    #[test]
    fn first_and_last_entries_match_phase0_table() {
        let devices = embedded();
        let first = devices.first().expect("non-empty");
        assert_eq!(
            (first.brand.as_str(), first.name.as_str()),
            ("Amorelie Joy", "Boogie")
        );
        let last = devices.last().expect("non-empty");
        assert_eq!(
            (last.brand.as_str(), last.name.as_str()),
            ("Zalo", "Versailles Jeanne")
        );
    }

    #[test]
    fn bucket_tally_matches_phase0() {
        let devices = embedded();
        assert_eq!(count_bucket(&devices, Bucket::A), 328);
        assert_eq!(count_bucket(&devices, Bucket::AGeneric), 62);
        assert_eq!(count_bucket(&devices, Bucket::B), 1);
        assert_eq!(count_bucket(&devices, Bucket::C), 80);
    }

    #[test]
    fn link_state_tally_matches_phase0() {
        let devices = embedded();
        assert_eq!(count_state(&devices, LinkState::En), 255);
        assert_eq!(count_state(&devices, LinkState::EnVariant), 0);
        assert_eq!(count_state(&devices, LinkState::NonEn), 11);
        assert_eq!(count_state(&devices, LinkState::Dead), 37);
        assert_eq!(count_state(&devices, LinkState::NoLink), 4);
        assert_eq!(count_state(&devices, LinkState::Unverified), 164);
    }

    #[test]
    fn untested_count_matches_phase0() {
        assert_eq!(embedded().iter().filter(|d| d.untested).count(), 52);
    }

    #[test]
    fn the_single_bucket_b_device_is_minna_kgoal_boost() {
        let devices = embedded();
        let b_rows: Vec<&Device> = devices.iter().filter(|d| d.bucket == Bucket::B).collect();
        assert_eq!(b_rows.len(), 1);
        assert_eq!(
            (b_rows[0].brand.as_str(), b_rows[0].name.as_str()),
            ("Minna", "kGoal Boost")
        );
    }

    #[test]
    fn search_matches_brand_name_and_type_case_insensitively() {
        let devices = embedded();
        let by_brand = devices.iter().filter(|d| matches_query(d, "zalo")).count();
        assert_eq!(by_brand, 3);
        let by_name = devices
            .iter()
            .filter(|d| matches_query(d, "VERSAILLES"))
            .count();
        assert_eq!(by_name, 1);
        let by_type = devices
            .iter()
            .filter(|d| matches_query(d, "love egg"))
            .count();
        assert!(by_type > 10, "expected many Love Egg rows, got {by_type}");
        assert!(devices.iter().all(|d| matches_query(d, "  ")));
    }

    #[test]
    fn parse_rejects_malformed_rows() {
        assert!(parse_catalogue("a\tb\tc\n").is_err());
        assert!(parse_catalogue("A\tB\tT\tX\t0\tEN\thttps://example.com\n").is_err());
        assert!(parse_catalogue("A\tB\tT\tA\t2\tEN\thttps://example.com\n").is_err());
        assert!(parse_catalogue("A\tB\tT\tA\t0\tNOLINK\thttps://example.com\n").is_err());
        assert!(parse_catalogue("A\tB\tT\tA\t0\tEN\tftp://example.com\n").is_err());
        let one = parse_catalogue("A\tB\tT\tA-g\t1\tDEAD\thttps://example.com/x\n")
            .expect("well-formed row parses");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].bucket, Bucket::AGeneric);
        assert!(one[0].untested);
    }
}
