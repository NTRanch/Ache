//! §4.1 deterministic replay (mandatory, per adopted format) and the
//! seed-library gate. Every fixture is compiled once and replayed three
//! times against the recording sink with the gate on every command; the
//! stream hashes must be byte-identical. Hashes, line counts, and clamp
//! counts print as `REPLAY …` lines — the run artefact (D-713); run with
//! `--nocapture` to collect them.

use serde_json::{Value, json};

use crate::automation::Event;
use crate::decide::{AllowedOutput, AxisLimit};
use crate::funscript::{import, import_pack};
use crate::pattern::Function;
use crate::provider::{Capability, Device};
use crate::session::tests::{armed, devices, library, replay};
use crate::session::{
    CompiledSession, Gated, Library, SESSION_FORMAT, SessionRun, gate_proposed, seed_library,
};

const OPEN: [AxisLimit; 6] = [AxisLimit {
    min: 0,
    max: 100,
    max_step: 100,
}; 6];
const KOISYNC_DEFAULT: [AxisLimit; 6] = [AxisLimit {
    min: 10,
    max: 90,
    max_step: 4,
}; 6];
const RUNS: usize = 3;

fn thrice(
    name: &str,
    s: &CompiledSession,
    ceiling: u8,
    events: &[(u64, Event)],
    limits: [AxisLimit; 6],
) -> String {
    let reports: Vec<_> = (0..RUNS)
        .map(|_| replay(s, ceiling, events, limits))
        .collect();
    for (i, r) in reports.iter().enumerate() {
        println!(
            "REPLAY format={name} run={} hash={} lines={} clamps={} stop={}",
            i + 1,
            r.hash,
            r.lines.len(),
            r.clamps,
            r.stop_reason
        );
    }
    assert!(
        !reports[0].lines.is_empty(),
        "{name}: a replay that emits nothing proves nothing"
    );
    for r in &reports[1..] {
        assert_eq!(
            r.hash, reports[0].hash,
            "{name}: replay must be byte-identical"
        );
        assert_eq!(r.lines, reports[0].lines);
    }
    reports[0].hash.clone()
}

fn session(id: &str, duration: u64, bindings: Value) -> Value {
    json!({"format": SESSION_FORMAT, "id": id, "duration_ms": duration, "bindings": bindings})
}

#[test]
fn replay_basic_pattern() {
    let p = json!({"format": "ache.pattern/v1", "id": "basic", "function": "Generic",
        "sections": [{"blocks": [
            {"kind": "square", "top": 40, "duration_ms": 700},
            {"kind": "linear", "bottom": 5, "top": 95, "duration_ms": 1500, "speed": 1.5},
            {"kind": "sine", "bottom": 10, "top": 80, "duration_ms": 900, "speed": 3.0, "loops": 2}
        ]}], "loop": {"repeat": 2}});
    let s = CompiledSession::compile(&session("b", 10_000, json!([
        {"id": "v", "device": "buttplug:1", "function": "Vibrate", "source": {"pattern": "basic"}},
        {"id": "t", "device": "tcode:0", "function": "Twist", "source": {"pattern": "basic"}}
    ])), &library(&[p], None), &devices()).unwrap();
    thrice("basic", &s, 20, &[], OPEN);
}

#[test]
fn replay_scripted_pattern() {
    let p = json!({"format": "ache.pattern/v1", "id": "scripted", "function": "Generic",
        "scripted": {"base": 10, "expr": "clamp(intensity + 7 * sin(time * 3) + index * 5, 0, 100)", "duration_ms": 4000},
        "loop": {"repeat": 2}});
    let s = CompiledSession::compile(&session("s", 10_000, json!([
        {"id": "a", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "scripted"}},
        {"id": "b", "device": "buttplug:1", "function": "Rotate", "source": {"pattern": "scripted"}},
        {"id": "c", "device": "tcode:0", "function": "Pitch", "source": {"pattern": "scripted"}}
    ])), &library(&[p], None), &devices()).unwrap();
    thrice("scripted", &s, 20, &[], OPEN);
}

#[test]
fn replay_funscript_import() {
    let actions: Vec<Value> = (0..120)
        .map(|k| json!({"at": k * 37, "pos": (k * 53) % 101}))
        .collect();
    let p = import(
        "fs",
        Function::StrokePosition,
        &json!({"version": "1.0", "actions": actions}),
    )
    .unwrap();
    let mut lib = Library::default();
    lib.patterns.insert("fs".into(), p);
    let s = CompiledSession::compile(&session("f", 10_000, json!([
        {"id": "l0", "device": "tcode:0", "function": "StrokePosition", "source": {"pattern": "fs"}}
    ])), &lib, &devices()).unwrap();
    thrice("funscript", &s, 20, &[], KOISYNC_DEFAULT);
}

