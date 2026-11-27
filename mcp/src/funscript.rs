//! The one funscript adapter (XToys spec §1.4, KoiSync SPEC-KOISYNC §2):
//! a compile-time translation of `.funscript` JSON into frozen
//! `ache.pattern/v1` data. A single-axis funscript and a KoiSync
//! six-axis pack go through the same code: a pack is a list of
//! channels, and a plain funscript is the degenerate one-channel pack.
//!
//! The imported file is never executed, streamed, or trusted at run
//! time — its only output is a validated pattern per channel.
//!
//! Translation law: consecutive actions become Linear blocks from the
//! earlier `pos` to the later `pos` over the interval (falling segments
//! use the `falling` Linear form, spec extension S-1; equal positions
//! become a Square hold); a first action at `at > 0` is preceded by a
//! Square hold at 0. Adapter output is validated exactly like a
//! hand-authored pattern.
//!
//! Stricter than KoiSync by design (SPEC-KOISYNC D1): non-monotonic or
//! duplicate `at` is an import error here, never silently sorted.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{AcheError, ErrorCode, Result};
use crate::pattern::{
    Block, BlockKind, Function, LoopPolicy, PATTERN_FORMAT, Pattern, PatternSpec, Provenance,
    Section, invalid, valid_id,
};

pub const PACK_FORMAT: &str = "ache.pack/v1";
/// Largest funscript document accepted, bytes of its JSON.
pub const FUNSCRIPT_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The six KoiSync channels in pack order, with their TCode axis
/// (`src/Shared/Motion.cs:66`) and file marker
/// (`src/Shared/Funscript.cs:70`, suffixes `StudioRecorder.cs:92`).
pub const PACK_CHANNELS: [(&str, &str, Function); 6] = [
    ("stroke", "L0", Function::StrokePosition),
    ("surge", "L1", Function::Surge),
    ("sway", "L2", Function::Sway),
    ("twist", "R0", Function::Twist),
    ("roll", "R1", Function::Roll),
    ("pitch", "R2", Function::Pitch),
];

#[derive(Deserialize)]
struct RawAction {
    at: i64,
    pos: i64,
}

/// Funscripts carry free-form extras (version, range, metadata, …);
/// only `actions`, `inverted`, and `metadata` title/creator are read.
#[derive(Deserialize)]
struct RawFunscript {
    actions: Vec<RawAction>,
    #[serde(default)]
    inverted: bool,
    #[serde(default)]
    metadata: Option<Value>,
}

/// One channel of an import: the pattern it compiled to.
#[derive(Clone, Debug)]
pub struct Channel {
    pub function: Function,
    pub pattern: Pattern,
}

/// A validated pack: one pattern per present channel, a common time
/// base, all-or-none (§3.3).
#[derive(Clone, Debug)]
pub struct Pack {
    pub id: String,
    pub channels: Vec<Channel>,
    pub total_ms: u64,
    pub hash: String,
}

fn metadata_text(metadata: &Option<Value>, key: &str) -> Option<String> {
    metadata
        .as_ref()
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(128).collect())
}

/// Translate one funscript document into a pattern for `function`.
pub fn import(id: &str, function: Function, document: &Value) -> Result<Pattern> {
    valid_id("funscript", id)?;
    let size = serde_json::to_vec(document)
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    if size > FUNSCRIPT_BYTES_MAX {
        return invalid(format!("funscript exceeds {FUNSCRIPT_BYTES_MAX} bytes"));
    }
    let raw: RawFunscript = serde_json::from_value(document.clone()).map_err(|e| {
        AcheError::new(ErrorCode::RequestInvalid, format!("invalid funscript: {e}"))
    })?;
    if raw.actions.len() < 2 {
        return invalid("funscript needs at least two actions");
    }
    let mut previous_at: Option<i64> = None;
    for (i, action) in raw.actions.iter().enumerate() {
        if !(0..=100).contains(&action.pos) {
            return invalid(format!("action {i}: pos {} is outside 0..=100", action.pos));
        }
        if action.at < 0 {
            return invalid(format!("action {i}: at {} is negative", action.at));
        }
        if let Some(previous) = previous_at
            && action.at <= previous
        {
            return invalid(format!(
                "action {i}: at {} is not after {previous}; `at` must increase strictly",
                action.at
            ));
        }
        previous_at = Some(action.at);
    }
    let pos = |a: &RawAction| -> u8 {
        let p = a.pos as u8;
        if raw.inverted { 100 - p } else { p }
    };
    let mut blocks = Vec::with_capacity(raw.actions.len());
    let first = &raw.actions[0];
    if first.at > 0 {
        blocks.push(Block {
            kind: BlockKind::Square,
            top: 0,
            bottom: 0,
            duration_ms: first.at as u64,
            speed: 1.0,
            loops: 1,
            falling: false,
        });
    }
    for pair in raw.actions.windows(2) {
        let (from, to) = (pos(&pair[0]), pos(&pair[1]));
        let duration_ms = (pair[1].at - pair[0].at) as u64;
        let block = if from == to {
            Block {
                kind: BlockKind::Square,
                top: from,
                bottom: 0,
                duration_ms,
                speed: 1.0,
                loops: 1,
                falling: false,
            }
        } else {
            Block {
                kind: BlockKind::Linear,
                top: from.max(to),
                bottom: from.min(to),
                duration_ms,
                speed: 1.0,
                loops: 1,
                falling: to < from,
            }
        };
        blocks.push(block);
    }
    let title = metadata_text(&raw.metadata, "title");
    let creator = metadata_text(&raw.metadata, "creator");
    Pattern::load(PatternSpec {
        format: PATTERN_FORMAT.to_string(),
        id: id.to_string(),
        function,
        sections: Some(vec![Section { blocks }]),
        scripted: None,
        loop_policy: LoopPolicy::Once,
        provenance: Some(Provenance {
            source: Some("funscript".to_string()),
            title,
            creator,
        }),
    })
}

