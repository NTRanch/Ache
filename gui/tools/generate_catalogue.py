#!/usr/bin/env python3
"""Generate the ache-gui device catalogue (catalogue.tsv) from the Phase 0
records of the ache x IoST device-support commission.

Sources (catalogue of record, do not edit by hand):
  DIFF.md  - per-device support bucket (A / A-g / B / C) and IoST "(Untested)"
             flag, in the "Full classification" table.
  URLS.md  - per-device IoST-listed URL (verbatim) and English-availability
             link state, in the "All 471 rows" table.

Output: catalogue.tsv with columns
  brand <TAB> device <TAB> type <TAB> bucket <TAB> untested(1|0)
  <TAB> link_state <TAB> url (empty when the row has no link)

Rows are sorted brand-then-device, case-insensitive (IoST table order, per
PLAN.md Step 4), with the exact source string as tie-break so regeneration
is byte-stable. The join is validated: both sources must carry exactly the
same 471 (brand, device) keys and agree on every link state, and the bucket
and link-state tallies must match the Phase 0 published counts. Any drift
aborts generation instead of emitting a quietly wrong catalogue.

Usage: generate_catalogue.py <DIFF.md> <URLS.md> <output.tsv>
"""

import re
import sys

EXPECTED_ROWS = 471
EXPECTED_BUCKETS = {"A": 328, "A-g": 62, "B": 1, "C": 80}
EXPECTED_STATES = {
    "EN": 255,
    "EN-VARIANT": 0,
    "NON-EN": 11,
    "DEAD": 37,
    "NOLINK": 4,
    "UNVERIFIED": 164,
}
EXPECTED_UNTESTED = 52
NO_LINK_TEXT = "no link shown"


def fail(message: str) -> None:
    print(f"generate_catalogue: error: {message}", file=sys.stderr)
    raise SystemExit(1)


def split_row(line: str) -> list[str]:
    """Split one markdown table row into trimmed cells.

    Cells may contain escaped pipes ('\\|' in the source, e.g. the device
    "OhMiBod blueMotion NEX|1"), so split only on unescaped '|' and then
    unescape each cell.
    """
    text = line.strip()
    if not (text.startswith("|") and text.endswith("|")):
        fail(f"not a table row: {line[:80]!r}")
    cells = re.split(r"(?<!\\)\|", text[1:-1])
    return [cell.strip().replace("\\|", "|") for cell in cells]


def parse_diff(path: str) -> dict[tuple[str, str], dict[str, str]]:
    rows: dict[tuple[str, str], dict[str, str]] = {}
    in_table = False
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            if line.startswith("## Full classification"):
                in_table = True
                continue
            if in_table and line.startswith("## "):
                break
            if not in_table or not line.startswith("|"):
                continue
            cells = split_row(line)
            if cells[0] == "Brand" or set(cells[0]) <= {"-", " "}:
                continue  # header or separator
            # Columns: Brand | Device | Av/Dc | Type | U | Bucket | Evidence | Link state
            if len(cells) != 8:
                fail(f"DIFF row with {len(cells)} cells (!= 8): {line[:100]!r}")
            brand = cells[0]
            device = cells[1]
            device_type = cells[3]
            untested = cells[4]
            bucket = cells[5]
            link_state = cells[7]
            if bucket not in EXPECTED_BUCKETS:
                fail(f"DIFF row with unknown bucket {bucket!r}: {line[:100]!r}")
            key = (brand, device)
            if key in rows:
                fail(f"duplicate DIFF row for {key}")
            rows[key] = {
                "type": device_type,
                "untested": "1" if untested else "0",
                "bucket": bucket,
                "link_state": link_state,
            }
    return rows


def parse_urls(path: str) -> dict[tuple[str, str], dict[str, str]]:
    rows: dict[tuple[str, str], dict[str, str]] = {}
    in_table = False
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            if line.startswith("## All 471 rows"):
                in_table = True
                continue
            if in_table and line.startswith("## "):
                break
            if not in_table or not line.startswith("|"):
                continue
            cells = split_row(line)
            if cells[0] == "Brand" or set(cells[0]) <= {"-", " "}:
                continue
            # Columns: Brand | Device | URL | State | Basis
            if len(cells) != 5:
                fail(f"URLS row with {len(cells)} cells (!= 5): {line[:100]!r}")
            brand = cells[0]
            device = cells[1]
            key = (brand, device)
            if key in rows:
                fail(f"duplicate URLS row for {key}")
            rows[key] = {"url": cells[2], "link_state": cells[3]}
    return rows


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print(__doc__)
        return 2
    diff = parse_diff(argv[1])
    urls = parse_urls(argv[2])
    if len(diff) != EXPECTED_ROWS:
        fail(f"DIFF parsed {len(diff)} rows, expected {EXPECTED_ROWS}")
    if len(urls) != EXPECTED_ROWS:
        fail(f"URLS parsed {len(urls)} rows, expected {EXPECTED_ROWS}")
    if set(diff) != set(urls):
        only_diff = sorted(set(diff) - set(urls))[:5]
        only_urls = sorted(set(urls) - set(diff))[:5]
        fail(f"key mismatch: only in DIFF {only_diff}; only in URLS {only_urls}")

    merged = []
    for key, d in diff.items():
        u = urls[key]
        if d["link_state"] != u["link_state"]:
            fail(f"link state disagreement for {key}: DIFF {d['link_state']} vs URLS {u['link_state']}")
        url = "" if u["url"] == NO_LINK_TEXT else u["url"]
        if d["link_state"] == "NOLINK" and url:
            fail(f"NOLINK row carries a URL: {key}")
        if d["link_state"] != "NOLINK" and not url:
            fail(f"non-NOLINK row has no URL: {key}")
        if url and not url.startswith(("http://", "https://")):
            fail(f"URL with unexpected scheme for {key}: {url[:60]!r}")
        for field in (key[0], key[1], d["type"]):
            if "\t" in field or "\n" in field:
                fail(f"field contains a tab/newline for {key}")
        merged.append(
            (key[0], key[1], d["type"], d["bucket"], d["untested"], d["link_state"], url)
        )

    bucket_tally = {name: 0 for name in EXPECTED_BUCKETS}
    state_tally = {name: 0 for name in EXPECTED_STATES}
    untested_total = 0
    for row in merged:
        bucket_tally[row[3]] += 1
        state_tally[row[5]] += 1
        untested_total += int(row[4])
    if bucket_tally != EXPECTED_BUCKETS:
        fail(f"bucket tally {bucket_tally} != expected {EXPECTED_BUCKETS}")
    if state_tally != EXPECTED_STATES:
        fail(f"link-state tally {state_tally} != expected {EXPECTED_STATES}")
    if untested_total != EXPECTED_UNTESTED:
        fail(f"untested total {untested_total} != expected {EXPECTED_UNTESTED}")

    merged.sort(key=lambda r: (r[0].lower(), r[1].lower(), r[0], r[1]))
    with open(argv[3], "w", encoding="utf-8", newline="\n") as out:
        out.write("# ache device catalogue - GENERATED by tools/generate_catalogue.py\n")
        out.write("# Sources: ache-iost-devices-20261126 DIFF.md + URLS.md (Phase 0, 471 devices).\n")
        out.write("# Columns: brand, device, type, bucket, untested(1|0), link_state, url (tab-separated).\n")
        out.write("# Order: brand then device, case-insensitive. Do not edit by hand; regenerate instead.\n")
        for row in merged:
            out.write("\t".join(row) + "\n")
    print(f"generate_catalogue: wrote {len(merged)} rows to {argv[3]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
