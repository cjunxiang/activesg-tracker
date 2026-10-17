# Preface

Meant to meaningfully consume activeSG data, not overwhelm activeSG servers; 
reasonable intervals, sufficient to understand crowd patterns for better decision making. (i'm sick of the crowd) 

In fact, we're aggregating data and channeling traffic over here which eases their server load. 

# ActiveSG Crowd Tracker

Polls ActiveSG's public gym/pool crowd endpoint every 10 minutes and serves a
filterable history dashboard. Rust + SQLite in one small binary.

## Layout

- `service/` - the Rust crate: poller, SQLite store, HTTP server, dashboard
- `data/activesg-capacities.csv` - historical seed (51 facilities, 9 Mar - 9 Apr 2025),
  recovered from [frizensami/activesg-capacities](https://github.com/frizensami/activesg-capacities)

## Endpoints

- `GET /` - dashboard (facility / type / range filters, client-side)
- `GET /api/data?days=7|30|90|max&facility=<name>&type=pool|gym` - readings as JSON

## Running

```sh
docker build -t activesg-tracker . && docker run -p 3000:3000 activesg-tracker
```

`DB_PATH` (SQLite file) and `SEED_CSV` (optional one-time historical import)
are environment variables; the Dockerfile defaults both to `/data`, which is
where a Railway volume should be mounted.

## Disclaimer

This is an **unofficial, non-commercial hobby project**. It is **not affiliated with, endorsed by, or connected to ActiveSG or Sport Singapore (SportSG)**.

- Crowd-level data is sourced from the publicly available ActiveSG gym & pool crowd page (https://activesg.gov.sg/gym-pool-crowd).
- The tracker makes a single lightweight request roughly every 10 minutes - far less traffic than a person loading the page in a browser. It does not bypass any login, paywall, or access control.
- Only aggregate, non-personal occupancy figures are collected. No personal data is accessed or stored.
- All facility names and data belong to ActiveSG / SportSG. This project simply visualises publicly displayed figures for convenience.
- The ActiveSG site publishes no robots.txt. We work on best effort, in good faith, to minimise our footprint: a single lightweight request every 10 minutes, no parallelism, no retries beyond backoff.

If you are the data owner and would like this project modified or taken down, please open an issue or contact me - happy to comply.

## Collector (Firecrawl)

ActiveSG sits behind Cloudflare bot management that rejects every datacenter client
we tested - Railway/GCP, Azure (GitHub Actions), Cloudflare Workers themselves,
impersonated TLS (curl-impersonate with Chrome/Safari fingerprints), headless and
headful Chromium, crawl4ai's undetected and camoufox modes. Residential egress is
the only thing that passes, so the service polls through Firecrawl's hosted
scraping, which brings its own rotating proxy pool.

Design goals, both sides of each request:

- **ActiveSG sees** one request every ~8-9 minutes, only while facilities are open
  (default 08:00-21:00 SGT), jittered +/-3 minutes so the pattern never forms a
  fixed grid, and never retried in a burst.
- **Firecrawl sees** several API keys used by remaining monthly headroom, each
  staying far below its plan allowance, with per-key accounting in SQLite so a key
  can never be driven into its cap.

The interval is derived, not configured: remaining credits divided by days left in
the month sets the per-day poll budget, so the cadence tightens automatically as the
month progresses and never overspends.

| Env var | Default | Meaning |
|---|---|---|
| `FIRECRAWL_KEYS` | _(unset)_ | comma-separated API keys; unset falls back to the direct fetch |
| `FIRECRAWL_MONTHLY_PER_KEY` | `1000` | credits assumed per key per month |
| `OPEN_HOUR` / `CLOSE_HOUR` | `8` / `21` | polling window, Singapore time |
| `DB_PATH` / `SEED_CSV` | `/data/...` | SQLite file and one-time historical import |

`scripts/collector/schedule.py` models the same maths offline
(`--optimal`, `--frontier`, `--chart out.png`).

## Notes

**Unofficial project** - not affiliated with, endorsed by, or connected to ActiveSG or
SportSG. The official crowd page lives at
[activesg.gov.sg/gym-pool-crowd](https://activesg.gov.sg/gym-pool-crowd).

- ActiveSG's endpoint publishes only the current snapshot; history exists
  because this service (and the seed) collected it over time.
- Cloudflare front-door: plain HTTP clients may receive 403 from datacenter
  IPs. If polls fail, the fallback ladder is curl-impersonate, then a real
  browser runtime.

## Historical Data Source Credits
Historical datasource from https://github.com/frizensami/activesg-capacities
