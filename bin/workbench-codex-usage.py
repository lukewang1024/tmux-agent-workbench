#!/usr/bin/env python3
"""Offline daily Codex totals when the optional ccusage CLI is unavailable."""
import json
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path


def daily_usage(home):
    totals = defaultdict(int)
    for path in sorted((Path(home) / "sessions").rglob("*.jsonl")):
        previous = 0
        try:
            with path.open() as stream:
                for line in stream:
                    try:
                        event = json.loads(line)
                        payload = event.get("payload", {})
                        if event.get("type") != "event_msg" or payload.get("type") != "token_count":
                            continue
                        usage = (payload.get("info") or {}).get("total_token_usage") or {}
                        current = usage.get("total_tokens")
                        if not isinstance(current, int) or current < 0:
                            continue
                        day = datetime.fromisoformat(event["timestamp"].replace("Z", "+00:00")).astimezone().date().isoformat()
                        # Repeated quota events can carry the same cumulative total.
                        totals[day] += max(0, current - previous)
                        previous = current
                    except (ValueError, KeyError, TypeError, AttributeError):
                        # Active sessions can end with an incomplete JSON line.
                        continue
        except OSError:
            continue
    return {"daily": [{"date": day, "totalTokens": tokens} for day, tokens in sorted(totals.items())]}


if __name__ == "__main__":
    print(json.dumps(daily_usage(sys.argv[1])))
