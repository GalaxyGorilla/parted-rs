#!/usr/bin/env bash
set -euo pipefail

binary=${1:-target/release/parted}
for tool in truncate sfdisk python3; do
  command -v "$tool" >/dev/null || {
    echo "missing required interoperability tool: $tool" >&2
    exit 77
  }
done
[[ -x "$binary" ]] || {
  echo "build the release binary first: cargo build --release --locked" >&2
  exit 2
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

ours_gpt="$work/ours-gpt.img"
truncate -s 64MiB "$ours_gpt"
"$binary" --script --backup "$work/gpt-label.bak" "$ours_gpt" mklabel gpt >/dev/null
"$binary" --script --backup "$work/gpt-part.bak" \
  "$ours_gpt" mkpart 1MiB 32MiB --name data >/dev/null
sfdisk --json "$ours_gpt" >"$work/ours-gpt.json"
python3 - "$work/ours-gpt.json" <<'PY'
import json, sys
table = json.load(open(sys.argv[1], encoding="utf-8"))["partitiontable"]
part = table["partitions"][0]
assert table["label"] == "gpt"
assert part["start"] == 2048
assert part["size"] == 65536 - 2048
assert part["name"] == "data"
PY

external_gpt="$work/external-gpt.img"
truncate -s 64MiB "$external_gpt"
sfdisk "$external_gpt" >/dev/null <<'EOF'
label: gpt
start=2048, size=32768, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="external"
EOF
"$binary" --json "$external_gpt" print >"$work/external-gpt.json"
python3 - "$work/external-gpt.json" <<'PY'
import json, sys
disk = json.load(open(sys.argv[1], encoding="utf-8"))
part = disk["partitions"][0]
assert disk["label"] == "gpt"
assert part["first_lba"] == 2048
assert part["last_lba"] == 2048 + 32768 - 1
assert part["name"] == "external"
PY

ours_mbr="$work/ours-mbr.img"
truncate -s 16MiB "$ours_mbr"
"$binary" --script --backup "$work/mbr-label.bak" "$ours_mbr" mklabel mbr >/dev/null
"$binary" --script --backup "$work/mbr-part.bak" \
  "$ours_mbr" mkpart 2048 4095 --type 83 >/dev/null
sfdisk --json "$ours_mbr" >"$work/ours-mbr.json"
python3 - "$work/ours-mbr.json" <<'PY'
import json, sys
table = json.load(open(sys.argv[1], encoding="utf-8"))["partitiontable"]
part = table["partitions"][0]
assert table["label"] == "dos"
assert part["start"] == 2048
assert part["size"] == 2048
assert part["type"].lower() == "83"
PY

echo "parted interoperability: reciprocal GPT and MBR checks passed"
