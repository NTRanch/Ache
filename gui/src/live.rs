//! The live renderer link — Option C (out-of-process renderer)
//! from the live-render3d scope
//! (`~/workspace/ache-live-render3d-scope-20261127/SCOPE.md` §1).
//!
//! When the operator sets `ACHE_LANA_RENDERER` (path to the
//! torogao-side `render3d-live` binary) together with
//! `ACHE_LANA_GLB` and `ACHE_LANA_CLIPS`, the GUI spawns the
//! renderer as a child process and shows its frames in the room's
//! figure cluster in place of the animation strip. Without the
//! variables the room is exactly the strip build — this module is
//! inert and nothing is spawned.
//!
//! Protocol (the scope's §1/§2 contract): one JSON handshake line
//! from the child, then JSON-line drive states in
//! (`{"seq":N,"clip":"lana/react/<band>.t<tier>","expr":{…}}`,
//! per-mille expression values straight from
//! [`crate::reaction::ReactionFrame`]) and length-prefixed binary
//! RGBA frames out, each stamped with the sequence of the state it
//! answers. The GUI keeps only the newest frame.
//!
//! Safety posture: the renderer is a display server. It contains
//! no ache code, no device code, and no network code; this link
//! sends it presentation state only. Nothing here constructs a
//! provider or touches `decide()` — the GUI stays simulation-only.
//! STOP: [`LiveRenderer::send_stop`] raises a sequence floor
//! before the stop state is written, so any frame rendered from a
//! pre-stop state is discarded on arrival, never displayed.
//! If the child dies, `alive` falls, the stage falls back to the
//! strip figure, and the room keeps running.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::reaction::Expr13;

/// The spike's fixed frame size (scope §4): twice the strip's
/// 240×480 display box, so the stage downsamples, never upsamples.
const FRAME_W: u32 = 480;
const FRAME_H: u32 = 960;
/// Frame record header: payload length is carried by the outer
/// u32le prefix; the record itself is seq u64, state_seq u64,
/// width u32, height u32, then RGBA bytes.
const HEADER_LEN: usize = 24;

struct Frame {
    state_seq: u64,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    latest: Mutex<Option<Frame>>,
    alive: AtomicBool,
    /// Frames with a smaller state_seq are pre-stop work and are
    /// discarded by the pump, never shown.
    min_state_seq: AtomicU64,
    received: AtomicU64,
}

/// The GUI's handle to the renderer child. Dropping it kills the
/// child — the renderer never outlives the room.
pub struct LiveRenderer {
    child: Child,
    stdin: Mutex<ChildStdin>,
    seq: AtomicU64,
    shared: Arc<Shared>,
}

impl LiveRenderer {
    /// Spawns the renderer when the environment asks for it.
    /// Returns `None` (strip figure, unchanged room) when the
    /// variables are unset or anything about the spawn fails.
    pub fn from_env() -> Option<LiveRenderer> {
        let bin = std::env::var_os("ACHE_LANA_RENDERER")?;
        let (Ok(glb), Ok(clips)) = (
            std::env::var("ACHE_LANA_GLB"),
            std::env::var("ACHE_LANA_CLIPS"),
        ) else {
            eprintln!(
                "ache-gui: ACHE_LANA_RENDERER is set but ACHE_LANA_GLB / \
                 ACHE_LANA_CLIPS are not — live figure off"
            );
            return None;
        };
        match Self::spawn(&bin, &glb, &clips) {
            Ok(renderer) => {
                eprintln!("ache-gui: live figure on (renderer: {})", bin.to_string_lossy());
                Some(renderer)
            }
            Err(err) => {
                eprintln!("ache-gui: live figure unavailable ({err}) — strip figure stays");
                None
            }
        }
    }

