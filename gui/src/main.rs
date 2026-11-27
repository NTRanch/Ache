//! ache-gui — desktop shell for the ache unified haptics MCP.
//!
//! Scope of this build:
//! - a searchable browser over the Phase 0 device catalogue
//!   ([`catalogue`]), each row with its estate-produced form-factor
//!   art and a detail view with a true 12-frame turntable;
//! - Lana, the estate's toy-canon guide: per-device explanations
//!   built from the devices' real buttplug config records
//!   ([`features`], [`guide`]), and a trial rig ([`trial`]) where
//!   virtual toys act on her simulated state — deterministic,
//!   code-owned numbers, live per-toy telemetry, sensitivity
//!   profiles and per-zone response ([`reaction`] drives her
//!   visible figure through the toon layer's parameter surface);
//! - the Lexicon ([`lexicon`]): the sourced guide book as data.
//!
//! There is NO device-control wiring in this build: no provider is
//! constructed and no device path exists. The trial is a simulation
//! over virtual devices only. Any future real-device path reaches
//! devices only through ache's existing `decide()` gate and tool
//! surface — this shell never grows a private path to a provider.

// `deny`, not `forbid`: the code Slint generates into this crate carries
// its own scoped allowances for the unsafe its vtable glue requires, and
// `forbid` would overrule them. Our own code uses no unsafe anywhere.
#![deny(unsafe_code)]

mod catalogue;
mod features;
mod guide;
mod lexicon;
mod reaction;
mod trial;

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use catalogue::{Device, is_sorted, matches_query, parse_catalogue};
use features::{DeviceFeatures, parse_features};
use lexicon::LexiconEntry;
use reaction::{Band, ClipRegistry};
use slint::{Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use trial::{SensitivityProfile, Trial, TrialToy, Zone};

slint::include_modules!();

/// Mutable application state. Every number the UI shows is owned
/// here or in the trial engine — the UI renders, it does not keep
/// its own copies.
struct AppState {
    devices: Rc<Vec<Device>>,
    features: Vec<DeviceFeatures>,
    entries: Vec<LexiconEntry>,
    lexicon_filtered: Vec<usize>,
    lexicon_selected: Option<usize>,
    selected: Option<usize>,
    trial: Trial,
    clips: ClipRegistry,
}

impl AppState {
    /// The exemplar device index for a family: the first catalogue
    /// device of that family with a matched config, else the first
    /// of the family at all.
    fn exemplar(&self, family: &str) -> Option<usize> {
        self.features
            .iter()
            .position(|f| f.family == family && f.match_kind != features::MatchKind::None)
            .or_else(|| self.features.iter().position(|f| f.family == family))
    }

    /// Adds the exemplar of `family` to the rig at `level_pct` of
    /// each channel's real step range. Returns Lana's refusal line
    /// when a hard limit refuses the add.
    fn add_family(&mut self, family: &str, level_pct: u32) -> Option<&'static str> {
        let index = self.exemplar(family)?;
        let record = &self.features[index];
        let device = &self.devices[index];
        let toy =
            TrialToy::from_features(&device.brand, &device.name, &record.family, &record.outputs);
        match self.trial.add_toy(toy) {
            Ok(()) => {
                let toy_index = self.trial.toys.len() - 1;
                for channel in 0..self.trial.toys[toy_index].channels.len() {
                    let max = self.trial.toys[toy_index].channels[channel].max;
                    self.trial
                        .set_level(toy_index, channel, max * level_pct / 100);
                }
                None
            }
            Err(refusal) => Some(refusal.lana_line()),
        }
    }
}

/// Converts one catalogue device into its UI row.
fn to_row(device: &Device, record: &DeviceFeatures, index: usize) -> DeviceRow {
    DeviceRow {
        brand: device.brand.clone().into(),
        device: device.name.clone().into(),
        device_type: device.device_type.clone().into(),
        bucket: device.bucket.label().into(),
        untested: device.untested,
        link_state: device.link_state.label().into(),
        url: device.url.clone().unwrap_or_default().into(),
        has_url: device.url.is_some(),
        family: record.family.clone().into(),
        index: index as i32,
    }
}

/// Replaces the catalogue model with the devices matching `query`.
fn refill(window: &MainWindow, model: &VecModel<DeviceRow>, state: &AppState, query: &str) {
    let rows: Vec<DeviceRow> = state
        .devices
        .iter()
        .enumerate()
        .filter(|(_, device)| matches_query(device, query))
        .map(|(index, device)| to_row(device, &state.features[index], index))
        .collect();
    let shown = rows.len();
    model.set_vec(rows);
    window.set_summary(
        format!(
            "{} devices in catalogue · showing {}",
            state.devices.len(),
            shown
        )
        .into(),
    );
}

