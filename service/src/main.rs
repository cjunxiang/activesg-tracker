use axum::{
    extract::{Query, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use rusqlite::Connection;
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

const POLL_URL: &str = "https://activesg.gov.sg/api/trpc/pass.getFacilityCapacities?input=%7B%22json%22%3Anull%2C%22meta%22%3A%7B%22values%22%3A%5B%22undefined%22%5D%7D%7D";
// Firecrawl collector: the target's Cloudflare blocks every datacenter client we
// tried (Railway/GCP, Azure, Cloudflare Workers, including impersonated TLS,
// headless and headful browsers). Firecrawl's hosted scraping gets through, so
// the service polls through it instead.
//
// Anti-detection design, both sides of the call:
//   ActiveSG sees Firecrawl's own rotating proxy pool, not us: a single request
//   every ~8-9 minutes, only while facilities are open, jittered so it never
//   forms a fixed grid, and never retried in a burst.
//   Firecrawl sees three API keys used in rotation, ~29 requests/day each, far
//   under the free plan's per-key allowance and concurrency, with per-key
//   monthly accounting so a key can never be driven into its cap.

use chrono::{Datelike, FixedOffset, TimeZone, Timelike};

const FIRECRAWL_API: &str = "https://api.firecrawl.dev/v1/scrape";
/// Singapore has no DST, so a fixed offset is exact.
const SGT_SECS: i32 = 8 * 3600;
const JITTER_SECS: i64 = 180;

fn sgt() -> FixedOffset {
    FixedOffset::east_opt(SGT_SECS).expect("valid offset")
}

/// Polling window in Singapore time. Gyms run 07:00-22:00 and pools 08:00-21:30,
/// so the default only covers the hours where facilities are actually open;
/// override with OPEN_HOUR / CLOSE_HOUR if the estate changes.
fn open_hour() -> u32 {
    std::env::var("OPEN_HOUR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8)
}

fn close_hour() -> u32 {
    std::env::var("CLOSE_HOUR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(21)
}

/// Comma-separated keys; a single key is fine too.
fn firecrawl_keys() -> Vec<String> {
    std::env::var("FIRECRAWL_KEYS")
        .unwrap_or_default()
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

fn per_key_monthly_budget() -> i64 {
    std::env::var("FIRECRAWL_MONTHLY_PER_KEY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000)
}

fn month_key(now: chrono::DateTime<FixedOffset>) -> String {
    format!("{:04}-{:02}", now.year(), now.month())
}

fn used_this_month(db: &Connection, month: &str, key_id: &str) -> i64 {
    db.query_row(
        "SELECT COALESCE(SUM(used), 0) FROM credit_usage WHERE month = ?1 AND key_id = ?2",
        rusqlite::params![month, key_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

fn total_used_this_month(db: &Connection, month: &str) -> i64 {
    db.query_row(
        "SELECT COALESCE(SUM(used), 0) FROM credit_usage WHERE month = ?1",
        rusqlite::params![month],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

fn record_use(db: &Connection, month: &str, key_id: &str) {
    let _ = db.execute(
        "INSERT INTO credit_usage(month, key_id, used) VALUES (?1, ?2, 1)
         ON CONFLICT(month, key_id) DO UPDATE SET used = used + 1",
        rusqlite::params![month, key_id],
    );
}

/// Key with the most headroom left this month, or None when all are spent.
fn pick_key(db: &Connection, keys: &[String], month: &str, budget: i64) -> Option<(String, i64)> {
    keys.iter()
        .map(|k| (k.clone(), budget - used_this_month(db, month, k)))
        .filter(|(_, left)| *left > 0)
        .max_by_key(|(_, left)| *left)
}

/// Densest interval (minutes) that still fits the remaining per-day allowance.
/// Fewer days left in the month means more credits per day, so the interval
/// tightens automatically as the month progresses.
fn interval_for(remaining_credits: i64, days_left: i64) -> i64 {
    if days_left <= 0 {
        return 60;
    }
    let per_day = (remaining_credits as f64 / days_left as f64).floor() as i64;
    let window_minutes = ((close_hour() - open_hour()) * 60) as i64;
    let max_polls = per_day.max(1).min(window_minutes);
    // polls = floor(window / interval) + 1 <= max_polls
    let interval = if max_polls <= 1 {
        window_minutes
    } else {
        window_minutes / (max_polls - 1)
    };
    interval.clamp(8, 60)
}

/// Sleep until the next poll: outside opening hours, wait for the next opening.
fn next_delay_secs(db: &Connection, keys: &[String]) -> u64 {
    let now = chrono::Utc::now().with_timezone(&sgt());
    if now.hour() < open_hour() || now.hour() >= close_hour() {
        let next_open = if now.hour() < open_hour() {
            sgt().with_ymd_and_hms(now.year(), now.month(), now.day(), open_hour(), 0, 0)
        } else {
            let tomorrow = now.date_naive() + chrono::Days::new(1);
            sgt().with_ymd_and_hms(
                tomorrow.year(),
                tomorrow.month(),
                tomorrow.day(),
                open_hour(),
                0,
                0,
            )
        };
        let target = next_open.single().unwrap_or(now);
        let secs = (target - now).num_seconds().max(60) as u64;
        put_jitter(secs)
    } else {
        let month = month_key(now);
        let budget = per_key_monthly_budget() * keys.len().max(1) as i64;
        let used = total_used_this_month(db, &month);
        let left = (budget - used).max(0);
        let days_left = match now.day() {
            31 => 1,
            d => {
                let last = chrono::NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
                    .and_then(|f| f.checked_add_months(chrono::Months::new(1)))
                    .and_then(|n| n.pred_opt())
                    .map(|l| l.day())
                    .unwrap_or(30);
                (last - d + 1) as i64
            }
        };
        let interval_min = interval_for(left, days_left);
        put_jitter(interval_min as u64 * 60)
    }
}

fn put_jitter(base: u64) -> u64 {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as i64)
        .unwrap_or(0);
    let spread = JITTER_SECS * 2 + 1;
    (base as i64 + (n % spread) - JITTER_SECS).max(30) as u64
}

/// One billed scrape. Returns Firecrawl's response payload; the credit is spent
/// as soon as this succeeds, whether or not the payload later parses.
async fn firecrawl_request(bin: &str, key: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "url": POLL_URL,
        "formats": ["rawHtml"],
        "onlyMainContent": false,
        "proxy": "auto",
        "timeout": 45000,
    })
    .to_string();

    let path = std::env::temp_dir().join("fc-body.json");
    std::fs::write(&path, &body).map_err(|e| e.to_string())?;

    let out = tokio::process::Command::new(bin)
        .args([
            "-sS",
            "-m",
            "120",
            "-X",
            "POST",
            FIRECRAWL_API,
            "-H",
            &format!("authorization: Bearer {key}"),
            "-H",
            "content-type: application/json",
            "--data-binary",
            &format!("@{}", path.display()),
            "-w",
            "\n%{http_code}",
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;

    let text = String::from_utf8_lossy(&out.stdout);
    let (payload, code) = text
        .trim_end()
        .rsplit_once('\n')
        .ok_or("no status line from curl")?;
    if code != "200" {
        return Err(format!(
            "firecrawl HTTP {code}: {}",
            payload.chars().take(160).collect::<String>()
        ));
    }
    Ok(payload.to_string())
}

/// Firecrawl returns the target body inside `data.rawHtml`, and that body is the
/// tRPC envelope `{"result":{"data":{"json":{...}}}}` - unwrap it before parsing.
fn parse_snapshot(payload: &str) -> Result<Snapshot, String> {
    let v: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| format!("envelope: {e}"))?;
    if v.get("success").and_then(|x| x.as_bool()) != Some(true) {
        return Err(format!(
            "firecrawl success=false: {}",
            payload.chars().take(160).collect::<String>()
        ));
    }
    let raw = v
        .pointer("/data/rawHtml")
        .and_then(|r| r.as_str())
        .ok_or("no data.rawHtml in response")?;
    let body: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("rawHtml not json: {e}"))?;
    let inner = body.pointer("/result/data/json").unwrap_or(&body);
    serde_json::from_value(inner.clone()).map_err(|e| format!("snapshot fields: {e}"))
}

struct AppState {
    db: Mutex<Connection>,
}

#[derive(Deserialize)]
struct Rpc {
    result: RpcResult,
}
#[derive(Deserialize)]
struct RpcResult {
    data: RpcData,
}
#[derive(Deserialize)]
struct RpcData {
    json: Snapshot,
}
#[derive(Deserialize)]
struct Snapshot {
    timestamp: i64,
    #[serde(default, rename = "swimFacilities")]
    swim_facilities: Vec<Facility>,
    #[serde(default, rename = "gymFacilities")]
    gym_facilities: Vec<Facility>,
}
#[derive(Deserialize)]
struct Facility {
    name: String,
    #[serde(rename = "capacityPercentage", default)]
    capacity: i64,
    #[serde(rename = "isClosed", default)]
    is_closed: bool,
}

#[tokio::main]
async fn main() {
    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "capacities.db".into());
    let db = Connection::open(&db_path).expect("open sqlite");
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS capacities(
             facility TEXT NOT NULL,
             ts INTEGER NOT NULL,
             capacity INTEGER NOT NULL,
             is_closed INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX IF NOT EXISTS idx_cap_ts ON capacities(ts);
         CREATE INDEX IF NOT EXISTS idx_cap_fac ON capacities(facility);
         CREATE TABLE IF NOT EXISTS credit_usage(
             month TEXT NOT NULL,
             key_id TEXT NOT NULL,
             used INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY(month, key_id));
         CREATE TABLE IF NOT EXISTS visitors(
             vid TEXT PRIMARY KEY,
             first_seen INTEGER NOT NULL);",
    )
    .expect("init schema");
    // Older rows may contain duplicates from retried polls; collapse them before
    // the uniqueness constraint goes in. Failure here is non-fatal (constraint
    // simply won't be created and OR IGNORE degrades to a plain insert).
    let _ = db.execute_batch(
        "DELETE FROM capacities WHERE rowid NOT IN
             (SELECT MIN(rowid) FROM capacities GROUP BY facility, ts);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_cap_fac_ts ON capacities(facility, ts);",
    );
    let state = Arc::new(AppState { db: Mutex::new(db) });

    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || seed_if_empty(&st));
    }

    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let keys = firecrawl_keys();
                // Poll straight away when facilities are open, then schedule the
                // next slot. Waiting first would leave a fresh deploy idle for a
                // full interval before its first reading.
                let now = chrono::Utc::now().with_timezone(&sgt());
                let open = now.hour() >= open_hour() && now.hour() < close_hour();

                if open && keys.is_empty() {
                    // No Firecrawl keys configured: direct impersonated fetch.
                    match poll_once(&st).await {
                        Ok(n) => println!("[poll] stored {n} readings (direct)"),
                        Err(e) => eprintln!("[poll] direct error: {e}"),
                    }
                    let wait = {
                        let db = st.db.lock().unwrap();
                        next_delay_secs(&db, &keys)
                    };
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }

                if !open {
                    let wait = {
                        let db = st.db.lock().unwrap();
                        next_delay_secs(&db, &keys)
                    };
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }

                let month = month_key(now);
                let budget = per_key_monthly_budget();
                let picked = {
                    let db = st.db.lock().unwrap();
                    pick_key(&db, &keys, &month, budget)
                };
                let Some((key, left)) = picked else {
                    eprintln!("[poll] every key is spent for {month}; idling");
                    continue;
                };

                let bin =
                    std::env::var("IMPERSONATE_BIN").unwrap_or_else(|_| "curl-impersonate".into());
                match firecrawl_request(&bin, &key).await {
                    Ok(payload) => {
                        // Billed the moment the scrape returns, even if the body
                        // turns out to be unparseable.
                        record_use(&st.db.lock().unwrap(), &month, &key);
                        match parse_snapshot(&payload) {
                            Ok(snap) => {
                                let n = store_snapshot(&st, &snap);
                                println!(
                                    "[poll] {n} readings stored; key ...{} has {} credits left",
                                    &key[key.len().saturating_sub(6)..],
                                    left - 1
                                );
                            }
                            Err(e) => eprintln!("[poll] parse error (1 credit spent): {e}"),
                        }
                    }
                    Err(e) => eprintln!("[poll] firecrawl error (not billed): {e}"),
                }

                let wait = {
                    let db = st.db.lock().unwrap();
                    next_delay_secs(&db, &keys)
                };
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        });
    }

    let app = Router::new()
        .route("/", get(dashboard))
        .route("/api/data", get(api_data))
        .route("/api/visitors", get(api_visitors))
        .route("/api/credits", get(api_credits))
        .route("/wallpaper", get(wallpaper))
        .route("/ingest", post(ingest))
        .with_state(state);
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("bind port");
    println!("listening on :{port}");
    axum::serve(listener, app).await.unwrap();
}

/// A poll can repeat a facility, or repeat a whole snapshot when a retry lands on
/// the same timestamp; keep the write idempotent.
fn store_snapshot(state: &AppState, snap: &Snapshot) -> usize {
    // Stamp with scrape time: ActiveSG's feed froze its own timestamp field at
    // 2026-10-17 21:29 SGT while crowd values keep changing, so keying rows by
    // it collapsed every new snapshot into an ignored duplicate.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(snap.timestamp);
    let db = state.db.lock().unwrap();
    let mut inserted = 0usize;
    for f in snap
        .swim_facilities
        .iter()
        .chain(snap.gym_facilities.iter())
    {
        let inserted_now = db
            .execute(
                "INSERT OR IGNORE INTO capacities(facility, ts, capacity, is_closed)
             VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![f.name, ts, f.capacity, f.is_closed as i64],
            )
            .unwrap_or(0);
        // OR IGNORE returns Ok(0) for a duplicate; counting attempts as rows
        // made a fully-ignored snapshot log as "56 readings stored".
        inserted += inserted_now;
    }
    inserted
}

async fn poll_once(state: &AppState) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    // Cloudflare rejects datacenter HTTP clients by TLS fingerprint; only an
    // impersonated browser handshake gets through, so we shell out to
    // curl-impersonate instead of using a native HTTP crate.
    let bin = std::env::var("IMPERSONATE_BIN").unwrap_or_else(|_| "curl-impersonate".into());
    let target = std::env::var("IMPERSONATE_TARGET").unwrap_or_else(|_| "chrome124".into());
    // Optional residential proxy (IPRoyal/Webshare/ScraperAPI proxy mode):
    // datacenter IPs are blocked regardless of fingerprint, so self-hosted
    // polling needs a trusted egress route to be self-sustaining.
    let proxy = std::env::var("IMPERSONATE_PROXY").unwrap_or_default();
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["--impersonate", &target, "--compressed", "-sS", "-m", "30"]);
    if !proxy.is_empty() {
        cmd.args(["--proxy", &proxy]);
    }
    cmd.args(["-w", "\n%{http_code}", POLL_URL]);
    let out = cmd.output().await?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let head: String = text.chars().take(200).collect();
        return Err(format!("curl-impersonate failed: {head}").into());
    }
    let (body, code) = text
        .trim_end()
        .rsplit_once('\n')
        .ok_or("curl produced no status line")?;
    if code != "200" {
        let head: String = body.chars().take(120).collect();
        return Err(format!("HTTP {code}: {head}").into());
    }
    let snap: Rpc = serde_json::from_str(body)?;
    // Scrape time, not the feed's timestamp: ActiveSG froze that field
    // (see store_snapshot).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(snap.result.data.json.timestamp);
    let facilities = snap
        .result
        .data
        .json
        .swim_facilities
        .into_iter()
        .chain(snap.result.data.json.gym_facilities);
    let db = state.db.lock().unwrap();
    let mut count = 0;
    for f in facilities {
        db.execute(
            "INSERT OR IGNORE INTO capacities(facility, ts, capacity, is_closed)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![f.name, ts, f.capacity, f.is_closed as i64],
        )?;
        count += 1;
    }
    Ok(count)
}