    fn spawn(
        bin: &std::ffi::OsStr,
        glb: &str,
        clips: &str,
    ) -> Result<LiveRenderer, String> {
        let mut child = Command::new(bin)
            .args([
                "--glb",
                glb,
                "--clips",
                clips,
                "--width",
                &FRAME_W.to_string(),
                "--height",
                &FRAME_H.to_string(),
                "--fps",
                "30",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| format!("spawn {}: {err}", bin.to_string_lossy()))?;
        let stdin = child.stdin.take().ok_or("renderer has no stdin")?;
        let stdout = child.stdout.take().ok_or("renderer has no stdout")?;
        // Handshake: one JSON line carrying "ready": true. A skim
        // for the marker is deliberate — the GUI crate has no JSON
        // dependency and gains none for a one-line greeting.
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|err| format!("renderer handshake read: {err}"))?;
        if !line.contains("\"ready\":true") && !line.contains("\"ready\": true") {
            return Err(format!("renderer handshake was not ready: {line:.80}"));
        }
        let shared = Arc::new(Shared::default());
        shared.alive.store(true, Ordering::SeqCst);
        {
            let shared = shared.clone();
            std::thread::spawn(move || read_frames(reader, &shared));
        }
        Ok(LiveRenderer {
            child,
            stdin: Mutex::new(stdin),
            seq: AtomicU64::new(0),
            shared,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::SeqCst)
    }

    /// Frames the renderer has produced so far (diagnostics).
    pub fn frames_received(&self) -> u64 {
        self.shared.received.load(Ordering::SeqCst)
    }

    /// Sends one drive state: the reaction engine's clip key and
    /// per-mille expression vector, unchanged.
    pub fn send_state(&self, clip_key: &str, expr: &Expr13) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let line = format!(
            "{{\"seq\":{seq},\"clip\":\"{}\",\"expr\":{{
             \"eyelid_aperture\":{},\"pupil_dilate\":{},\"pupil_shape\":{},
             \"eye_gloss\":{},\"tear_sheen\":{},\"brow_raise\":{},\"brow_angle\":{},
             \"mouth_open\":{},\"mouth_shape\":{},\"blush_intensity\":{},
             \"blush_spread\":{},\"skin_flush\":{},\"skin_sheen\":{}}}}}",
            escape_key(clip_key),
            expr.eyelid_aperture,
            expr.pupil_dilate,
            expr.pupil_shape,
            expr.eye_gloss,
            expr.tear_sheen,
            expr.brow_raise,
            expr.brow_angle,
            expr.mouth_open,
            expr.mouth_shape,
            expr.blush_intensity,
            expr.blush_spread,
            expr.skin_flush,
            expr.skin_sheen,
        );
        self.write_line(&line);
    }

    /// The live route's STOP: the sequence floor rises first, so
    /// frames already in flight from pre-stop states are discarded
    /// by the pump; then the stop state itself is written.
    pub fn send_stop(&self) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared.min_state_seq.store(seq, Ordering::SeqCst);
        self.write_line(&format!("{{\"seq\":{seq},\"stop\":true}}"));
    }

    /// Takes the newest frame as a Slint image, if one arrived
    /// since the last pump and it answers a state at or after the
    /// stop floor. Called on the GUI's 30 ms timer.
    pub fn poll_image(&self) -> Option<Image> {
        let frame = self.shared.latest.lock().ok()?.take()?;
        if frame.state_seq < self.shared.min_state_seq.load(Ordering::SeqCst) {
            return None;
        }
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(FRAME_W, FRAME_H);
        for (dst, src) in buffer
            .make_mut_slice()
            .iter_mut()
            .zip(frame.rgba.as_chunks::<4>().0)
        {
            *dst = Rgba8Pixel {
                r: src[0],
                g: src[1],
                b: src[2],
                a: src[3],
            };
        }
        Some(Image::from_rgba8(buffer))
    }

    fn write_line(&self, line: &str) {
        let result = self
            .stdin
            .lock()
            .map_err(|_| std::io::Error::other("renderer stdin lock poisoned"))
            .and_then(|mut stdin| writeln!(stdin, "{line}").and_then(|()| stdin.flush()));
        if result.is_err() {
            self.shared.alive.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for LiveRenderer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Clip keys are minted by `reaction::clip_key` from a closed
/// alphabet (band names, `.t<tier>`), but the wire format is JSON:
/// escape the two characters that could break the string anyway.
fn escape_key(key: &str) -> String {
    key.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The reader thread: length-prefixed frame records off the
/// child's stdout, newest-wins into the shared slot. Any EOF or
/// malformed record ends the stream and drops `alive`.
fn read_frames(mut reader: BufReader<std::process::ChildStdout>, shared: &Shared) {
    loop {
        let mut prefix = [0u8; 4];
        if reader.read_exact(&mut prefix).is_err() {
            break;
        }
        let len = u32::from_le_bytes(prefix) as usize;
        if len < HEADER_LEN || len > HEADER_LEN + (FRAME_W * FRAME_H * 4) as usize {
            break;
        }
        let mut payload = vec![0u8; len];
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        let state_seq = u64::from_le_bytes(payload[8..16].try_into().unwrap());
        let width = u32::from_le_bytes(payload[16..20].try_into().unwrap());
        let height = u32::from_le_bytes(payload[20..24].try_into().unwrap());
        if width != FRAME_W || height != FRAME_H || len != HEADER_LEN + (width * height * 4) as usize
        {
            break;
        }
        shared.received.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut latest) = shared.latest.lock() {
            *latest = Some(Frame {
                state_seq,
                rgba: payload[HEADER_LEN..].to_vec(),
            });
        }
    }
    shared.alive.store(false, Ordering::SeqCst);
}
