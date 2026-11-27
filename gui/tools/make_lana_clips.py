#!/usr/bin/env python3
"""Render Lana's frozen reaction clips into GUI playback strips.

The ache GUI's figure was, until this tool, five still state strips
with a procedural breathing bob. The 32 frozen torogao-motion-clip/2
reaction clips (8 arousal bands x 4 intensity tiers, authored by the
animation lane against this app's own band taxonomy — see BANDS.md in
the lane record) are the real motion. This tool renders them off
Lana's rung-1 rig and packs them as strips the GUI plays.

Pipeline (deterministic; re-running reproduces the shipped strips):
  1. The official Blender build (primo's azoth build — the Arch build
     segfaults on IK-bearing files; TOOLS.md two-build rule) runs
     tools/render_lana_clips.py over the rung-1 blend: every frame of
     every clip's loop, Eevee, transparent film, 480x960 (2x the
     figure's 240x480 display cell). Binding and sampling laws are
     the rung-1 binder's and the frozen format's own — see that
     script's header.
  2. Each clip's frames are downscaled (LANCZOS) to the 240x480 cell
     and laid into a grid strip, COLS frames per row, saved as
     gui/assets/lana/clips/<band>.t<tier>.png. The GUI displays one
     cell via Slint source-clip, the same mechanism as the family
     turntable strips.
  3. gui/assets/lana/clips/manifest.json records, per clip: the
     frozen clip's freeze SHA-256, frame count, grid geometry, the
     rig blend's SHA-256, and the render settings — the strips are
     never hand-carried binaries without provenance.

Frame counts are asserted against each frozen clip's own range; a
mismatch aborts the build rather than shipping a short loop (the
clips' loop closure is exactly 0.0 by construction, and a truncated
strip would silently break it).

Usage (on primo, from gui/):
  python3 tools/make_lana_clips.py [--only building.t1]
                                   [--skip-render]
                                   [--frames-dir DIR]
Defaults resolve the estate locations of record: the rung-1 blend in
the lana-rung1 lane dir and the frozen clips in the reaction lane's
primo gate workspace. --blend / --clips-dir / --blender override.
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image

GUI_DIR = Path(__file__).resolve().parent.parent
CELL_W, CELL_H = 240, 480
RENDER_W, RENDER_H = 480, 960
COLS = 12

DEFAULT_BLEND = Path.home() / ".local/share/pax/lana-rung1-20261127/build/out/lana.blend"
DEFAULT_CLIPS = Path.home() / ".local/share/pax/lana-reaction-20261127/clips/frozen"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def find_blender(explicit: str | None) -> str:
    if explicit:
        return explicit
    hits = sorted(glob.glob(str(Path.home() / ".cache/azoth/blender-official/*/blender")))
    if hits:
        return hits[-1]
    return "blender"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", help="render/assemble just this clip key, e.g. building.t1")
    ap.add_argument("--skip-render", action="store_true",
                    help="reuse frames already in --frames-dir")
    ap.add_argument("--frames-dir", default="/tmp/ache-lana-clip-frames")
    ap.add_argument("--blend", default=str(DEFAULT_BLEND))
    ap.add_argument("--clips-dir", default=str(DEFAULT_CLIPS))
    ap.add_argument("--blender", default=None)
    args = ap.parse_args()

    frames_dir = Path(args.frames_dir)
    clips_dir = Path(args.clips_dir)
    out_dir = GUI_DIR / "assets" / "lana" / "clips"
    out_dir.mkdir(parents=True, exist_ok=True)

    clip_files = sorted(clips_dir.glob("*.clip.json"))
    if not clip_files:
        print(f"no frozen clips in {clips_dir}", file=sys.stderr)
        return 1
    census = {}
    for path in clip_files:
        doc = json.loads(path.read_text())
        key = doc["name"].rsplit("/", 1)[-1]
        census[key] = {
            "frames": doc["range"]["end"] - doc["range"]["start"],
            "freeze_sha256": doc["freeze"]["sha256"],
            "source": path.name,
        }
    if args.only and args.only not in census:
        print(f"--only {args.only} not in census", file=sys.stderr)
        return 1

    if not args.skip_render:
        cmd = [
            find_blender(args.blender), "-b", "-Y", "--offline-mode", "-noaudio",
            "-P", str(GUI_DIR / "tools" / "render_lana_clips.py"), "--",
            "--blend", args.blend, "--clips-dir", str(clips_dir),
            "--frames-dir", str(frames_dir),
        ]
        if args.only:
            cmd += ["--only", args.only]
        print("+", " ".join(cmd), flush=True)
        subprocess.run(cmd, check=True)

    manifest_path = out_dir / "manifest.json"
    manifest = {}
    if manifest_path.exists() and args.only:
        manifest = json.loads(manifest_path.read_text())
    manifest.update({
        "_rig_blend": args.blend,
        "_rig_blend_sha256": sha256(Path(args.blend)),
        "_render": f"Eevee, {RENDER_W}x{RENDER_H}, transparent film, "
                   f"cell {CELL_W}x{CELL_H}, cols {COLS}",
        "_tool": "gui/tools/make_lana_clips.py + render_lana_clips.py",
    })

    total_bytes = 0
    for key in sorted(census):
        if args.only and key != args.only:
            continue
        want = census[key]["frames"]
        frame_paths = sorted((frames_dir / key).glob("f*.png"))
        if len(frame_paths) != want:
            print(f"{key}: rendered {len(frame_paths)} frames, clip range says {want}",
                  file=sys.stderr)
            return 1
        rows = (want + COLS - 1) // COLS
        strip = Image.new("RGBA", (COLS * CELL_W, rows * CELL_H), (0, 0, 0, 0))
        for i, path in enumerate(frame_paths):
            frame = Image.open(path).convert("RGBA")
            if frame.size != (RENDER_W, RENDER_H):
                print(f"{key}: frame {path.name} is {frame.size}, "
                      f"expected {(RENDER_W, RENDER_H)}", file=sys.stderr)
                return 1
            frame = frame.resize((CELL_W, CELL_H), Image.LANCZOS)
            strip.paste(frame, ((i % COLS) * CELL_W, (i // COLS) * CELL_H))
        dest = out_dir / f"{key}.png"
        strip.save(dest, compress_level=6)
        total_bytes += dest.stat().st_size
        manifest[key] = {
            "frames": want,
            "cols": COLS,
            "cell": [CELL_W, CELL_H],
            "strip_px": [COLS * CELL_W, rows * CELL_H],
            "freeze_sha256": census[key]["freeze_sha256"],
            "source_clip": census[key]["source"],
        }
        print(f"{key}: {want} frames -> {dest.name} "
              f"{strip.size[0]}x{strip.size[1]}, {dest.stat().st_size / 1e6:.2f} MB",
              flush=True)

    manifest_path.write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    print(f"manifest: {manifest_path}")
    print(f"strips total this run: {total_bytes / 1e6:.1f} MB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