fn seed_if_empty(state: &AppState) {
    normalize_names(&state.db.lock().unwrap());
    let seed_path = std::env::var("SEED_CSV").unwrap_or_default();
    if seed_path.is_empty() {
        return;
    }
    let Ok(csv) = std::fs::read_to_string(&seed_path) else {
        eprintln!("[seed] csv not found at {seed_path}, skipping");
        return;
    };
    let db = state.db.lock().unwrap();
    let existing: i64 = db
        .query_row("SELECT COUNT(*) FROM capacities", [], |r| r.get(0))
        .unwrap_or(0);
    if existing > 0 {
        println!("[seed] table not empty ({existing} rows), skipping");
        return;
    }
    let mut inserted = 0;
    // CSV rows: facility,timestamp_utc,capacity_pct,is_closed; parse from the
    // right so facility names containing commas survive.
    for line in csv.lines().skip(1) {
        let Some((facility, ts_ms, cap, closed)) = parse_csv_line(line) else {
            continue;
        };
        if db
            .execute(
                "INSERT OR IGNORE INTO capacities(facility, ts, capacity, is_closed)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![facility, ts_ms, cap, closed as i64],
            )
            .is_ok()
        {
            inserted += 1;
        }
    }
    println!("[seed] imported {inserted} historical rows (empty-capacity source rows skipped)");
}