#[test]
fn replay_multi_axis_pack() {
    let channel = |phase: i64| {
        let actions: Vec<Value> = (0..60)
            .map(|k| json!({"at": k * 50, "pos": ((k + phase) * 17) % 101}))
            .collect();
        json!({"actions": actions})
    };
    let pack = import_pack(&json!({"format": "ache.pack/v1", "id": "six", "channels": {
        "stroke": channel(0), "surge": channel(1), "sway": channel(2),
        "twist": channel(3), "roll": channel(4), "pitch": channel(5)}}))
    .unwrap();
    let mut lib = Library::default();
    lib.packs.insert("six".into(), pack);
    let s = CompiledSession::compile(
        &session(
            "p",
            5_000,
            json!([
                {"id": "sr6", "device": "tcode:0", "source": {"pack": "six"}}
            ]),
        ),
        &lib,
        &devices(),
    )
    .unwrap();
    thrice("pack", &s, 20, &[], KOISYNC_DEFAULT);
}

#[test]
fn replay_automation_with_jobs_queues_seed_and_event_log() {
    let reward = json!({"format": "ache.pattern/v1", "id": "reward", "function": "Generic",
        "sections": [{"blocks": [{"kind": "sine", "bottom": 20, "top": 90, "duration_ms": 1000, "loops": 2}]}], "loop": "once"});
    let automation = json!({"format": "ache.automation/v1", "id": "auto",
        "variables": [{"id": "lvl", "type": "number", "init": 10, "min": 0, "max": 70},
                      {"id": "dial", "type": "number", "init": 0, "min": 0, "max": 100}],
        "controls": [{"id": "stop", "kind": "stop_button"}, {"id": "click", "kind": "button"},
                     {"id": "knob", "kind": "slider", "var": "dial"}],
        "triggers": [{"id": "go", "on": "session_start"}, {"id": "c", "on": {"control_pressed": "click"}},
                     {"id": "e", "on": {"external_event": "cue"}}, {"id": "f", "on": {"pattern_finished": "reward"}}],
        "jobs": [
            {"id": "wander", "trigger": "go", "repeat": {"every": {"ms": 500}}, "max_runs": 40, "max_span_ms": 12000,
             "actions": [{"set_variable": {"var": "lvl", "expr": "20 + hash01(time * 2) * 50"}},
                         {"set_function": {"target": "main", "value": "lvl"}}]},
            {"id": "click", "trigger": "c", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
             "actions": [{"play_pattern": {"pattern": "reward", "target": "main"}}]},
            {"id": "cue", "trigger": "e", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
             "actions": [{"start_queue": "steps"}]},
            {"id": "after", "trigger": "f", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
             "actions": [{"notify": "reward finished"}]}
        ],
        "queues": [{"id": "steps", "loop": {"repeat": 2}, "actions": [
            {"set_function": {"target": "axis", "value": "30"}}, {"delay": {"ms": 750}},
            {"set_function": {"target": "axis", "value": "70"}}, {"delay": {"ms": 750}}]}]});
    let lib = library(&[reward], Some(automation));
    let mut spec = session(
        "a",
        12_000,
        json!([
            {"id": "main", "device": "buttplug:1", "function": "Vibrate", "source": "automation"},
            {"id": "axis", "device": "tcode:0", "function": "Surge", "source": "automation"},
            {"id": "manual", "device": "buttplug:1", "function": "Rotate", "source": {"manual": "knob"}}
        ]),
    );
    spec["automation"] = json!("auto");
    spec["seed"] = json!(20_261_126u64);
    spec["inputs"] = json!([{"id": "cue"}]);
    let s = CompiledSession::compile(&spec, &lib, &devices()).unwrap();
    // The event log is part of the fixture (§4.1).
    let log = vec![
        (1_000, s.event("press", "click", None).unwrap()),
        (2_500, s.event("slider", "knob", Some(45.0)).unwrap()),
        (4_000, s.event("external", "cue", None).unwrap()),
        (6_000, s.event("slider", "knob", Some(200.0)).unwrap()),
    ];
    let hash = thrice("automation", &s, 20, &log, KOISYNC_DEFAULT);
    // A different seed is a different stream: the seed is real input.
    let mut other = spec.clone();
    other["seed"] = json!(7u64);
    let s2 = CompiledSession::compile(&other, &lib, &devices()).unwrap();
    assert_ne!(replay(&s2, 20, &log, KOISYNC_DEFAULT).hash, hash);
    let r = replay(&s, 20, &log, KOISYNC_DEFAULT);
    assert!(
        r.lines.iter().any(|l| l.ends_with("Rotate:20")),
        "slider 200 clamps to 100 % = 20"
    );
    assert!(
        r.lines.iter().any(|l| l.contains("tcode:0 L1")),
        "the queue drives the surge axis"
    );
}