/// Pushes the selected device's guide text into the window.
fn sync_detail(window: &MainWindow, state: &AppState) {
    let Some(index) = state.selected else {
        window.set_has_detail(false);
        return;
    };
    let device = &state.devices[index];
    let record = &state.features[index];
    let text = guide::explain(device, record);
    window.set_has_detail(true);
    window.set_detail_family(record.family.clone().into());
    window.set_detail_title(format!("{} — {}", device.brand, device.name).into());
    window.set_detail_what(text.what.into());
    window.set_detail_features(text.features_block.into());
    window.set_detail_kinks(text.kink_block.into());
    window.set_detail_basis(text.basis.into());
}

/// Rebuilds the trial toy model (telemetry + channels) from the
/// engine. Called on set changes (add/remove/level/profile), not per
/// tick — per-tick values are scalar properties and the trace image.
fn sync_trial_model(window: &MainWindow, state: &AppState) {
    let telemetry = state.trial.telemetry();
    let mut y_offset = 0i32;
    let rows: Vec<TrialToyRow> = state
        .trial
        .toys
        .iter()
        .enumerate()
        .map(|(index, toy)| {
            let card_height = 96 + 30 * toy.channels.len() as i32;
            let this_y = y_offset;
            y_offset += card_height + 8;
            TrialToyRow {
                toy_index: index as i32,
                name: format!("{} — {} ({})", toy.brand, toy.name, toy.family).into(),
                family: toy.family.clone().into(),
                pleasure: (telemetry[index].pleasure / 10).min(100),
                force: ((telemetry[index].force / 10) as i32).min(100),
                channel_count: toy.channels.len() as i32,
                y_offset: this_y,
                card_height,
                channels: ModelRc::from(Rc::new(VecModel::from(
                    toy.channels
                        .iter()
                        .map(|channel| ChannelRow {
                            label: format!(
                                "{}{}",
                                channel.kind.token(),
                                if channel.max == 0 {
                                    String::new()
                                } else {
                                    format!(" · 0–{}", channel.max)
                                }
                            )
                            .into(),
                            level: channel.level as i32,
                            max: channel.max as i32,
                        })
                        .collect::<Vec<_>>(),
                ))),
                spark: Image::default(),
            }
        })
        .collect();
    window.set_rig_height(y_offset);
    window.set_trial_toys(ModelRc::from(Rc::new(VecModel::from(rows))));
    window.set_trial_count(
        format!(
            "{} / {} toys",
            state.trial.toys.len(),
            trial::MAX_TOTAL_TOYS
        )
        .into(),
    );
    window.set_combo_text(guide::explain_combination(&state.trial.toys).into());
}

/// Pushes Lana's live state (band, readouts, reaction parameters)
/// into the window. Runs per trial tick.
fn sync_lana(window: &MainWindow, state: &AppState) {
    let frame = reaction::reaction_frame(&state.trial);
    let strip = match frame.band {
        Band::Composed | Band::LightPleasure => 0,
        Band::Building => 1,
        Band::NearOrgasm => 2,
        Band::Orgasm | Band::SquirtEvent | Band::Afterglow => 3,
        Band::Overstimulation => 4,
    };
    window.set_lana_strip_idx(strip);
    window.set_lana_frame((state.trial.tick_count % 12) as i32);
    let body = frame.body;
    let x = body.tremble_wave as f32 * body.tremble_amp as f32 / 1000.0 * 0.004
        - body.recoil as f32 * 0.008;
    let y = body.breath_wave as f32 * body.breath_depth as f32 / 1000.0 * 0.004
        - body.settle as f32 * 0.004
        - body.arch as f32 * 0.002;
    window.set_lana_x(x);
    window.set_lana_y(y);
    window.set_lana_band(frame.band.label().into());
    window.set_lana_stage(state.trial.stage().label().into());
    window.set_lana_arousal((state.trial.arousal / 10) as i32);
    window.set_lana_reaction(state.trial.reaction_line().into());
    window.set_lana_events(
        format!(
            "Orgasms: {} · Squirts: {} · Profile: {}",
            state.trial.orgasms, state.trial.squirts, state.trial.profile.name
        )
        .into(),
    );
    window.set_lana_params(
        format!(
            "Reaction surface — pupil shape {}‰ · blush {}‰ · mouth open {}‰ · breath {} /min · tremble {}‰ · arch {}‰ · recoil {}‰ · settle {}‰",
            frame.expr.pupil_shape,
            frame.expr.blush_intensity,
            frame.expr.mouth_open,
            frame.body.breath_rate / 10,
            frame.body.tremble_amp,
            frame.body.arch,
            frame.body.recoil,
            frame.body.settle
        )
        .into(),
    );
    let clip_status = match state.clips.lookup(&frame) {
        Some(key) => format!("Playing reaction clip: {key}"),
        None => format!(
            "Procedural placeholder reaction — clip {} not registered yet (animation lane).",
            frame.clip_key
        ),
    };
    window.set_lana_clip(clip_status.into());
    // Zone drives and total pleasure are per-mille in the engine; the
    // meters display percent.
    let drives = state.trial.zone_drives();
    let pct = |pm: u32| ((pm / 10) as i32).min(100);
    window.set_zone_ext(pct(drives[Zone::External as usize]));
    window.set_zone_int(pct(drives[Zone::Internal as usize]));
    window.set_zone_anal(pct(drives[Zone::Anal as usize]));
    window.set_zone_nip(pct(drives[Zone::Nipples as usize]));
    window.set_total_pleasure((state.trial.total_pleasure() / 10).clamp(0, 100));
    window.set_trace_image(trace_image(state));
}

