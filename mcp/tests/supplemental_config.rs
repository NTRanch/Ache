//! Load test for ache's supplemental device configuration
//! (`mcp/device-config/supplemental.json`): the file must parse
//! through the buttplug crate's own user-configuration loader, every
//! supplemental entry must resolve to its named definition through
//! the device configuration manager, and the embedded configuration
//! must be untouched by the layering (same base definitions, plus
//! exactly the supplemental additions).
//!
//! The supplemental entries are config data only: they widen the
//! device catalogue through the generic capability mapping, never
//! the `decide()` gate's authority. Provenance: the IoST device
//! census (config-addition class) — see the implementation record.

use buttplug_core::message::OutputType;
use buttplug_server_device_config::{
    DeviceConfigurationManager, ProtocolCommunicationSpecifier, UserDeviceIdentifier,
    load_protocol_configs,
};

const SUPPLEMENTAL: &str = include_str!("../device-config/supplemental.json");

/// The 29 Vibratissimo models the pinned configuration does not
/// name. Identifiers are the model strings the devices report, per
/// the family's configured entries ("Licker", "Rabbit").
const VIBRATISSIMO_MODELS: [&str; 29] = [
    "Anal Plug",
    "Berlin",
    "Blade",
    "Cancun",
    "Cole",
    "Dubai",
    "DuoBall",
    "G-Vibrator",
    "Hercules",
    "Hollywood",
    "Lee",
    "LieDown",
    "Little Want",
    "London",
    "Mini",
    "Moskau",
    "New York",
    "Obsession",
    "PantyBuster",
    "PantyBuster Deluxe",
    "Paris",
    "Power Want",
    "Rambo",
    "Rom",
    "SingleBall",
    "Souta",
    "Sydney",
    "TouchMe",
    "VibroEgg",
];

fn build(user_config: Option<String>) -> DeviceConfigurationManager {
    load_protocol_configs(&None, &user_config, false)
        .and_then(|mut builder| builder.finish())
        .expect("device configuration must load")
}

fn resolve(dcm: &DeviceConfigurationManager, protocol: &str, identifier: &str) -> Option<String> {
    let ident =
        UserDeviceIdentifier::new("00:00:00:00:00:00", protocol, &Some(identifier.to_owned()));
    dcm.device_definition(&ident).map(|definition| {
        assert!(
            definition
                .features()
                .values()
                .any(|feature| feature.contains_output(OutputType::Vibrate)),
            "{protocol}/{identifier} must expose a vibrate output"
        );
        definition.name().clone()
    })
}

#[test]
fn supplemental_config_parses_and_all_entries_resolve() {
    let dcm = build(Some(SUPPLEMENTAL.to_owned()));
    for model in VIBRATISSIMO_MODELS {
        let name = resolve(&dcm, "vibratissimo", model)
            .unwrap_or_else(|| panic!("vibratissimo/{model} must resolve"));
        assert_eq!(name, format!("Vibratissimo {model}"));
    }
    let name = resolve(&dcm, "svakom-v6", "Echo Neo").expect("svakom-v6/Echo Neo must resolve");
    assert_eq!(name, "Svakom Echo Neo");
}

#[test]
fn supplemental_layering_leaves_embedded_config_intact() {
    let embedded = build(None);
    let layered = build(Some(SUPPLEMENTAL.to_owned()));

    // Exactly the supplemental entries are added — nothing else.
    let added = layered.base_device_definitions().len() - embedded.base_device_definitions().len();
    assert_eq!(added, VIBRATISSIMO_MODELS.len() + 1);

    // Pre-existing entries keep their own definitions (the Rabbit's
    // three vibrate outputs, not a supplemental substitute).
    let rabbit = UserDeviceIdentifier::new(
        "00:00:00:00:00:00",
        "vibratissimo",
        &Some("Rabbit".to_owned()),
    );
    let definition = layered
        .device_definition(&rabbit)
        .expect("embedded Rabbit must still resolve");
    assert_eq!(definition.name(), "Vibratissimo Rabbit");
    assert_eq!(
        definition
            .features()
            .values()
            .filter(|feature| feature.contains_output(OutputType::Vibrate))
            .count(),
        3
    );
    assert!(resolve(&layered, "vibratissimo", "Licker").is_some());
    assert!(resolve(&layered, "svakom-v6", "Echo 2").is_some());

    // An identifier nobody configured falls back to the protocol
    // defaults (the crate's designed behaviour), not to any
    // supplemental entry.
    let fallback = UserDeviceIdentifier::new(
        "00:00:00:00:00:00",
        "vibratissimo",
        &Some("No Such Model".to_owned()),
    );
    assert_eq!(
        layered
            .device_definition(&fallback)
            .expect("protocol defaults must resolve")
            .name(),
        "Vibratissimo Device"
    );
}

#[test]
fn supplemental_discovers_echo_neo_by_name() {
    let dcm = build(Some(SUPPLEMENTAL.to_owned()));
    let specifiers = dcm
        .user_communication_specifiers()
        .get("svakom-v6")
        .expect("supplemental svakom-v6 specifier must exist");
    let names: Vec<&String> = specifiers
        .iter()
        .flat_map(|specifier| match specifier {
            ProtocolCommunicationSpecifier::BluetoothLE(btle) => btle.names().iter().collect(),
            _ => Vec::new(),
        })
        .collect();
    assert!(names.iter().any(|name| name.as_str() == "Echo Neo"));
}
