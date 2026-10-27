#!/usr/bin/env bash
# Scripted stdio smoke for ache over the mock transport: one
# full session — discovery, a denied output while disarmed, arm, an
# over-ceiling function (clamped on the wire), pattern, preset, stop.
# Usage: scripts/stdio-smoke.sh <path-to-ache>
# Prints the request/response transcript; exits non-zero on any failed check.
# A manual convenience: the gated smoke is `cargo test --test stdio_smoke`,
# which writes its transcript to target/tmp/stdio-smoke.txt.
set -euo pipefail

binary="${1:?usage: stdio-smoke.sh <path-to-ache>}"

requests='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lovense_status","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lovense_toys","arguments":{}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"lovense_function","arguments":{"feature":"Vibrate","strength":5,"time_sec":5}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"lovense_arm","arguments":{}}}
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"lovense_function","arguments":{"feature":"Vibrate","strength":25,"time_sec":5}}}
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"lovense_pattern","arguments":{"features":"v","interval_ms":500,"strengths":[10,20],"time_sec":4}}}
{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"lovense_preset","arguments":{"name":"pulse","time_sec":9}}}
{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"lovense_stop","arguments":{}}}
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"lovense_status","arguments":{}}}'

# Mock transport: run with no config file.
responses="$(printf '%s\n' "$requests" | env -u ACHE_MCP_CONFIG "$binary")"

echo "== requests"
printf '%s\n' "$requests"
echo "== responses"
printf '%s\n' "$responses"

python3 -I - "$responses" <<'EOF'
import json, sys
lines = [json.loads(line) for line in sys.argv[1].splitlines()]
assert [m["id"] for m in lines] == list(range(1, 12)), "one response per request, none for the notification"
init = lines[0]["result"]
assert init["protocolVersion"] == "2024-11-05"
assert init["serverInfo"] == {"name": "ache", "version": "0.3.0"}
names = [tool["name"] for tool in lines[1]["result"]["tools"]]
assert names == ["lovense_status", "lovense_toys", "lovense_arm", "lovense_disarm",
                 "lovense_function", "lovense_pattern", "lovense_preset", "lovense_stop",
                 "ache_devices", "ache_device"], names

def payload(line):
    result = line["result"]
    return json.loads(result["content"][0]["text"]), result["isError"]

status, err = payload(lines[2])
assert err is False and status["state"] == "mock" and status["armed"] is False, status
toys, err = payload(lines[3])
assert err is False and toys["source"] == "mock_fixture" and toys["count"] == 2, toys
denied, err = payload(lines[4])
assert err is True and denied["error"]["code"] == "not_armed", denied
armed, err = payload(lines[5])
assert err is False and armed["armed"] is True, armed
ran, err = payload(lines[6])
assert err is False and ran["effective"]["strength"] == 20 and ran["gate"]["clamped"] is True, ran
assert ran["command"]["action"] == "Vibrate:20", ran
pattern, err = payload(lines[7])
assert err is False and pattern["command"]["rule"] == "V:1;F:v;S:500#", pattern
preset, err = payload(lines[8])
assert err is False and preset["command"]["name"] == "pulse", preset
stopped, err = payload(lines[9])
assert err is False and stopped["stop_sent"] is True, stopped
final, err = payload(lines[10])
assert err is False and final["armed"] is False and final["output_active"] is False, final
print("== smoke: PASS (10 tools; disarmed denied; arm; 25 clamped to 20; pattern; preset; stop; final state safe)")
EOF
