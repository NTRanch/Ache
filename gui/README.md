# ache-gui

Desktop GUI for [ache](../README.md), the unified haptics MCP. A thin
Slint shell over ache's shared runtime — never a second implementation of
device logic. Build order: (1) device catalogue browser, (2) device
control, (3) session composition. This crate is phase 1: the catalogue
browser plus an inert control-preview pane. There is no device wiring in
this phase; later phases reach devices only through ache's existing
`decide()` gate and tool surface.

## Licensing

ache's own code is Apache-2.0, like the rest of this repository.

The Slint toolkit is used under the **Slint Royalty-free Desktop, Mobile,
and Web Applications License, version 2.0** (one of the three licences
SixtyFPS offers for Slint; the GPL-3.0-only and paid options are not
elected). That licence permits royalty-free distribution of Slint as part
of a desktop application, on conditions this crate satisfies by
construction:

- **Attribution (condition 2a):** the application displays the
  `AboutSlint` widget in an About dialog reachable from the window's top
  level (the "About" button in the header). Do not remove it.
- **No standalone distribution (limitation 3):** Slint ships only as part
  of the built `ache-gui` application, never on its own.
- **Not an embedded system, no Slint API exposure:** the GUI is a desktop
  application and exposes no Slint APIs.

## Catalogue data

`data/catalogue.tsv` is generated — do not edit it by hand. Regenerate
from the Phase 0 records of record after any catalogue update (e.g. the
link re-verification pass):

```sh
python3 tools/generate_catalogue.py \
    <path-to>/ache-iost-devices-20261126/DIFF.md \
    <path-to>/ache-iost-devices-20261126/URLS.md \
    data/catalogue.tsv
```

The generator validates the join (471 rows, bucket and link-state tallies
equal to the Phase 0 counts) and aborts on drift. `cargo test` re-checks
the embedded data, including the sortedness guard (brand then device,
case-insensitive).

## Build and check

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

The toolchain is pinned in `rust-toolchain.toml`, matching the server
crate in `mcp/`.
