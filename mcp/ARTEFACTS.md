# Release artefacts — ache 1.0.0 (R-602 / R-603 evidence)

Built 2026-10-28 on primo from a fresh copy of the 1.0.0
tree (the canonical tree copied with `target/` and `.git`
excluded, then built cold in the copy — R-602's
clean-environment build; the copy's release binary then ran
the shipped `scripts/stdio-smoke.sh` full session: PASS —
R-602's run check).

| Artefact | Bytes | SHA-256 |
|---|---|---|
| `ache` (release binary, `cargo build --release`) | 16,100,448 | `1adeb5692bf7c2bf10aa2d768fe18d288a6fdfabf062f8e4dee3b5330f665218` |
| `ache-1.0.0.crate` (`cargo package`) | 1,631,920 | `c55d45ca2b2ea74e5ae6af8bb924385c147d81f8ce957396c3fb54f94a1c716f` |

Toolchain: nightly-2026-09-25 pin (`rust-toolchain.toml`);
resolved clippy 0.1.100 (f7575a9da8 2026-09-24). The crate
artefact is the file published to crates.io as ache 1.0.0
under the NTRanch account (owner id 445961); crates.io
serves its own copy, and its published checksum is visible
on the crate's API record.
