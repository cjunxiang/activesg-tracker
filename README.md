# Preface

Meant to meaningfully consume activeSG data, not overwhelm activeSG servers; 
reasonable intervals, sufficient to understand crowd patterns for better decision making. (i'm sick of the crowd) 

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

If you are the data owner and would like this project modified or taken down, please open an issue or contact me - happy to comply.

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
