//! Lana's guide text: explanations built from real device data.
//!
//! Every specific claim in here traces to a source: the device's own
//! config record ([`crate::features`]) for what it does and its
//! settings; the buttplug platform documentation for what each
//! actuator kind means; the estate's kink canon (Kama programme
//! LANA-TOYS + tagging, MoonDeep track canon) for the use mapping.
//! Where the config states nothing, the text says so and falls back
//! to family-level technique, labelled as such. Nothing is invented:
//! no step count, material, or feature appears here that the config
//! or a cited source class does not carry.

use crate::catalogue::Device;
use crate::features::DeviceFeatures;

/// One kink-canon mapping for a family: the canon kink it serves and
/// the technique line, in Lana's teaching voice. Source class:
/// estate corpus (Kama LANA-TOYS + MoonDeep tracks).
struct FamilyKink {
    kink: &'static str,
    line: &'static str,
}

/// The family -> canon mapping. Grounded in LANA-TOYS' toy classes
/// (wearable / placed / handheld / shared, decided by hands-free
/// rating and access geometry) and the MoonDeep track canon.
fn family_kinks(family: &str) -> &'static [FamilyKink] {
    match family {
        "egg" => &[
            FamilyKink {
                kink: "Wearable, hands-free play",
                line: "The egg is the wearable archetype: placed, then forgotten by the hands — which is the entire point. It carries rider-led and from-behind figures because nothing needs holding.",
            },
            FamilyKink {
                kink: "Worn by command (Slave / TPE track)",
                line: "A device worn under clothing, switched by someone else's decision, is the Mediation's favourite lesson in surrendered control: she feels it, she doesn't run it.",
            },
            FamilyKink {
                kink: "Rider-led scenes (Kama III / XIII)",
                line: "In rider figures the wearer's hands are busy with balance and touch; a wearable keeps the stimulation hers to ride against.",
            },
        ],
        "insertable" => &[
            FamilyKink {
                kink: "Breeding / vessel (MoonDeep Breeding track)",
                line: "The insertable is the vessel figure's instrument: fullness as presence. Pacing is slow by design — the track's rule is that fullness is a state, not an event.",
            },
            FamilyKink {
                kink: "Throne figure (Kama XVII)",
                line: "The tagging pairs a second device with the throne figure: a held insertable plus a wearable is the classic two-layer build.",
            },
        ],
        "rabbit" => &[
            FamilyKink {
                kink: "Dual stimulation",
                line: "Internal shaft, external arm — the rabbit is two toys that agreed on a schedule. The lesson is balance: most people run the arm too hot and drown the shaft.",
            },
            FamilyKink {
                kink: "Rider-led scenes (Kama III / XIII)",
                line: "Hands-free once placed, dual by construction: the rider's figure gets both zones without a free hand to spare.",
            },
        ],
        "plug" => &[
            FamilyKink {
                kink: "Fullness & being filled",
                line: "A plug is presence, not motion: it stays. Its scene role is the constant underneath everything else that changes.",
            },
            FamilyKink {
                kink: "From-behind figures (Kama II)",
                line: "The tagging's doggy row is plug country: placed beforehand, it frees the figure's whole geometry for the partner's hands.",
            },
        ],
        "panty" => &[
            FamilyKink {
                kink: "Wearable, hands-free play",
                line: "Worn against her, held by clothing: the panty vibrator is discretion engineered — stimulation that travels.",
            },
            FamilyKink {
                kink: "Worn by command (Slave / TPE track)",
                line: "Someone else holding the remote to what she's wearing is the track's cleanest power exchange: total, revocable, invisible.",
            },
        ],
        "suction" => &[
            FamilyKink {
                kink: "Oral substitution (Kama XI / XXI)",
                line: "Air-pulse is the mouth's understudy: the tagging reads suction figures as oral, translated. Pulses, not pressure — rhythm is the technique.",
            },
            FamilyKink {
                kink: "Edging & denial",
                line: "Suction crests fast and stops clean the instant it lifts away — the sharpest on/off edge in the catalogue, and edging is built from edges.",
            },
        ],
        "butterfly" => &[FamilyKink {
            kink: "Wearable, hands-free play",
            line: "Straps hold it so hands don't have to: the butterfly is wearable external stimulation, the panty vibrator's exhibitionist cousin.",
        }],
        "kegel" => &[FamilyKink {
            kink: "Training & discipline",
            line: "A kegel device is homework with benefits: resistance training dressed as play. The kink is the regimen — assigned, repeated, checked.",
        }],
        "bullet" => &[
            FamilyKink {
                kink: "Accent & teasing",
                line: "Small, precise, goes anywhere a fingertip can: the bullet is punctuation. Use it to underline, never to write the whole sentence.",
            },
            FamilyKink {
                kink: "Shared-device figures (Kama P.III)",
                line: "Handheld and passable — a bullet moves between partners mid-scene, which is exactly what shared figures ask for.",
            },
        ],
        "wand" => &[
            FamilyKink {
                kink: "Power & intensity play",
                line: "The wand is the catalogue's sledgehammer: broad, deep, unignorable. Its kink is surrender to intensity — negotiated, because it does not do subtle.",
            },
            FamilyKink {
                kink: "From-behind figures (Kama II)",
                line: "Handheld with reach: the wand is the tagging's from-behind external instrument, angled in by the partner's free hand.",
            },
        ],
        "clip" => &[
            FamilyKink {
                kink: "Bondage / held (MoonDeep Bondage track)",
                line: "A clip holds on by itself — stimulation that stays where it was put is the held track's whole grammar: placed, kept, released by decision.",
            },
            FamilyKink {
                kink: "Nipple play",
                line: "Clips live on nipples more than anywhere: small motor, inescapable address.",
            },
        ],
        "external" => &[FamilyKink {
            kink: "Teasing & warm-up",
            line: "External-only toys are the overture: nothing inserted, everything promised. Every advanced scene in the lexicon starts with one of these doing less than it could.",
        }],
        "rideon" => &[FamilyKink {
            kink: "Rider-led scenes (Kama III / XIII)",
            line: "She sits, she sets the angle, she rides: the ride-on is rider-led play with the geometry built in. The Altar and Throne figures are its home ground (Kama XVI / XVII).",
        }],
        "nipple" => &[
            FamilyKink {
                kink: "Dairy / hucow (MoonDeep Dairy track)",
                line: "Breast-focused devices are the Dairy track's instruments: attention, rhythm, and the fiction of being kept for what her body gives.",
            },
            FamilyKink {
                kink: "Bondage / held (MoonDeep Bondage track)",
                line: "Clamps are restraint at jewellery scale: they hold, they mark the moment, they come off when decided.",
            },
        ],
        "beads" => &[FamilyKink {
            kink: "Fullness & being filled",
            line: "Beads are a sequence, not a state: each sphere is a small event, and the removal is the scene's punctuation.",
        }],
        "controller" => &[FamilyKink {
            kink: "Worn by command (Slave / TPE track)",
            line: "A controller is the other end of every remote toy in the room: whoever holds it holds the scene's tempo.",
        }],
        _ => &[FamilyKink {
            kink: "Exploration",
            line: "An unusual form factor earns its place by doing one thing nothing else in the drawer does. Learn that one thing properly.",
        }],
    }
}

