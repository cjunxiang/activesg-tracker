"""Static guard: every helper the dashboard calls must be defined in the inline
script. Catches the class of bug where a block replacement deletes a function
while its call site survives (that shipped once: buildDayChart).

    python3 tests/reference-guard.py
"""
import re
import sys
from pathlib import Path

html = Path(__file__).resolve().parents[1] / "service/src/dashboard.html"
src = html.read_text()
main = [b for b in re.findall(r"<script>(.*?)</script>", src, re.S) if "const state =" in b][0]

called = {
    "load", "loadTimeline", "loadBestTime", "buildHeatmap", "buildGoTable",
    "buildDayChart", "heatTableHtml", "bucket", "orderFacilities", "renderStats",
    "renderChart", "rebuildFacilityBox", "updateHint", "showFatal", "loadVisitors",
}
missing = [
    fn for fn in sorted(called)
    if not re.search(rf"function {fn}\s*\(", main) and not re.search(rf"const {fn}\s*=", main)
]
if missing:
    print("missing definitions:", ", ".join(missing))
    sys.exit(1)
print(f"ok - {len(called)} helpers defined")
