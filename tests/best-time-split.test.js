const fs = require("fs");
// Extract the compare-view helpers from the shipped dashboard and exercise them
// with synthetic rows. Run: node tests/best-time-split.test.js
// (First dump the inline script: python3 - <<'EOF' ... see README)
const src = fs.readFileSync(process.argv[2] || "/tmp/app.js", "utf8");

// pull out the pure helpers the compare view depends on
function grab(name) {
  const decl = new RegExp(`function ${name}\\s*\\([^)]*\\)\\s*\\{[\\s\\S]*?\\n\\}`, "m");
  const arrow = new RegExp(`const ${name}\\s*=\\s*[^;]+;`, "m");
  const m = src.match(decl) || src.match(arrow);
  if (!m) throw new Error("missing function: " + name);
  return m[0];
}

const preamble = `
const DAYS = ["Mon","Tue","Wed","Thu","Fri","Sat","Sun"];
const HOURS = { open: 8, close: 21 };
const state = { favs: ["Gym B"] };
const egg = v => String(v);
`;

const body = [
  preamble,
  grab("heatColor"),
  grab("hourLabel"),
  grab("bucket"),
  grab("heatTableHtml"),
  grab("orderFacilities"),
].join("\n");

const sandbox = {};
const fn = new Function(body + "\nreturn {bucket, heatTableHtml, orderFacilities, heatColor, hourLabel};");
const api = fn(sandbox);

// synthetic rows: two facilities, distinct values, across two days
const rows = [];
// build rows in LOCAL time: the view buckets by local hour, which is what a
// Singapore user reads off the heatmap
for (const [facility, value] of [["Gym A", 80], ["Gym B", 20]]) {
  for (let d = 0; d < 2; d++) {
    for (let h = 7; h <= 22; h++) {
      rows.push({ f: facility, t: new Date(2026, 9, 5 + d, h, 15).getTime(), c: value });
    }
  }
}

let failures = 0;
const check = (label, cond, detail = "") => {
  if (!cond) { failures++; console.log(`FAIL ${label} ${detail}`); }
  else console.log(`ok   ${label} ${detail}`);
};

// 1) bucketing isolates one facility
const a = api.bucket(rows, "Gym A");
const b = api.bucket(rows, "Gym B");
const val = (buckets, day, hour) => buckets[day]?.[hour] ? Math.round(buckets[day][hour].sum / buckets[day][hour].n) : null;
check("Gym A bucketed to 80", val(a, 0, 10) === 80, `got ${val(a, 0, 10)}`);
check("Gym B bucketed to 20", val(b, 0, 10) === 20, `got ${val(b, 0, 10)}`);
check("closed hours excluded", val(a, 0, 7) === null && val(a, 0, 21) === null, "07:00 and 21:00 skipped");

// 2) heat table shape: 7 day rows x 13 open hours
const { html } = api.heatTableHtml(rows, "Gym A");
const rowsInTable = (html.match(/<tr>/g) || []).length - 1; // minus header row
const cellsInFirstRow = (html.match(/<td/g) || []).length / 7;
check("7 day rows", rowsInTable === 7, `got ${rowsInTable}`);
check("13 hour cells per row", cellsInFirstRow === 13, `got ${cellsInFirstRow}`);
check("caption carries facility", html.includes("<caption>Gym A</caption>"));

// 3) REGRESSION: each facility's heatmap must carry its own numbers, not the
// aggregate (the first version ignored the facility argument and every split
// heatmap came out identical)
const htmlA = api.heatTableHtml(rows, "Gym A").html;
const htmlB = api.heatTableHtml(rows, "Gym B").html;
const firstCell = html => (html.match(/<td[^>]*>(\d+)/) || [])[1];
check("Gym A heatmap shows 80", firstCell(htmlA) === "80", firstCell(htmlA));
check("Gym B heatmap shows 20", firstCell(htmlB) === "20", firstCell(htmlB));
check("split heatmaps differ", htmlA !== htmlB);

// 4) favourites sort first
const order = api.orderFacilities(rows);
check("favourite ranked first", order[0] === "Gym B", order.join(","));

// 4) hour labels
check("hour labels", api.hourLabel(8) === "8am" && api.hourLabel(12) === "12pm" && api.hourLabel(20) === "8pm",
  [api.hourLabel(8), api.hourLabel(12), api.hourLabel(20)].join(" "));

process.exit(failures ? 1 : 0);