// ActiveSG renamed several facilities between the March 2025 seed era and
// now (CC -> Community Centre, branding word order). Historical rows keep the
// old names, so fold them onto the current names at startup. Idempotent.
const RENAMES: &[(&str, &str)] = &[
    (
        "ActiveSG Gym @ Ang Mo Kio CC",
        "ActiveSG Gym @ Ang Mo Kio Community Centre",
    ),
    (
        "Enabling Village ActiveSG Gym",
        "ActiveSG Gym @ Enabling Village",
    ),
    (
        "ActiveSG Gym @ Toa Payoh West CC",
        "ActiveSG Gym @ Toa Payoh West Community Centre",
    ),
    (
        "Jurong Lake Gardens Gym",
        "Jurong Lake Gardens ActiveSG Gym",
    ),
    ("Senja-Cashew Gym", "Senja-Cashew ActiveSG Gym"),
];

fn normalize_names(db: &Connection) {
    let mut changed = 0;
    for (old, new) in RENAMES {
        changed += db
            .execute(
                "UPDATE capacities SET facility = ?2 WHERE facility = ?1",
                rusqlite::params![old, new],
            )
            .unwrap_or(0);
    }
    if changed > 0 {
        println!("[migrate] normalized {changed} rows to current facility names");
    }
}

