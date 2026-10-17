#!/usr/bin/env python3
"""ActiveSG poll schedule: 30-min slots inside opening hours, Singapore time.

Why these numbers: one scrape returns all 56 facilities, so cost is per poll.
Gyms run 07:00-22:00 and pools 08:00-21:30, so the useful window is 07:00-21:00
(the last hour is the thinnest signal and the cheapest thing to drop). At
29 polls/day the worst month (31 days) costs 899 credits, leaving 101 of the
1,000 free monthly credits for retries and failures.

Jitter exists because ActiveSG classes start on :00 and :30 - a perfectly
aligned grid would alias against the class schedule and bias every average.
Slots are clamped so jitter never escapes the open window.

    python3 schedule.py            # today's slots + monthly cost
    python3 schedule.py --seed 7   # reproducible slots
"""

from __future__ import annotations

import argparse
import datetime as dt
import random
from zoneinfo import ZoneInfo

SGT = ZoneInfo("Asia/Singapore")
OPEN_FROM = dt.time(7, 0)
OPEN_TO = dt.time(21, 0)
INTERVAL_MIN = 30
JITTER_MIN = 5
FREE_CREDITS_PER_MONTH = 1000


def _at(day: dt.date, t: dt.time) -> dt.datetime:
    return dt.datetime.combine(day, t, tzinfo=SGT)


def slots(day: dt.date, seed: int | None = None) -> list[dt.datetime]:
    """Jittered poll times for one day, always inside [OPEN_FROM, OPEN_TO]."""
    rnd = random.Random(seed)
    start, end = _at(day, OPEN_FROM), _at(day, OPEN_TO)
    out: list[dt.datetime] = []
    cursor = start
    while cursor <= end:
        offset = rnd.randint(-JITTER_MIN, JITTER_MIN) if JITTER_MIN else 0
        when = cursor + dt.timedelta(minutes=offset)
        when = min(max(when, start), end)
        out.append(when)
        cursor += dt.timedelta(minutes=INTERVAL_MIN)
    return out


def per_day() -> int:
    start, end = _at(dt.date(2026, 1, 1), OPEN_FROM), _at(dt.date(2026, 1, 1), OPEN_TO)
    n, cursor = 0, start
    while cursor <= end:
        n += 1
        cursor += dt.timedelta(minutes=INTERVAL_MIN)
    return n


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=None)
    ap.add_argument("--day", type=dt.date.fromisoformat, default=None)
    args = ap.parse_args()

    day = args.day or dt.datetime.now(SGT).date()
    todays = slots(day, args.seed)
    daily = per_day()

    print(f"window   : {OPEN_FROM:%H:%M}-{OPEN_TO:%H:%M} SGT, every {INTERVAL_MIN} min "
          f"(+/-{JITTER_MIN} min jitter)")
    print(f"per day  : {daily} polls")
    print(f"monthly  : " + ", ".join(
        f"{d}d={daily * d} ({FREE_CREDITS_PER_MONTH - daily * d} left)"
        for d in (28, 30, 31)))
    print(f"\n{day:%a %d %b %Y} slots ({len(todays)}):")
    print("  " + "  ".join(f"{t:%H:%M}" for t in todays))


if __name__ == "__main__":
    main()