// --- adversarial at session level (A1, A3) ----------------------------

#[test]
fn a1_mutual_restart_ends_at_the_session_bound() {
    let automation = json!({"format": "ache.automation/v1", "id": "loop",
        "controls": [{"id": "stop", "kind": "stop_button"}],
        "triggers": [{"id": "go", "on": "session_start"}],
        "jobs": [{"id": "j", "trigger": "go", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
            "actions": [{"start_queue": "q"}, {"set_function": {"target": "m", "value": "time * 10"}}]}],
        "queues": [{"id": "q", "loop": "once", "actions": [{"start_job": "j"}]}]});
    let lib = library(&[], Some(automation));
    let mut spec = session(
        "l",
        3_000,
        json!([{"id": "m", "device": "buttplug:0", "function": "Vibrate", "source": "automation"}]),
    );
    spec["automation"] = json!("loop");
    let s = CompiledSession::compile(&spec, &lib, &devices()).unwrap();
    let r = replay(&s, 20, &[], OPEN);
    assert_eq!(r.stop_reason, "session_duration");
    let last_t: u64 = r
        .lines
        .last()
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(last_t < 3_000, "nothing is emitted at or past the bound");
    println!(
        "REPLAY a1_mutual_restart lines={} last_t={last_t} stop={}",
        r.lines.len(),
        r.stop_reason
    );
}

#[test]
fn a3_every_emitted_command_is_at_or_under_its_ceiling_including_slow_drift() {
    let hot = json!({"format": "ache.pattern/v1", "id": "hot", "function": "Generic",
        "scripted": {"base": 0, "expr": "intensity + 50", "duration_ms": 3000}, "loop": "once"});
    let automation = json!({"format": "ache.automation/v1", "id": "drift",
        "variables": [{"id": "x", "type": "number", "init": 0, "min": 0, "max": 100000}],
        "controls": [{"id": "stop", "kind": "stop_button"}],
        "triggers": [{"id": "go", "on": "session_start"}],
        "jobs": [{"id": "creep", "trigger": "go", "repeat": {"every": {"ms": 250}}, "max_runs": 40, "max_span_ms": 10000,
            "actions": [{"add_variable": {"var": "x", "expr": "3"}}, {"set_function": {"target": "m", "value": "x"}}]}]});
    let lib = library(&[hot], Some(automation));
    let mut spec = session(
        "c",
        10_000,
        json!([
            {"id": "m", "device": "buttplug:0", "function": "Vibrate", "source": "automation"},
            {"id": "p", "device": "buttplug:1", "function": "Rotate", "source": {"pattern": "hot"}},
            {"id": "ax", "device": "tcode:0", "function": "StrokePosition", "source": {"pattern": "hot"}}
        ]),
    );
    spec["automation"] = json!("drift");
    let s = CompiledSession::compile(&spec, &lib, &devices()).unwrap();
    let ceiling = 6;
    let r = replay(&s, ceiling, &[], KOISYNC_DEFAULT);
    let mut previous: Option<u32> = None;
    for line in &r.lines {
        let wire = line.rsplit(' ').next().unwrap();
        if let Some((_, strength)) = wire.split_once(':') {
            assert!(
                strength.parse::<i64>().unwrap() <= i64::from(ceiling),
                "{line}"
            );
        } else {
            let digits: u32 = wire[2..6].parse().unwrap();
            assert!((1000..=9000).contains(&digits), "window [10, 90]: {line}");
            if let Some(p) = previous {
                assert!(digits.abs_diff(p) <= 400, "slew 4 per frame: {line}");
            }
            previous = Some(digits);
        }
    }
    assert!(r.clamps > 0);
    println!(
        "REPLAY a3_ceiling lines={} clamps={} ceiling={ceiling}",
        r.lines.len(),
        r.clamps
    );
}

// --- the seed library -------------------------------------------------

fn suction_device() -> Device {
    let mut d = devices().remove(1);
    d.id = "buttplug:7".into();
    d.capabilities.push(Capability {
        feature: "Suction".into(),
        max: Some(20),
        native: "Suction".into(),
        steps: Some(20),
    });
    d
}