/// The live trace: one pleasure polyline per toy plus her arousal,
/// drawn from the engine's tick history into an RGBA buffer.
fn trace_image(state: &AppState) -> Image {
    const W: usize = 600;
    const H: usize = 96;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(W as u32, H as u32);
    let pixels = buffer.make_mut_slice();
    for pixel in pixels.iter_mut() {
        *pixel = Rgba8Pixel {
            r: 30,
            g: 25,
            b: 48,
            a: 255,
        };
    }
    let palette: [(u8, u8, u8); 10] = [
        (236, 72, 153),
        (167, 139, 250),
        (249, 168, 212),
        (196, 181, 253),
        (244, 114, 182),
        (139, 92, 246),
        (251, 207, 232),
        (124, 58, 237),
        (219, 39, 119),
        (168, 85, 247),
    ];
    let history = &state.trial.history;
    let plot = |pixels: &mut [Rgba8Pixel], x: usize, y: i32, color: (u8, u8, u8)| {
        if x < W && y >= 0 && (y as usize) < H {
            pixels[y as usize * W + x] = Rgba8Pixel {
                r: color.0,
                g: color.1,
                b: color.2,
                a: 255,
            };
        }
    };
    let y_of = |value: i32, min: i32, span: i32| -> i32 {
        (H as i32 - 2) - (value.clamp(min, min + span) - min) * (H as i32 - 4) / span
    };
    for (tick_index, record) in history.iter().enumerate() {
        let x = tick_index * (W / trial::HISTORY_TICKS);
        for (toy_index, (pleasure, _force)) in record.per_toy.iter().enumerate() {
            let color = palette[toy_index % palette.len()];
            let y = y_of(*pleasure, -300, 1300);
            for dy in -1..=1 {
                plot(pixels, x, y + dy, color);
            }
        }
        let y = y_of(record.arousal as i32, 0, 1000);
        plot(pixels, x, y, (245, 242, 250));
    }
    Image::from_rgba8(buffer)
}

/// Pushes the profile panel (name, Lana's narration, zone sliders).
fn sync_profile(window: &MainWindow, state: &AppState) {
    window.set_profile_name(state.trial.profile.name.clone().into());
    window.set_profile_text(guide::explain_profile(&state.trial.profile).into());
    let zones: Vec<f32> = state
        .trial
        .profile
        .zone_pm
        .iter()
        .map(|value| *value as f32 / 10.0)
        .collect();
    window.set_zone_values(ModelRc::from(Rc::new(VecModel::from(zones))));
}