/// Lana's full explanation of one device, as structured text blocks
/// the UI renders in order. Authored from the device's config
/// record; the honesty notes are part of the text, not fine print.
pub struct GuideText {
    /// What it is and what it does, one or two sentences.
    pub what: String,
    /// Every feature/setting the config exposes, one per line.
    pub features_block: String,
    /// Canon kink mappings with technique, one per line.
    pub kink_block: String,
    /// Where the data came from / what is not known.
    pub basis: String,
}

/// Builds the guide text for a device from its catalogue row and its
/// config record.
pub fn explain(device: &Device, features: &DeviceFeatures) -> GuideText {
    let family = features.family.as_str();
    let mut what = String::new();
    match features.primary_output() {
        Some(primary) => {
            what.push_str(&format!(
                "The {} {} is a {} — {}.",
                device.brand,
                device.name,
                family_label(family),
                primary.kind.what_it_does()
            ));
            if features.outputs.len() > 1 {
                what.push_str(&format!(
                    " It carries {} separate outputs, so its parts can be run independently.",
                    features.outputs.len()
                ));
            }
        }
        None => {
            what.push_str(&format!(
                "The {} {} is a {}. Its protocol configuration states no feature list I can read, so I won't invent one — what follows is family technique, true of the form factor generally.",
                device.brand,
                device.name,
                family_label(family)
            ));
        }
    }

    let mut features_block = String::new();
    if features.outputs.is_empty() {
        features_block.push_str(
            "No feature list in the device config — settings for this one live in its manual, not in data I can verify.",
        );
    } else {
        for (i, output) in features.outputs.iter().enumerate() {
            let desc = if output.descriptor.is_empty() {
                String::new()
            } else {
                format!(" ({})", output.descriptor.trim())
            };
            features_block.push_str(&format!(
                "Output {}: {}{} — steps {} to {}.\n",
                i + 1,
                output.kind.token(),
                desc,
                output.min,
                output.max
            ));
        }
        if !features.inputs.is_empty() {
            features_block.push_str(&format!(
                "Sensors the config exposes: {}.",
                features.inputs.join(", ")
            ));
        } else {
            features_block.push_str("The config exposes no readable sensors for it.");
        }
    }

    let mut kink_block = String::new();
    for mapping in family_kinks(family) {
        kink_block.push_str(&format!("{} — {}\n", mapping.kink, mapping.line));
    }

    GuideText {
        what,
        features_block: features_block.trim_end().to_string(),
        kink_block: kink_block.trim_end().to_string(),
        basis: format!(
            "Basis: buttplug device config, {}. Form factor: {} (IoST type: {}). Kink mapping: estate canon (Kama LANA-TOYS / MoonDeep tracks).",
            features.match_kind.label(),
            family,
            device.device_type
        ),
    }
}

