# KoiSync → ache — extracted spec (Phase 0)

Source: KoiSync v1.0.0, MIT, clone at
`~/.local/share/pax/koisync-study/KoiSync` @ `ec12ffb92e3e` ("Release
KoiSync v1.0.0"). Every claim below cites `path:line` in that tree.
Shipped-binary checks were made against the release zips
`KoiSync-v1.0.0-{KK,KKS}.zip` (SHA256 verified at intake) by reading
the UTF-16 string heap of `KoiSync.MainGame.dll` and `KoiSync.Studio.dll`
from both zips; no shipped code was run. KoiSync is evidence, not a port
base: ache's implementation is estate Rust under ache's own contract.

Discrepancies are numbered **D1…** and recorded, not resolved.

---

## 1. Axis model (6-DOF)

- Six axes in a fixed order, TCode names `L0 L1 L2 R0 R1 R2`
  (`src/Shared/Motion.cs:66`). File markers, same order:
  `stroke surge sway twist roll pitch` (`src/Shared/Funscript.cs:70`).
- Semantics (user guide table, `docs/KoiSync_User_Guide_EN.md:134-139`):
  L0 stroke, L1 surge (forward/back), L2 sway (left/right), R0 twist,
  R1 roll, R2 pitch. Neutral is the midpoint for L1–R2; L0's neutral is
  "out" (`:134`).
- Internal value space: every axis is a normalized double in `[0, 1]`
  (`src/Shared/Motion.cs:107-108`, defaults `Values = 0.5` for all six).
  With no script value, L0 defaults to `1`, the others to `0.5`
  (`src/Shared/SixAxisDrive.cs:35`).
- Per-axis limits and gain: `AxisMinimum`/`AxisMaximum` in `[0, 1]`,
  `AxisGain` default 1 (`src/Shared/Motion.cs:105-107`); gain is refused
  outside `[0, 20]` (`src/Shared/Motion.cs:308`); the result is clamped
  into `[lo, hi]` (`src/Shared/Motion.cs:309`). A global
  `Minimum = 0.1, Maximum = 0.9` and a slew cap `MaxUnitsPerSecond = 2`
  apply on top (`src/Shared/Motion.cs:104`, slew at `:310`).
- Device axis sets (`src/Shared/Motion.cs:318-323`): SR6 = all six,
  OSR2 = `L0, R1, R2`, SSR1 = `L0`; Handy has no TCode axes (it is
  Intiface-only, `src/Shared/Motion.cs:327`).

**Ache mapping.** Six first-class axes, each a 0–100 integer setpoint
in spec space (the XToys spec's percent convention), with a per-axis
ceiling. "Ceiling" for a positional axis is a *travel window*
`[min, max]` around the axis (the KoiSync limit-bar model above), not an
intensity cap: a setpoint outside the window clamps into it.

## 2. Multi-axis funscript pack format

- Writer (`src/KoiSync.Studio/StudioRecorder.cs:88-105`): six files per
  stem, suffixes `"" .surge .sway .twist .roll .pitch`
  (`:92`); each file is
  `{"version":"1.0","inverted":false,"range":100,"actions":[{"at":<ms>,"pos":<0..100>},…]}`
  (`:95-100`). All six files carry the **same** `at` column (one
  `times` list, `:12`, written for every axis at `:96-99`): the pack
  shares one time base. Positions are `round(clamp(v,0,1)×100)`
  (`:36`). Files are written to `.tmp` then replaced atomically
  (`:102-104`). Fewer than two frames refuses to save (`:90`).
- Reader (`src/Shared/Funscript.cs:14-46`): actions with `at < 0` or
  `pos` outside `0..100` are refused (`:25`); an empty action list is
  refused (`:28`); actions are **sorted by `at` and de-duplicated by
  key, last write wins** (`:22-26`); duration = last `at` (`:19`);
  `inverted` flips `pos → 100 - pos` (`:34`). Sampling is linear
  interpolation between bracketing actions, holding the first value
  before the first action (`:35-45`).
- Pack container: exactly six slots, absent axes allowed as `null`
  (`src/Shared/Funscript.cs:48-66`, six-slot check `:55`); pack
  duration = max of track durations (`:53`).
- Companion discovery accepts both marker families (`.surge` and
  `.L1`), ranking the TCode name first (`src/Shared/Funscript.cs:76-98`);
  the Studio loader also accepts a `.L0.funscript` main file
  (`src/KoiSync.Studio/Plugin.Scripts.cs:126`).
- Shipped check: the exact header literal
  `{"version":"1.0","inverted":false,"range":100,"actions":[` and the
  `.surge` / `.pitch` suffix literals are present once each in both
  shipped DLLs of both zips. Conforms.

**D1 (reader vs ache).** KoiSync's reader silently sorts and
de-duplicates out-of-order `at` values (`Funscript.cs:22-26`). Ache's
adapter refuses non-monotonic `at` (XToys spec §1.4 / A5). Ache is
stricter by design; a pack KoiSync plays may be refused by ache.

## 3. The 50 Hz sampling law

- Bake: the Studio script bake runs at a `0.02 s` step
  (`src/KoiSync.Studio/Plugin.Scripts.cs:232`; default
  `src/KoiSync.Studio/Plugin.StepBake.cs:27`), clamped to
  `[0.015, 0.10] s` (`Plugin.StepBake.cs:83`, `:118`). The guide states
  "50Hz (`0.02s` step)" (`docs/KoiSync_User_Guide_EN.md:125`).
- Recorder: one frame per ≥ 20 ms of timeline time when no oscillation
  is active (`src/KoiSync.Studio/StudioRecorder.cs:26`, `:35`).
- Live: the device worker loop sleeps 20 ms per iteration
  (`src/KoiSync.Plugin/Devices.cs:355`, 10 ms when a send delay or
  pulse is active, 50 ms on Intiface), sending the freshest command.

**D2 (law vs file).** Packs are *not* strict uniform 50 Hz grids: the
recorder inserts oscillation turn points at millisecond resolution
between 20 ms frames (`StudioRecorder.cs:37-49`, `:71-85`). Ache
therefore treats a pack as a piecewise-linear curve (the reader's own
law, `Funscript.cs:35-45`) and **resamples** it onto an exact 20 ms grid
at compile time; the 50 Hz law in ache is the *emission* grid, never an
assumption about the input file.

## 4. TCode framing (serial / UDP)

- Encoder (`src/Shared/Motion.cs:325-337`): for each supported, enabled
  axis, `AXIS + DDDD + "I" + interval`, where
  `DDDD = round(clamp(v, 0, 0.9999) × 10000)` formatted 4-digit
  zero-padded (`:333-334`) and `interval` is clamped to `5..250` ms
  (`:334`). Parts are space-joined and the frame ends with `\n`
  (`:336`). A non-finite axis value throws (`:332`). No enabled axis →
  empty string, nothing is sent (`:336`,
  `src/KoiSync.Plugin/Devices.cs:255`).
- Interval in use: `I20` for live commands
  (`src/KoiSync.Plugin/Devices.cs:254`), `I10` on the delayed and pulse
  paths (`:330`, `:337`).
- Stop: `DSTOP\n` on both serial and UDP
  (`src/KoiSync.Plugin/Devices.cs:50`,
  `src/KoiSync.Plugin/SerialTransport.cs:77`).
- Dead-man: a command older than `FreshnessSeconds = 0.25`
  (`src/Shared/Motion.cs:65`) is not resent; if the device was moving,
  the worker sends `DSTOP` once (`src/KoiSync.Plugin/Devices.cs:320`,
  `:345`). Closing a moving transport also sends `DSTOP`
  (`Devices.cs:360`).
- Serial: 8N1, no flow control, DTR/RTS disabled, write timeout 200 ms,
  baud refused outside `1200..2000000`, default 115200
  (`src/KoiSync.Plugin/SerialTransport.cs:49-60`,
  `src/KoiSync.Plugin/Devices.cs:35`). A short write is an error
  (`SerialTransport.cs:75`).
- UDP: connected datagram socket, one ASCII frame per datagram, no
  reply expected (`src/KoiSync.Plugin/Devices.cs:45-52`); default
  `127.0.0.1:8000` (`docs/KoiSync_User_Guide_EN.md:36`).
- Intiface bridge: L0 only, `LinearCmd L0=<F4>`
  (`src/KoiSync.Plugin/Devices.cs:254`). Not adopted by ache (ache's
  Buttplug provider already owns that route).
- Shipped check: `DSTOP`, `LinearCmd L0=` and the `D4` format literal
  are present in all four shipped DLLs. Conforms.

**D3 (guide vs encoder digits).** The guide's axis table speaks in
3-digit values `000…999` (`docs/KoiSync_User_Guide_EN.md:134-139`)
while its transport table and the encoder use 4 digits `0000–9999`
(`:35`; `Motion.cs:333-334`). Ache follows the encoder (4 digits).

**D4 (L0 polarity).** The guide says L0 `000` = deepest, `999` =
withdrawn (`docs/KoiSync_User_Guide_EN.md:134`), and the script drive
defaults a missing L0 to `1` = out (`SixAxisDrive.cs:35`). But the depth
solver's primary output is the *insertion fraction* (0 = withdrawn,
`StudioShaftMotion.cs:155`; `ShaftStrokeMapping.cs:24`, `:31`), which is
encoded directly unless the operator toggles `InvertStroke`
(`Motion.cs:103`, `:212`, `:290`). Source and guide disagree on which
end of L0 is "deep" in the default configuration. **Not resolved
here.** Ache's depth transform outputs insertion fraction and exposes
the inversion as an explicit, frozen calibration flag; which default is
physically right for a given device is an S4 (hardware-in-hand) fact.

## 5. Depth solver (origin-to-tip kinematics + calibration)

- Origin-to-tip: the shaft is a rod from `root = Origin` along the
  reference X axis with length `ShaftRestLength` (or visible path
  length), `tip = root + x·length` (`src/Shared/StudioShaftMotion.cs:121-127`).
  Target depth is `length − dot(entry − root, x)`
  (`StudioShaftMotion.cs:152`); the primary (L0) value is
  `clamp(targetDepth/length, 0, 1) × engagement`, forced to 0 when no
  material is inserted and the tip is not pinned (`:155`). Lateral
  L1/L2 come from `entry − tip` projected on the device Y/Z axes times
  engagement (`:156-158`), normalized as `0.5 + raw / (width × span)`
  (`:168`).
- Depth → stroke mapping (`src/Shared/ShaftStrokeMapping.cs:20-32`):
  inputs are the pose, `player` length (the user's), `game` length,
  `stroke` (device travel), and a mode from
  `DriveMappingMode { Absolute, Soft, Relative, Manual }`
  (`src/Shared/SixAxisDrive.cs:5`).
  `depth = max(0, TargetDepth) × Engagement` (`:27`);
  Soft scales by `player / modelLength` (`:28`); Relative (and Manual)
  scale by `min(player, game) / modelLength` (`:29`); Absolute does not
  scale. `limit = min(player, stroke)` (`:30`); Absolute clamps to
  `[0, limit]`, the others apply `SoftDepth` (`:31`); the result is
  divided by `stroke` (`:31`).
- `SoftDepth(depth, limit)`: linear to a knee at `0.8·limit`, then
  `knee + shoulder · excess/(shoulder + excess)` with
  `shoulder = 0.2·limit` (`ShaftStrokeMapping.cs:34-40`) — an asymptotic
  approach to `limit`, never exceeding it.
- Bounds: lengths valid in `[0.02, 0.5] m` (`ShaftStrokeMapping.cs:11`;
  model length down to `0.001 m`, `:12`,
  `src/Shared/ManualCalibrationMotion.cs:9`); invalid lengths throw
  (`ShaftStrokeMapping.cs:22`).
- Per-character calibration: interaction length from body height,
  `0.130 × height / 1.70`, height accepted in `[0.5, 5] m`, result
  clamped to `[0.02, 0.5]` (`ShaftStrokeMapping.cs:9`, `:14-18`).
  Reference length/stroke constants `0.1384` / `0.120`
  (`ShaftStrokeMapping.cs:7-8`).

**Ache mapping.** The depth solver lands as a *pure spec → axis-curve
transform*: input is a frozen depth curve (target depth and engagement
per sample) plus a frozen calibration (`player`, `game`, `stroke`,
`model` lengths, mode, invert flag); output is an L0 curve in 0–100.
Bone geometry (live in-game sampling) is not ache's input: ache never
reads a game. The transform's laws are §5's, re-derived in Rust and
proven by estate tests, not by KoiSync's checks.

## 6. Baker segmentation heuristics (reference only — not built)

From `src/KoiSync.Studio/StudioStepAnalyzer.cs` (comments cited):
1. Detect three-signal climax windows first; their boundaries become
   chapter cuts (`:117`). The three signals: rising-edge fluid prop
   activation (`:626`), sprint → deep-hold drop (`:638`), facial
   blend-shape spasm (`:645`); windows merge within 2.5 s and cap at
   7.0 s from onset (`:666-668`); a 16 s cooldown suppresses weak
   ripples after a burst (`:693-694`).
2. Detect pose/opening/actor-pair transitions from bone observations
   to split multi-act and multi-actor scenes (`:120-122`).
3. Merge chapter boundaries (`:125`); vote the binding per chapter
   (`:159`, `:256`), with guards: explicit attachment counts only within
   12 cm (`:266-268`), oral beats hand when oral ≤ 24 mm with ≥ 45 %
   of hand's score (`:331-334`), anal must beat vaginal by > 65 %
   without explicit attachment (`:349-351`).
4. Per chapter: P05–P95 percentile depth calibration, frequency, and a
   phase-matched loop seam ≥ max(2.5 s, 30 % of chapter) (`:392`,
   `:443`).
5. Anti-sprint guard: a 2.1–4.8 Hz, ≥ 18 % amplitude chapter is never
   replaced by synthetic climax motion (`:447-449`, `:616-619`).

These inform a later torogao-side exporter (INTAKE "Baker → export
path"); ache builds none of it.

## 7. Gap record

- **G1** Device firmware response to `I`-intervals, `DSTOP`, and
  out-of-range values: not established by source; an S4 fact.
- **G2** L0 physical polarity per device (D4): S4 fact.
- **G3** Shipped-behaviour checks were string-heap presence only; no
  shipped code was executed, so behavioural conformance of the
  shipped DLLs (cadence, interval choice) is unobserved.
- **G4** KoiSync's 11 check suites (`tests/`) were not run (they target
  .NET/Unity); they are reference for what ache must prove itself.
