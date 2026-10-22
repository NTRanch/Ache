# lovense-mcp

An MCP stdio server over the Lovense Standard API **local Game Mode**
route, built by Nice Thoughts Ranch. The governing brief is
[`../MCP-BRIEF.md`](../MCP-BRIEF.md); build tracking is Phase 6 of
[`../CHECKLIST.md`](../CHECKLIST.md).

## Stage S1: read core

This stage can only **read**. It reports its own status and discovers
toys. There is no output path at all: no function, pattern, preset,
arm, or stop exists in the code, the transport trait, or the tool list.
S2 adds those behind the brief's `decide()` gate.

Tools (protocol `2024-11-05`, newline-delimited JSON-RPC on stdio):

| Tool | What it does |
| --- | --- |
| `lovense_status` | State (`unconfigured`, `mock`, `ready`), transport, target, ceiling, watchdog, version. Makes no network call. |
| `lovense_toys` | Mock: the two fixture toys. Real with a host: one `GetToys` request, result reported honestly. Real without a host: `target_unconfigured` error and no network call. |

Neither tool takes arguments; any argument field is refused.

`ready` means a real target is **configured**, not that it answered.
Only `lovense_toys` contacts the app.

## Configuration

Set `LOVENSE_MCP_CONFIG` to a TOML file. Unset, the server runs with
the defaults below (mock transport, no target). Set, the file must exist
and validate; otherwise the server exits with status 2 and a JSON error
on stderr. Unknown keys are refused.

```toml
transport = "mock"        # "mock" | "real"
host = ""                 # bare hostname or IPv4; empty = no target
port = 20010              # 1..=65535 (the app's HTTPS port is usually 30010)
use_https = false
intensity_ceiling = 20    # 0..=20; reported in S1, binds output from S2
watchdog_secs = 5         # 1..=60; reported in S1, binds output from S2
```

Host and port are always configuration, never constants: the app's
ports move. The local route needs no token, and the server holds no
credentials.

## Wire contract (real transport)

`POST {scheme}://{host}:{port}/command`, header `X-platform`, body
`{"command":"GetToys"}`. Two-second timeout, no redirects, response capped
at 256 KiB. The reply must carry `"code": 200`; `data.toys` may be a
JSON object or a JSON string holding one (the documented shape).

## Build and test

```sh
nix develop          # cargo, clippy, rustfmt, python3
cargo test           # the transport tests start a loopback python3 stub
nix build
```

Licensed Apache-2.0 (see the repository `LICENSE`).
