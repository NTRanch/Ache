//! ache-gui — desktop shell for the ache unified haptics MCP.
//!
//! Phase 1 scope, deliberately narrow:
//! - a searchable browser over the Phase 0 device catalogue
//!   ([`catalogue`]), with each device's IoST-listed link shown and
//!   clickable through the OS opener;
//! - an inert control-preview pane proving the safety rendering: the
//!   master switch is OFF and every control renders inert while it is
//!   off, with the stop control on screen.
//!
//! There is NO device-control wiring in this phase: no provider is
//! constructed and no device path exists. Later phases reach devices
//! only through ache's existing `decide()` gate and tool surface — this
//! shell never grows a private path to a provider.

// `deny`, not `forbid`: the code Slint generates into this crate carries
// its own scoped allowances for the unsafe its vtable glue requires, and
// `forbid` would overrule them. Our own code uses no unsafe anywhere.
#![deny(unsafe_code)]

mod catalogue;

use std::process::{Command, Stdio};
use std::rc::Rc;

use catalogue::{Device, is_sorted, matches_query, parse_catalogue};
use slint::{ModelRc, VecModel};

slint::include_modules!();

/// Converts one catalogue device into its UI row. The URL string is empty
/// exactly when the device has no listed link; `has_url` carries the
/// distinction so the UI never has to guess from the string.
fn to_row(device: &Device) -> DeviceRow {
    DeviceRow {
        brand: device.brand.clone().into(),
        device: device.name.clone().into(),
        device_type: device.device_type.clone().into(),
        bucket: device.bucket.label().into(),
        untested: device.untested,
        link_state: device.link_state.label().into(),
        url: device.url.clone().unwrap_or_default().into(),
        has_url: device.url.is_some(),
    }
}

/// Replaces the list model with the devices matching `query` and refreshes
/// the window's summary line. The full catalogue is 471 rows, so a linear
/// filter per keystroke is bounded and cheap.
fn refill(window: &MainWindow, model: &VecModel<DeviceRow>, devices: &[Device], query: &str) {
    let rows: Vec<DeviceRow> = devices
        .iter()
        .filter(|device| matches_query(device, query))
        .map(to_row)
        .collect();
    let shown = rows.len();
    model.set_vec(rows);
    window
        .set_summary(format!("{} devices in catalogue · showing {}", devices.len(), shown).into());
}

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
                // Best-effort reap: the open either happened or it did not;
                // the child's exit status carries no actionable signal.
                let _ = child.wait();
            });
        }
        Err(err) => eprintln!("ache-gui: could not launch xdg-open: {err}"),
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse_catalogue(catalogue::CATALOGUE_TSV)?;
    // The sortedness guard also runs here, not only in tests: a catalogue
    // regenerated badly must fail loudly at startup, never browse wrong.
    if !is_sorted(&parsed) {
        return Err("embedded catalogue is not in brand/device order".into());
    }
    let devices: Rc<Vec<Device>> = Rc::new(parsed);
    let window = MainWindow::new()?;
    let model = Rc::new(VecModel::<DeviceRow>::default());
    refill(&window, &model, &devices, "");
    window.set_devices(ModelRc::from(model.clone()));

    let search_devices = devices.clone();
    let search_model = model.clone();
    let search_window = window.as_weak();
    window.on_search_changed(move |query| {
        if let Some(window) = search_window.upgrade() {
            refill(&window, &search_model, &search_devices, &query);
        }
    });

    window.on_open_url(|url| open_url(&url));

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
