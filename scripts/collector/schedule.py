#!/usr/bin/env python3
"""ActiveSG poll schedule optimiser.

One scrape returns all 56 facilities (pools + gyms in a single payload), so cost
is per poll, never per facility or per type.

Goal: densest cadence that still fits a monthly credit budget, inside opening
hours only (gyms 07:00-22:00, pools 08:00-21:30). Below 08:00 only gyms are
open, so the default window starts there.

    python3 schedule.py --optimal                  # 1 free token
    python3 schedule.py --optimal --budget 2000    # 2 free tokens
    python3 schedule.py --frontier                 # full interval table
    python3 schedule.py --chart out.png            # plot both budgets
"""

from __future__ import annotations

import argparse
import calendar
import datetime as dt
import random
from zoneinfo import ZoneInfo

SGT = ZoneInfo("Asia/Singapore")
OPEN_FROM = dt.time(8, 0)
OPEN_TO = dt.time(21, 0)
JITTER_MIN = 3
CREDITS_PER_TOKEN = 1000
DAYS_PER_MONTH = 365 / 12

# A healthy reserve covers failed scrapes (a failure still costs a credit),
# manual re-runs while debugging, and months with 31 days.
RESERVE_RATIO = 0.08


def _at(day: dt.date, t: dt.time) -> dt.datetime:
    return dt.datetime.combine(day, t, tzinfo=SGT)