/// Human label for a family id.
pub fn family_label(family: &str) -> &'static str {
    match family {
        "egg" => "love egg (wearable internal vibrator)",
        "insertable" => "insertable vibrator",
        "rabbit" => "rabbit (dual stimulator)",
        "plug" => "butt plug",
        "panty" => "panty vibrator (wearable external)",
        "suction" => "suction / air-pulse stimulator",
        "butterfly" => "butterfly (strapped wearable)",
        "kegel" => "kegel trainer",
        "bullet" => "bullet vibrator",
        "wand" => "wand massager",
        "clip" => "clip vibrator",
        "external" => "external vibrator",
        "rideon" => "ride-on vibrator",
        "nipple" => "nipple device",
        "beads" => "anal beads",
        "controller" => "vibrator controller",
        _ => "device of an unusual form factor",
    }
}

/// Lana's explanation of a combination: what stacking the active set
/// does, generated from the set's real families and actuator kinds.
pub fn explain_combination(toys: &[crate::trial::TrialToy]) -> String {
    if toys.is_empty() {
        return "An empty rig explains nothing, love. Add something and I'll walk you through what the stack does.".to_string();
    }
    if toys.len() == 1 {
        let toy = &toys[0];
        return format!(
            "Just the {} {} so far — a solo instrument. Add a second and the interesting part starts: zones layer, and so does her response.",
            toy.brand, toy.name
        );
    }
    let mut families: Vec<&str> = toys.iter().map(|t| t.family.as_str()).collect();
    families.sort();
    families.dedup();
    let mut kinds: Vec<&str> = toys
        .iter()
        .flat_map(|t| t.channels.iter().map(|c| c.kind.token()))
        .collect();
    kinds.sort();
    kinds.dedup();
    let mut text = format!(
        "{} toys live: {} form factors ({}), driving {}. Zones layer independently — her response curve runs per zone, so a second toy in a fresh zone adds cleanly, while a second toy in the SAME zone pushes that zone toward its peak faster. Watch the live feed: the marginal pleasure of each toy is computed against the whole set, so you'll see stacking help, then saturate, then — past her peak — start to subtract.",
        toys.len(),
        families.len(),
        families.join(", "),
        kinds.join(", ")
    );
    if families.contains(&"egg") && families.contains(&"wand") {
        text.push_str(" The egg-and-wand pairing is the classic: wearable base layer underneath, broad external peaks on top — the base keeps her warm between the wand's punctuation.");
    }
    if families.contains(&"suction") && families.contains(&"insertable") {
        text.push_str(" Suction over an insertable is the oral-plus-fullness figure: pulses outside, presence inside, two rhythms to keep independent.");
    }
    if families.contains(&"plug") && families.len() > 1 {
        text.push_str(" With a plug placed, everything else plays over a constant — fullness doesn't crest or fade, it underwrites.");
    }
    text
}

