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
const POLL_SECS: u64 = 600;

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
         CREATE INDEX IF NOT EXISTS idx_cap_fac ON capacities(facility);",
    )
    .expect("init schema");
    let state = Arc::new(AppState { db: Mutex::new(db) });

    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || seed_if_empty(&st));
    }

    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                match poll_once(&st).await {
                    Ok(n) => println!("[poll] stored {n} facility readings"),
                    Err(e) => eprintln!("[poll] error: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(POLL_SECS)).await;
            }
        });
    }

    let app = Router::new()
        .route("/", get(dashboard))
        .route("/api/data", get(api_data))
        .route("/ingest", post(ingest))
        .with_state(state);
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("bind port");
    println!("listening on :{port}");
    axum::serve(listener, app).await.unwrap();
}

async fn poll_once(state: &AppState) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    // Cloudflare rejects datacenter HTTP clients by TLS fingerprint; only an
    // impersonated browser handshake gets through, so we shell out to
    // curl-impersonate instead of using a native HTTP crate.
    let bin = std::env::var("IMPERSONATE_BIN").unwrap_or_else(|_| "curl-impersonate".into());
    let target = std::env::var("IMPERSONATE_TARGET").unwrap_or_else(|_| "chrome124".into());
    let out = tokio::process::Command::new(bin)
        .args([
            "--impersonate",
            &target,
            "--compressed",
            "-sS",
            "-m",
            "30",
            "-w",
            "\n%{http_code}",
            POLL_URL,
        ])
        .output()
        .await?;
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
    let ts = snap.result.data.json.timestamp;
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
            "INSERT INTO capacities(facility, ts, capacity, is_closed) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![f.name, ts, f.capacity, f.is_closed as i64],
        )?;
        count += 1;
    }
    Ok(count)
}

fn seed_if_empty(state: &AppState) {
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
                "INSERT INTO capacities(facility, ts, capacity, is_closed) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![facility, ts_ms, cap, closed as i64],
            )
            .is_ok()
        {
            inserted += 1;
        }
    }
    println!("[seed] imported {inserted} historical rows (empty-capacity source rows skipped)");
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

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
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
    let facility = params.get("facility").cloned().unwrap_or_default();
    let ftype = params.get("type").cloned().unwrap_or_default();

    let (gym, pool) = match ftype.as_str() {
        "gym" => (1, 0),
        "pool" => (0, 1),
        _ => (0, 0),
    };
    let sql = "SELECT facility, ts, capacity FROM capacities \
        WHERE (:days < 0 OR ts >= :days) \
          AND (:facility = '' OR facility = :facility) \
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
                ":facility": facility,
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
    let inner = value
        .pointer("/result/data/json")
        .cloned()
        .unwrap_or(value);
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
