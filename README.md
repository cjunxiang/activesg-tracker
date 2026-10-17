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

## Notes

- ActiveSG's endpoint publishes only the current snapshot; history exists
  because this service (and the seed) collected it over time.
- Cloudflare front-door: plain HTTP clients may receive 403 from datacenter
  IPs. If polls fail, the fallback ladder is curl-impersonate, then a real
  browser runtime.

## Historical Data Source Credits
Historical datasource from https://github.com/frizensami/activesg-capacities