/// Import a multi-axis pack: `{"format": "ache.pack/v1", "id": …,
/// "channels": {"stroke": <funscript>, "surge": <funscript>, …}}`.
/// Channel keys are the KoiSync markers or the TCode axis names.
/// `stroke` (the main channel) is required. All channels must share
/// one duration; any channel failing fails the whole pack.
pub fn import_pack(document: &Value) -> Result<Pack> {
    let object = document
        .as_object()
        .ok_or_else(|| AcheError::new(ErrorCode::RequestInvalid, "pack must be an object"))?;
    for key in object.keys() {
        if !matches!(key.as_str(), "format" | "id" | "channels") {
            return invalid(format!("pack has unknown field {key:?}"));
        }
    }
    if object.get("format").and_then(Value::as_str) != Some(PACK_FORMAT) {
        return invalid(format!("pack format must be {PACK_FORMAT:?}"));
    }
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| AcheError::new(ErrorCode::RequestInvalid, "pack needs an id"))?;
    valid_id("pack", id)?;
    let channels = object
        .get("channels")
        .and_then(Value::as_object)
        .ok_or_else(|| AcheError::new(ErrorCode::RequestInvalid, "pack needs a channels object"))?;
    let mut slots: [Option<&Value>; 6] = [None; 6];
    for (key, value) in channels {
        let Some(slot) = PACK_CHANNELS.iter().position(|(marker, axis, _)| {
            key.eq_ignore_ascii_case(marker) || key.eq_ignore_ascii_case(axis)
        }) else {
            return invalid(format!("pack channel {key:?} is not one of the six axes"));
        };
        if slots[slot].is_some() {
            return invalid(format!("pack channel {key:?} is given twice"));
        }
        slots[slot] = Some(value);
    }
    if slots[0].is_none() {
        return invalid("pack needs its main (stroke / L0) channel");
    }
    let mut out = Vec::new();
    for (slot, document) in slots.iter().enumerate() {
        let Some(document) = document else { continue };
        let (marker, _, function) = PACK_CHANNELS[slot];
        let pattern = import(&format!("{id}.{marker}"), function, document)
            .map_err(|e| AcheError::new(e.code, format!("pack channel {marker}: {}", e.message)))?;
        out.push(Channel { function, pattern });
    }
    let total_ms = out[0].pattern.total_ms();
    if let Some(bad) = out.iter().find(|c| c.pattern.total_ms() != total_ms) {
        return invalid(format!(
            "pack channels disagree on duration: {} is {} ms, main is {total_ms} ms",
            bad.function.as_str(),
            bad.pattern.total_ms()
        ));
    }
    let joined: String = out
        .iter()
        .map(|c| c.pattern.hash())
        .collect::<Vec<_>>()
        .join(":");
    Ok(Pack {
        id: id.to_string(),
        hash: crate::pattern::content_hash(format!("{PACK_FORMAT}:{id}:{joined}").as_bytes()),
        channels: out,
        total_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn script(actions: &[(i64, i64)]) -> Value {
        json!({
            "version": "1.0", "inverted": false, "range": 100,
            "actions": actions.iter().map(|(at, pos)| json!({"at": at, "pos": pos})).collect::<Vec<_>>()
        })
    }

    // --- validation (V3) ---------------------------------------------

    #[test]
    fn v3_known_funscript_compiles_to_the_expected_block_list() {
        let p = import(
            "f",
            Function::Vibrate,
            &script(&[(500, 20), (1500, 80), (2000, 80), (3000, 0)]),
        )
        .unwrap();
        let blocks = &p.spec().sections.as_ref().unwrap()[0].blocks;
        let shape: Vec<(BlockKind, u8, u8, u64, bool)> = blocks
            .iter()
            .map(|b| (b.kind, b.top, b.bottom, b.duration_ms, b.falling))
            .collect();
        assert_eq!(
            shape,
            vec![
                (BlockKind::Square, 0, 0, 500, false),
                (BlockKind::Linear, 80, 20, 1000, false),
                (BlockKind::Square, 80, 0, 500, false),
                (BlockKind::Linear, 80, 0, 1000, true),
            ]
        );
        assert_eq!(p.total_ms(), 3000);
        // Sampled, the curve follows the reader's linear law
        // (KoiSync Funscript.cs:35-45): halfway up is 50.
        let s = p.sample(250, 0);
        assert_eq!(s[0], 0);
        assert_eq!(s[2], 20);
        assert_eq!(s[4], 50);
        assert_eq!(s[6], 80);
        assert_eq!(s[10], 40);
        assert_eq!(
            p.spec().provenance.as_ref().unwrap().source.as_deref(),
            Some("funscript")
        );
    }

    #[test]
    fn inverted_flag_flips_positions() {
        let mut doc = script(&[(0, 10), (100, 30)]);
        doc["inverted"] = json!(true);
        let p = import("f", Function::Vibrate, &doc).unwrap();
        let b = &p.spec().sections.as_ref().unwrap()[0].blocks[0];
        assert_eq!((b.top, b.bottom, b.falling), (90, 70, true));
    }

    #[test]
    fn v3_six_axis_pack_compiles_to_six_aligned_patterns() {
        let doc = json!({
            "format": PACK_FORMAT, "id": "scene",
            "channels": {
                "stroke": script(&[(0, 10), (1000, 90)]),
                "surge": script(&[(0, 50), (1000, 60)]),
                "sway": script(&[(0, 50), (1000, 40)]),
                "R0": script(&[(0, 50), (1000, 50)]),
                "roll": script(&[(0, 45), (1000, 55)]),
                "pitch": script(&[(0, 50), (1000, 70)]),
            }
        });
        let pack = import_pack(&doc).unwrap();
        assert_eq!(pack.channels.len(), 6);
        assert_eq!(pack.total_ms, 1000);
        let functions: Vec<Function> = pack.channels.iter().map(|c| c.function).collect();
        assert_eq!(
            functions,
            vec![
                Function::StrokePosition,
                Function::Surge,
                Function::Sway,
                Function::Twist,
                Function::Roll,
                Function::Pitch
            ]
        );
        for channel in &pack.channels {
            assert_eq!(
                channel.pattern.sample(20, 0).len(),
                50,
                "50 Hz grid, shared base"
            );
        }
        assert_eq!(pack.hash, import_pack(&doc).unwrap().hash);
    }

    #[test]
    fn single_axis_is_the_degenerate_pack() {
        let doc = json!({"format": PACK_FORMAT, "id": "one", "channels": {"stroke": script(&[(0, 0), (400, 100)])}});
        let pack = import_pack(&doc).unwrap();
        let plain = import(
            "one.stroke",
            Function::StrokePosition,
            &script(&[(0, 0), (400, 100)]),
        )
        .unwrap();
        assert_eq!(pack.channels.len(), 1);
        assert_eq!(pack.channels[0].pattern.hash(), plain.hash());
    }

    // --- adversarial (A5) --------------------------------------------

    #[test]
    fn a5_malformed_funscripts_are_import_errors() {
        for actions in [
            vec![(0, 10)],
            vec![(0, 10), (0, 20)],
            vec![(100, 10), (50, 20)],
            vec![(0, 101), (10, 5)],
            vec![(0, -1), (10, 5)],
            vec![(-5, 1), (10, 5)],
            vec![(0, 1), (1_800_001, 5)],
        ] {
            assert!(
                import("f", Function::Vibrate, &script(&actions)).is_err(),
                "{actions:?}"
            );
        }
        assert!(import("f", Function::Vibrate, &json!({"actions": "x"})).is_err());
        assert!(import("f", Function::Vibrate, &json!({"actions": [{"at": 0}]})).is_err());
    }

    #[test]
    fn a5_packs_load_all_or_none() {
        let good = script(&[(0, 10), (1000, 90)]);
        let short = script(&[(0, 10), (900, 90)]);
        let bad = script(&[(0, 10), (0, 90)]);
        for channels in [
            json!({"stroke": good, "surge": short}),
            json!({"stroke": good, "surge": bad}),
            json!({"surge": good}),
            json!({"stroke": good, "vibrate": good}),
            json!({"stroke": good, "L0": good}),
        ] {
            let doc = json!({"format": PACK_FORMAT, "id": "p", "channels": channels});
            assert!(import_pack(&doc).is_err(), "{doc}");
        }
        let extra =
            json!({"format": PACK_FORMAT, "id": "p", "channels": {"stroke": good}, "exec": "x"});
        assert!(import_pack(&extra).is_err());
    }
}
