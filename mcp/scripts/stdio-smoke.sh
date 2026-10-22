#!/usr/bin/env bash
# Scripted stdio smoke for lovense-mcp over the mock transport.
# Usage: scripts/stdio-smoke.sh <path-to-lovense-mcp>
# Prints the request/response transcript; exits non-zero on any failed check.
# A manual convenience: the gated smoke is `cargo test --test stdio_smoke`,
# which writes its transcript to target/tmp/stdio-smoke.txt.
set -euo pipefail

binary="${1:?usage: stdio-smoke.sh <path-to-lovense-mcp>}"

requests='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lovense_status","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lovense_toys","arguments":{}}}'

# Mock transport: run with no config file.
responses="$(printf '%s\n' "$requests" | env -u LOVENSE_MCP_CONFIG "$binary")"

echo "== requests"
printf '%s\n' "$requests"
echo "== responses"
printf '%s\n' "$responses"

python3 -I - "$responses" <<'EOF'
import json, sys
lines = [json.loads(line) for line in sys.argv[1].splitlines()]
assert [m["id"] for m in lines] == [1, 2, 3, 4], "one response per request, none for the notification"
init = lines[0]["result"]
assert init["protocolVersion"] == "2024-11-05"
assert init["serverInfo"] == {"name": "lovense", "version": "0.1.0"}
names = sorted(tool["name"] for tool in lines[1]["result"]["tools"])
assert names == ["lovense_status", "lovense_toys"], names
status = json.loads(lines[2]["result"]["content"][0]["text"])
assert lines[2]["result"]["isError"] is False
assert status["state"] == "mock" and status["transport"] == "mock", status
toys = json.loads(lines[3]["result"]["content"][0]["text"])
assert lines[3]["result"]["isError"] is False
assert toys["source"] == "mock_fixture" and toys["count"] == 2, toys
print("== smoke: PASS (initialize, tools/list = 2 tools, lovense_status mock, lovense_toys 2 fixture toys)")
EOF
