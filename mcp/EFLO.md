# EFLO record — ache tools/list payload size

EFLO = the measured token size of the server's live
`tools/list` payload under a real tokenizer, plus the
declared-vs-detected access audit (no tool exceeds its
annotations).

## Standing 1.0 measure (10-tool surface)

Measured live on primo on 2026-10-28 against the built
1.0.0 binary (mock session, `server/discover` →
`tools/list`; the payload is the full wire result object a
client receives):

| Measure | Value |
|---|---|
| Tools | 10 |
| Payload | 7,738 bytes |
| **cl100k_base** | **1,760** |
| o200k_base (informational) | 1,793 |
| Fleet budget (cl100k_base) | ≤ 2,500 |
| Margin | 740 |

Per-tool cl100k_base (each tool object, canonical JSON):

| Tool | Tokens |
|---|---|
| `lovense_status` | 112 |
| `lovense_toys` | 120 |
| `lovense_arm` | 105 |
| `lovense_disarm` | 94 |
| `lovense_function` | 303 |
| `lovense_pattern` | 353 |
| `lovense_preset` | 254 |
| `lovense_stop` | 110 |
| `ache_devices` | 157 |
| `ache_device` | 143 |
| **Sum of tools** | **1,751** |

(Sum of tools 1,751 + result-object framing 9 = the 1,760
full-payload figure above.)

## Declared vs detected access (R-403)

Annotations as served on the wire at 1.0.0, audited against
the dispatch behaviour in `src/server.rs` / `src/service.rs`:

| Tool | readOnly | destructive | idempotent | Detected behaviour |
|---|---|---|---|---|
| `lovense_status` | true | false | true | Reads session state; makes no network call, changes nothing. Matches. |
| `lovense_toys` | true | false | true | One discovery read on the Lovense provider; arms nothing, outputs nothing. Matches. |
| `ache_devices` | true | false | true | Enumerates both providers; a provider that cannot enumerate reports its typed error in its entry. No output. Matches. |
| `ache_device` | true | false | true | One device lookup by unified id. No output. Matches. |
| `lovense_arm` | false | false | true | Changes session state (arms the master toggle); starts no output by itself. Matches. |
| `lovense_disarm` | false | false | true | Changes session state; zeroes active output first. Matches. |
| `lovense_function` | false | false | false | Timed output through the `decide()` gate; repeating a call repeats output. Matches. |
| `lovense_pattern` | false | false | false | Timed pattern output through the gate. Matches. |
| `lovense_preset` | false | false | false | Timed preset output through the gate (Lovense provider only). Matches. |
| `lovense_stop` | false | false | true | Zeroes every device on both providers and latches off — a state change, so not read-only; repeating it is a no-op success, so idempotent. Matches. |

No tool is destructive: nothing deletes data, and the one
tool that forces state (`lovense_stop`) is the safety tool
itself. **No tool exceeds its annotations — the EFLO check
passes.**

## Disposition

1,760 cl100k_base is inside the fleet ≤2,500 budget with a
740-token margin; no waiver is needed or requested (contrast
neemu's D-147 waiver, which is neemu-only). Any new tool must
budget its description + schema against the remaining margin
before shipping.