fn parse_csv_line(line: &str) -> Option<(String, i64, i64, bool)> {
    let (b, closed_str) = line.rsplit_once(',')?;
    let (c, cap_str) = b.rsplit_once(',')?;
    let (facility, ts_str) = c.rsplit_once(',')?;
    let ts_ms = chrono::DateTime::parse_from_rfc3339(ts_str.trim())
        .ok()?
        .timestamp_millis();
    let cap = cap_str.trim().parse().ok()?;
    let closed = closed_str.trim().eq_ignore_ascii_case("true");
    Some((facility.to_string(), ts_ms, cap, closed))
}

async fn dashboard(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let cookies = headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .unwrap_or("");
    let existing = cookies.split(';').find_map(|p| {
        let p = p.trim();
        p.strip_prefix("vid=").map(|s| s.to_string())
    });
    let vid = existing
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // Random opaque id only - no IP, no fingerprint.
        let _ = state.db.lock().unwrap().execute(
            "INSERT OR IGNORE INTO visitors(vid, first_seen) VALUES (?1, ?2)",
            rusqlite::params![vid, now],
        );
    }
    let mut resp = Html(include_str!("dashboard.html")).into_response();
    if existing.is_none() {
        if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
            "vid={vid}; HttpOnly; SameSite=Lax; Max-Age=31536000; Path=/"
        )) {
            resp.headers_mut().append(axum::http::header::SET_COOKIE, v);
        }
    }
    resp
}