#[test]
fn seed_library_loads_whole_and_every_seed_replays_deterministically() {
    let lib = seed_library().unwrap();
    assert_eq!(lib.patterns.len(), 7);
    assert_eq!(lib.automations.len(), 7);
    let mut devs = devices();
    devs.push(suction_device());
    // Each automation seed bound in a session with every external
    // source it names registered, driven by an event log.
    let externals = [
        "say_stop",
        "treat",
        "reward",
        "warning",
        "punishment",
        "edge_near",
        "movement",
        "hr_high",
        "hr_ok",
    ];
    for id in lib.automations.keys() {
        let mut spec = session(
            &format!("seed.{id}"),
            600_000,
            json!([
                {"id": "main", "device": "buttplug:1", "function": "Vibrate", "source": "automation"}
            ]),
        );
        spec["automation"] = json!(id);
        spec["inputs"] = json!(
            externals
                .iter()
                .map(|e| json!({"id": e}))
                .collect::<Vec<_>>()
        );
        let s =
            CompiledSession::compile(&spec, &lib, &devs).unwrap_or_else(|e| panic!("{id}: {e}"));
        let mut log = Vec::new();
        for (k, name) in externals.iter().enumerate() {
            log.push((
                5_000 + 7_000 * k as u64,
                s.event("external", name, None).unwrap(),
            ));
        }
        if id == "hyp-clicker" {
            log.push((2_000, s.event("press", "click", None).unwrap()));
        }
        if id == "say-stop" {
            // The stop primitive drives nothing by design: its proof is
            // the program's own record, identical across runs.
            let runs: Vec<_> = (0..RUNS).map(|_| replay(&s, 20, &log, OPEN)).collect();
            assert!(runs[0].lines.is_empty());
            assert!(
                runs[0].log.iter().any(|l| l.contains("stop word heard")),
                "{:?}",
                runs[0].log
            );
            assert!(
                runs.iter()
                    .all(|r| r.log == runs[0].log && r.hash == runs[0].hash)
            );
            println!(
                "REPLAY format=seed:say-stop runs={RUNS} log={:?}",
                runs[0].log
            );
            continue;
        }
        thrice(&format!("seed:{id}"), &s, 20, &log, OPEN);
    }
    // The dual-function for-her pair binds together and peaks together.
    let s = CompiledSession::compile(&session("seed.forher", 60_000, json!([
        {"id": "v", "device": "buttplug:7", "function": "Vibrate", "source": {"pattern": "forher-vibe-peak"}},
        {"id": "a", "device": "buttplug:7", "function": "AirPulse", "source": {"pattern": "forher-air-peak"}}
    ])), &lib, &devs).unwrap();
    thrice("seed:forher-pair", &s, 20, &[], OPEN);
    for id in ["alma-draining-loop", "alma-nipple-cycle"] {
        let s = CompiledSession::compile(&session(&format!("seed.{id}"), 600_000, json!([
            {"id": "m", "device": "buttplug:7", "function": "Suction", "source": {"pattern": id}}
        ])), &lib, &devs).unwrap();
        thrice(&format!("seed:{id}"), &s, 20, &[], OPEN);
    }
}

#[test]
fn the_denial_seed_denies_at_the_meter_peak() {
    let lib = seed_library().unwrap();
    // The seed's jobs span 10 min; §2.4 requires the session to cover it.
    let mut spec = session(
        "deny",
        600_000,
        json!([
            {"id": "main", "device": "buttplug:1", "function": "Vibrate", "source": "automation"}
        ]),
    );
    spec["automation"] = json!("denial-arousal-meter");
    let s = CompiledSession::compile(&spec, &lib, &devices()).unwrap();
    let mut run = SessionRun::new(&s);
    let mut strengths = Vec::new();
    while let Some(out) = run.step(&[]) {
        for p in out {
            if let Gated::Output {
                plan: AllowedOutput::Function { strength, .. },
                ..
            } = gate_proposed(&s, &p, &armed(), None).unwrap()
            {
                strengths.push(strength);
            }
        }
    }
    // It climbs, drops to zero at the peak (denial), and climbs again.
    let peak = strengths
        .iter()
        .position(|v| *v == 0)
        .expect("a denial happens");
    assert!(strengths[..peak].windows(2).all(|w| w[1] >= w[0]));
    assert!(
        strengths[peak + 1..].iter().any(|v| *v > 0),
        "the meter resumes"
    );
    assert!(
        run.log()
            .iter()
            .any(|l| l.contains("denied at the meter's peak"))
    );
}