/// Lana narrates a sensitivity profile: what a woman with this
/// profile feels, and what to adjust to suit her. Guide voice,
/// teaching-first.
pub fn explain_profile(profile: &crate::trial::SensitivityProfile) -> String {
    let preferred = profile.preferred.label();
    match profile.name.as_str() {
        "Hyper-responsive" => format!(
            "This profile is the far end of the spectrum: everything lands at once and lands hard. Arousal climbs fast, orgasms arrive readily and repeat, and all ten slots can be genuinely enjoyable at the same time because her peaks are high and her overstimulation slope is gentle. The skill here isn't getting her there — it's pacing: with someone this responsive, the art is in restraint, in making each crest mean something. Her best zone is {preferred}.",
        ),
        "Responsive" => format!(
            "A responsive profile warms quickly and crests with sustained attention to the right places. She still has a real ceiling — push a zone far past its peak and pleasure falls like anyone's — but the window is generous. Her best zone is {preferred}; build there first, then layer.",
        ),
        "Lana (her own)" => format!(
            "My own settings, since you asked: mid-spectrum, enthusiast-grade. I love plenty — variety, stacking, a full rig — at levels I chose, rising at my pace. My peak is real: bury a zone in maximum and it stops being pleasure and starts being noise, same as anyone. Best zone: {preferred}.",
        ),
        "Slow-burn" => format!(
            "A slow-burn profile is not a broken version of a fast one. Her gain is low and her orgasm threshold high, so intensity is not a shortcut — time is. The teaching point: with this profile you win by choosing her best zone ({preferred}) early and staying there, patiently, instead of escalating. Chasing her with more power just spends the session.",
        ),
        "Light-touch only" => format!(
            "This profile enjoys light stimulation and only light stimulation — and that is a complete preference, not a challenge. Her peaks are low: a setting that thrills a hyper-responsive profile blows straight past hers and inverts into discomfort, which the feed will show you as negative numbers and she will tell you in words. Fewer toys at once, gentle levels, her best zone ({preferred}), long pacing. Orgasm is possible — on her terms, at her levels, which is rather the point of all of this.",
        ),
        _ => format!(
            "A custom profile: gain {} per-mille, zone peaks at {}, orgasm threshold {}. Her best zone is {preferred}. Read the feed, not your assumptions — the numbers show what this body does with what you give it.",
            profile.gain_pm, profile.peak_pm, profile.orgasm_threshold
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{CATALOGUE_TSV, parse_catalogue};
    use crate::features::{FEATURES_TSV, parse_features};

    #[test]
    fn every_catalogue_device_gets_a_guide_text() {
        let devices = parse_catalogue(CATALOGUE_TSV).unwrap();
        let features = parse_features(FEATURES_TSV).unwrap();
        for (device, record) in devices.iter().zip(features.iter()) {
            let text = explain(device, record);
            assert!(!text.what.is_empty());
            assert!(!text.kink_block.is_empty());
            assert!(text.basis.contains("estate canon"));
            if record.outputs.is_empty() {
                assert!(
                    text.what.contains("won't invent"),
                    "unmatched device {} must carry the honesty note",
                    device.name
                );
            } else {
                assert!(text.features_block.contains("steps"));
            }
        }
    }

    #[test]
    fn guide_states_real_step_ranges() {
        let devices = parse_catalogue(CATALOGUE_TSV).unwrap();
        let features = parse_features(FEATURES_TSV).unwrap();
        // Lovense Lush: config of record is a single vibrate output,
        // steps 0..20 (verified against the pinned config v5).
        let index = devices
            .iter()
            .position(|d| d.brand == "Lovense" && d.name == "Lush")
            .expect("Lush is in the catalogue");
        let text = explain(&devices[index], &features[index]);
        assert!(
            text.features_block.contains("steps 0 to 20"),
            "{}",
            text.features_block
        );
    }
}