async fn api_credits(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let keys = firecrawl_keys();
    let now = chrono::Utc::now().with_timezone(&sgt());
    let month = month_key(now);
    let budget = per_key_monthly_budget();
    let db = state.db.lock().unwrap();
    let mut rows = Vec::new();
    let mut total = 0i64;
    for k in &keys {
        let used = used_this_month(&db, &month, k);
        total += used;
        rows.push(serde_json::json!({
            "key": format!("...{}", &k[k.len().saturating_sub(6)..]),
            "used": used,
            "left": budget - used,
        }));
    }
    let body = serde_json::json!({
        "month": month,
        "per_key_budget": budget,
        "total_budget": budget * keys.len().max(1) as i64,
        "total_used": total,
        "keys": rows,
    });
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        body.to_string(),
    )
}

async fn api_visitors(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let count: i64 = state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM visitors", [], |r| r.get(0))
        .unwrap_or(0);
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        format!("{{\"visitors\":{count}}}"),
    )
}

const WALLPAPER: &[u8] = include_bytes!("wallpaper.jpg");

async fn wallpaper() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
        WALLPAPER,
    )
}

#[derive(serde::Serialize)]
struct Row {
    f: String,
    t: i64,
    c: i64,
}

async fn api_data(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let days = params.get("days").cloned().unwrap_or_else(|| "30".into());
    // facility=JSON array string, e.g. ["Bishan ActiveSG Gym"]
    let facility = params.get("facility").cloned().unwrap_or_default();
    // back-compat: old clients sent a bare name; wrap it into the array form
    let facs = if facility.is_empty() || facility.starts_with('[') {
        facility
    } else {
        serde_json::to_string(&facility).unwrap_or_default()
    };
    let ftype = params.get("type").cloned().unwrap_or_default();

    let (gym, pool) = match ftype.as_str() {
        "gym" => (1, 0),
        "pool" => (0, 1),
        _ => (0, 0),
    };
    let sql = "SELECT facility, ts, capacity FROM capacities \
        WHERE (:days < 0 OR ts >= :days) \
          AND (:facs = '' OR facility IN (SELECT je.value FROM json_each(:facs) AS je)) \
          AND (:gym = 0 OR facility LIKE '%Gym%') \
          AND (:pool = 0 OR facility NOT LIKE '%Gym%') \
        ORDER BY ts ASC";

    let from_ms: i64 = if days == "max" {
        -1
    } else {
        days.parse().unwrap_or(30)
    };
    let cutoff: i64 = if from_ms < 0 {
        -1
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        now - from_ms * 86_400_000
    };

    let db = state.db.lock().unwrap();
    let mut stmt = match db.prepare(sql) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let rows = stmt
        .query_map(
            rusqlite::named_params! {
                ":days": cutoff,
                ":facs": facs,
                ":gym": gym,
                ":pool": pool,
            },
            |r| {
                Ok(Row {
                    f: r.get(0)?,
                    t: r.get(1)?,
                    c: r.get(2)?,
                })
            },
        )
        .and_then(|m| m.collect::<Result<Vec<_>, _>>());
    match rows {
        Ok(list) => (
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            serde_json::to_string(&list).unwrap_or_default(),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// Collectors that can solve Cloudflare (residential machine, CF Worker) push
// the raw tRPC snapshot here; auth is a shared bearer token. Accepts either
// the full tRPC envelope or the inner snapshot object.
async fn ingest(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> impl IntoResponse {
    let expected = std::env::var("INGEST_TOKEN").unwrap_or_default();
    let got = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if expected.is_empty() || got != expected {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    let inner = value.pointer("/result/data/json").cloned().unwrap_or(value);
    let body: Snapshot = match serde_json::from_value(inner) {
        Ok(b) => b,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response(),
    };
    let ts = body.timestamp;
    let db = state.db.lock().unwrap();
    let mut inserted = 0;
    for f in body.swim_facilities.into_iter().chain(body.gym_facilities) {
        if db
            .execute(
                "INSERT INTO capacities(facility, ts, capacity, is_closed) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![f.name, ts, f.capacity, f.is_closed as i64],
            )
            .is_ok()
        {
            inserted += 1;
        }
    }
    println!("[ingest] stored {inserted} readings at ts={ts}");
    (StatusCode::OK, format!("{{\"inserted\":{inserted}}}")).into_response()
}