/// Pushes the lexicon list model from the current filter.
fn sync_lexicon_list(window: &MainWindow, state: &AppState) {
    let rows: Vec<LexiconRow> = state
        .lexicon_filtered
        .iter()
        .map(|index| {
            let entry = &state.entries[*index];
            LexiconRow {
                title: entry.title.clone().into(),
                meta: format!("{} · {}", entry.section, entry.source.label()).into(),
            }
        })
        .collect();
    window.set_lexicon_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// Pushes the selected lexicon entry into the reading pane.
fn sync_lexicon_entry(window: &MainWindow, state: &AppState) {
    let Some(index) = state.lexicon_selected else {
        return;
    };
    let entry = &state.entries[index];
    window.set_lexicon_title(entry.title.clone().into());
    window.set_lexicon_meta(
        format!(
            "{} · key: {} · source: {}",
            entry.section,
            entry.key,
            entry.source.label()
        )
        .into(),
    );
    window.set_lexicon_body(entry.body.clone().into());
    window.set_lexicon_can_try(matches!(
        entry.section.as_str(),
        "combo" | "lesson" | "family"
    ));
}

/// The scenario a lexicon entry preloads: combo keys map to their
/// rig; lesson/family entries preload their own family.
fn scenario_families(entry: &LexiconEntry) -> Vec<(&'static str, u32)> {
    match (entry.section.as_str(), entry.key.as_str()) {
        ("combo", "egg-and-wand") => vec![("egg", 45), ("wand", 40)],
        ("combo", "suction-and-insertable") => vec![("suction", 45), ("insertable", 35)],
        ("combo", "the-ii-stack") => vec![("plug", 40), ("egg", 40), ("wand", 40)],
        ("combo", "rider-throne") => vec![("rideon", 45), ("egg", 40)],
        ("combo", "split-attention") => vec![("nipple", 25), ("suction", 40)],
        ("combo", "training-and-tease") => vec![("kegel", 40), ("egg", 35)],
        ("combo", "the-conducted-rig") => vec![
            ("egg", 40),
            ("plug", 40),
            ("suction", 45),
            ("wand", 35),
            ("bullet", 40),
        ],
        ("combo", "the-light-touch-rig") => vec![("bullet", 15), ("external", 20), ("panty", 20)],
        ("lesson", family) | ("family", family) => {
            // Family ids are data, not statics; leak-free mapping via
            // the known family set.
            FAMILIES
                .iter()
                .filter(|known| **known == family)
                .map(|known| (*known, 35))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The family ids the art/trial pipelines know.
const FAMILIES: &[&str] = &[
    "egg",
    "insertable",
    "rabbit",
    "plug",
    "panty",
    "suction",
    "butterfly",
    "kegel",
    "bullet",
    "wand",
    "clip",
    "external",
    "rideon",
    "nipple",
    "beads",
    "controller",
    "other",
];

/// Opens `url` with the OS opener (`xdg-open`, separate argv, no shell).
///
/// Only http(s) URLs are accepted — the catalogue parser already enforces
/// the scheme, and this is the second, independent check at the OS
/// boundary. Spawning is best-effort: a failure is reported on stderr, and
/// a reaper thread owns the child handle so it cannot linger as a zombie.
fn open_url(url: &str) {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        eprintln!("ache-gui: refusing to open non-http(s) URL: {url}");
        return;
    }
    match Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => eprintln!("ache-gui: could not launch xdg-open: {err}"),
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse_catalogue(catalogue::CATALOGUE_TSV)?;
    if !is_sorted(&parsed) {
        return Err("embedded catalogue is not in brand/device order".into());
    }
    let devices: Rc<Vec<Device>> = Rc::new(parsed);
    let features = parse_features(features::FEATURES_TSV)?;
    if features.len() != devices.len() {
        return Err("features table does not align with the catalogue".into());
    }
    let entries = lexicon::parse_lexicon(lexicon::LEXICON_TSV)?;
    {
        let families: Vec<String> = features.iter().map(|f| f.family.clone()).collect();
        let gaps = lexicon::coverage_gaps(&entries, &families);
        if !gaps.is_empty() {
            return Err(format!("lexicon coverage gaps: {}", gaps.join("; ")).into());
        }
    }
    let state = Rc::new(RefCell::new(AppState {
        devices: devices.clone(),
        features,
        lexicon_filtered: (0..entries.len()).collect(),
        lexicon_selected: None,
        entries,
        selected: None,
        trial: Trial::new(),
        clips: ClipRegistry::new(),
    }));

    let window = MainWindow::new()?;
    let catalogue_model = Rc::new(VecModel::<DeviceRow>::default());
    refill(&window, &catalogue_model, &state.borrow(), "");
    window.set_devices(ModelRc::from(catalogue_model.clone()));
    sync_lexicon_list(&window, &state.borrow());
    sync_profile(&window, &state.borrow());
    sync_trial_model(&window, &state.borrow());
    sync_lana(&window, &state.borrow());
    if !state.borrow().entries.is_empty() {
        state.borrow_mut().lexicon_selected = Some(0);
        sync_lexicon_entry(&window, &state.borrow());
    }

    // --- catalogue callbacks ---
    {
        let state = state.clone();
        let model = catalogue_model.clone();
        let weak = window.as_weak();
        window.on_search_changed(move |query| {
            if let Some(window) = weak.upgrade() {
                refill(&window, &model, &state.borrow(), &query);
            }
        });
    }
    window.on_open_url(|url| open_url(&url));
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_device_selected(move |index| {
            if index < 0 {
                return;
            }
            state.borrow_mut().selected = Some(index as usize);
            if let Some(window) = weak.upgrade() {
                sync_detail(&window, &state.borrow());
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_add_selected_to_trial(move || {
            let mut note = String::new();
            {
                let mut state = state.borrow_mut();
                if let Some(index) = state.selected {
                    let record = &state.features[index];
                    let device = &state.devices[index];
                    let toy = TrialToy::from_features(
                        &device.brand,
                        &device.name,
                        &record.family,
                        &record.outputs,
                    );
                    if let Err(refusal) = state.trial.add_toy(toy) {
                        note = refusal.lana_line().to_string();
                    }
                }
            }
            if let Some(window) = weak.upgrade() {
                window.set_trial_note(note.into());
                let state = state.borrow();
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }

    // --- trial callbacks ---
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_trial_remove(move |index| {
            state.borrow_mut().trial.remove_toy(index.max(0) as usize);
            if let Some(window) = weak.upgrade() {
                let state = state.borrow();
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_trial_clear(move || {
            state.borrow_mut().trial.clear_toys();
            if let Some(window) = weak.upgrade() {
                window.set_trial_note("".into());
                let state = state.borrow();
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_channel_released(move |toy, channel, value| {
            state.borrow_mut().trial.set_level(
                toy.max(0) as usize,
                channel.max(0) as usize,
                value.max(0.0) as u32,
            );
            if let Some(window) = weak.upgrade() {
                let state = state.borrow();
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_profile_selected(move |index| {
            let presets = SensitivityProfile::presets();
            if let Some(profile) = presets.get(index.max(0) as usize) {
                state.borrow_mut().trial.set_profile(profile.clone());
            }
            if let Some(window) = weak.upgrade() {
                let state = state.borrow();
                sync_profile(&window, &state);
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_zone_changed(move |zone, value| {
            {
                let mut state = state.borrow_mut();
                let zone = (zone.max(0) as usize).min(3);
                state.trial.profile.zone_pm[zone] = (value.clamp(20.0, 200.0) * 10.0) as u32;
                if !state.trial.profile.name.starts_with("Custom") {
                    state.trial.profile.name =
                        format!("Custom (from {})", state.trial.profile.name);
                }
            }
            if let Some(window) = weak.upgrade() {
                let state = state.borrow();
                sync_profile(&window, &state);
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }

    // --- lexicon callbacks ---
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_lexicon_search(move |query| {
            {
                let mut state = state.borrow_mut();
                state.lexicon_filtered = lexicon::search(&state.entries, &query);
            }
            if let Some(window) = weak.upgrade() {
                sync_lexicon_list(&window, &state.borrow());
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_lexicon_select(move |row| {
            {
                let mut state = state.borrow_mut();
                if let Some(entry_index) = state.lexicon_filtered.get(row.max(0) as usize) {
                    state.lexicon_selected = Some(*entry_index);
                }
            }
            if let Some(window) = weak.upgrade() {
                sync_lexicon_entry(&window, &state.borrow());
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_lexicon_try(move || {
            let mut note = String::new();
            {
                let mut state = state.borrow_mut();
                if let Some(index) = state.lexicon_selected {
                    let families = scenario_families(&state.entries[index].clone());
                    state.trial.clear_toys();
                    for (family, level) in families {
                        if let Some(refusal) = state.add_family(family, level) {
                            note = refusal.to_string();
                            break;
                        }
                    }
                }
            }
            if let Some(window) = weak.upgrade() {
                window.set_trial_note(note.into());
                let state = state.borrow();
                sync_trial_model(&window, &state);
                sync_lana(&window, &state);
            }
        });
    }

    // --- timers: turntable spin + the trial engine tick ---
    let spin_timer = slint::Timer::default();
    {
        let weak = window.as_weak();
        let mut frame = 0i32;
        spin_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(120),
            move || {
                frame = (frame + 1) % 12;
                if let Some(window) = weak.upgrade() {
                    window.set_spin_frame(frame);
                }
            },
        );
    }
    let tick_timer = slint::Timer::default();
    {
        let state = state.clone();
        let weak = window.as_weak();
        tick_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                state.borrow_mut().trial.tick();
                if let Some(window) = weak.upgrade() {
                    sync_lana(&window, &state.borrow());
                }
            },
        );
    }

    let about = AboutDialog::new()?;
    window.on_show_about(move || {
        if let Err(err) = about.show() {
            eprintln!("ache-gui: could not show About dialog: {err}");
        }
    });

    window.run()?;
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("ache-gui: {err}");
        std::process::exit(1);
    }
}