def polls_per_day(interval: int, open_from: dt.time, open_to: dt.time) -> int:
    start, end = _at(dt.date(2026, 1, 1), open_from), _at(dt.date(2026, 1, 1), open_to)
    return int((end - start).total_seconds() // 60) // interval + 1


def open_hours(open_from: dt.time, open_to: dt.time) -> int:
    return ((open_to.hour * 60 + open_to.minute) - (open_from.hour * 60 + open_from.minute)) // 60


def monthly(polls: int) -> tuple[int, int]:
    """(30-day month, 31-day month) credit cost."""
    return polls * 30, polls * 31


def cells_per_month(polls: int, hours: int) -> float:
    """Samples each (weekday, hour) heatmap cell collects per month."""
    return (DAYS_PER_MONTH / 7) * (polls / hours)


def optimal(budget: int, open_from: dt.time, open_to: dt.time) -> dict:
    """Densest interval whose worst month stays under budget minus reserve."""
    cap = int(budget * (1 - RESERVE_RATIO))
    hours = open_hours(open_from, open_to)
    for interval in range(5, 61):
        polls = polls_per_day(interval, open_from, open_to)
        _, worst = monthly(polls)
        if worst <= cap:
            return {
                "interval": interval,
                "polls": polls,
                "worst": worst,
                "spare": budget - worst,
                "cells": cells_per_month(polls, hours),
                "staleness": interval + JITTER_MIN,
                "hours": hours,
                "cap": cap,
            }
    raise SystemExit("no interval fits this budget")


def daily_slots(day: dt.date, interval: int, open_from: dt.time,
                open_to: dt.time, seed: int | None = None) -> list[dt.datetime]:
    """Jittered poll times, clamped inside the open window."""
    rnd = random.Random(seed)
    start, end = _at(day, open_from), _at(day, open_to)
    out, cursor = [], start
    while cursor <= end:
        off = rnd.randint(-JITTER_MIN, JITTER_MIN) if JITTER_MIN else 0
        out.append(min(max(cursor + dt.timedelta(minutes=off), start), end))
        cursor += dt.timedelta(minutes=interval)
    return out


def show(budget: int, open_from: dt.time, open_to: dt.time, detail: bool) -> dict:
    o = optimal(budget, open_from, open_to)
    tokens = budget / CREDITS_PER_TOKEN
    print(f"\n=== {tokens:g} token(s) = {budget} credits/month ===")
    print(f"window     : {open_from:%H:%M}-{open_to:%H:%M} SGT ({o['hours']} open hours)")
    print(f"interval   : every {o['interval']} min (+/-{JITTER_MIN} min jitter)")
    print(f"polls/day  : {o['polls']}")
    print(f"worst month: {o['worst']} credits, {o['spare']} spare "
          f"({o['spare'] / budget:.0%})")
    print(f"staleness  : up to {o['staleness']} min")
    print(f"per cell   : {o['cells']:.1f} samples per (weekday x hour) per month")
    if detail:
        day = dt.datetime.now(SGT).date()
        times = daily_slots(day, o["interval"], open_from, open_to, seed=1)
        print(f"sample day : {len(times)} slots ->")
        print("  " + "  ".join(f"{t:%H:%M}" for t in times))
    return o


def chart(path: str, open_from: dt.time, open_to: dt.time) -> None:
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print("matplotlib not installed - skipping chart (pip install matplotlib)")
        return

    intervals = list(range(8, 41))
    budgets = [(1000, "1 token - 1,000 credits"), (2000, "2 tokens - 2,000 credits")]
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.8))
    colours = ["#1f4fd8", "#d81f4f"]

    for (budget, label), colour in zip(budgets, colours):
        polls = [polls_per_day(i, open_from, open_to) for i in intervals]
        worst = [p * 31 for p in polls]
        cells = [cells_per_month(p, open_hours(open_from, open_to)) for p in polls]
        o = optimal(budget, open_from, open_to)

        axes[0].plot(intervals, worst, colour, label=label, linewidth=2)
        axes[0].axhline(budget, linestyle="--", color=colour, alpha=0.45, linewidth=1)
        axes[0].plot(o["interval"], o["worst"], "o", color=colour, markersize=8)
        axes[0].annotate(f"{o['interval']} min\n{o['polls']}/day",
                         (o["interval"], o["worst"]), textcoords="offset points",
                         xytext=(6, 8), fontsize=8, color=colour)

        axes[1].plot(intervals, cells, colour, label=label, linewidth=2)
        axes[1].plot(o["interval"], o["cells"], "o", color=colour, markersize=8)

    axes[0].set_title("Monthly credits vs polling interval")
    axes[0].set_xlabel("interval (minutes)")
    axes[0].set_ylabel("credits in a 31-day month")
    axes[0].legend(fontsize=8)
    axes[0].grid(alpha=0.25)

    axes[1].set_title("Heatmap confidence vs polling interval")
    axes[1].set_xlabel("interval (minutes)")
    axes[1].set_ylabel("samples per (weekday x hour) per month")
    axes[1].legend(fontsize=8)
    axes[1].grid(alpha=0.25)

    for ax in axes:
        ax.invert_xaxis()
    fig.suptitle(f"ActiveSG collector - window {open_from:%H:%M}-{open_to:%H:%M} SGT",
                 fontsize=11)
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    print(f"\nchart written: {path}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--budget", type=int, default=CREDITS_PER_TOKEN)
    ap.add_argument("--tokens", type=float, default=None,
                    help="shortcut: --tokens 2 means 2,000 credits")
    ap.add_argument("--from", dest="open_from", type=dt.time.fromisoformat, default=OPEN_FROM)
    ap.add_argument("--to", dest="open_to", type=dt.time.fromisoformat, default=OPEN_TO)
    ap.add_argument("--optimal", action="store_true")
    ap.add_argument("--frontier", action="store_true")
    ap.add_argument("--chart", metavar="PNG")
    args = ap.parse_args()

    budget = int(args.tokens * CREDITS_PER_TOKEN) if args.tokens else args.budget

    if args.chart:
        chart(args.chart, args.open_from, args.open_to)
        return

    if args.frontier:
        print(f"budget {budget}, window {args.open_from:%H:%M}-{args.open_to:%H:%M} SGT")
        print(f"{'interval':>9} {'polls/day':>10} {'31d':>6} {'cells/cell':>11}  verdict")
        for i in range(8, 41):
            p = polls_per_day(i, args.open_from, args.open_to)
            worst = p * 31
            spare = budget - worst
            verdict = ("fits" if spare >= budget * RESERVE_RATIO
                       else "tight" if spare >= 0 else "over")
            cells = cells_per_month(p, open_hours(args.open_from, args.open_to))
            print(f"{i:>7}min {p:>10} {worst:>6} {cells:>11.1f}  {verdict}")
        return

    show(CREDITS_PER_TOKEN, args.open_from, args.open_to, detail=True)
    show(2 * CREDITS_PER_TOKEN, args.open_from, args.open_to, detail=False)
    if budget not in (CREDITS_PER_TOKEN, 2 * CREDITS_PER_TOKEN):
        show(budget, args.open_from, args.open_to, detail=False)


if __name__ == "__main__":
    main()
