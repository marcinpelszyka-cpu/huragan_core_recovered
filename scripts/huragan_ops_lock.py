#!/usr/bin/env python3
"""Huragan operator handoff lock.

Runtime coordination only. This script never reads or prints secrets and never
changes trading config. It manages `.ops_lock.json` and `OPERATOR_STATE.md`.
"""
import argparse
import json
import os
import socket
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOCK_PATH = ROOT / ".ops_lock.json"
STATE_PATH = ROOT / "OPERATOR_STATE.md"
VALID_OWNERS = {"codex", "hermes", "operator"}


def now():
    return datetime.now(timezone.utc)


def iso(dt):
    return dt.astimezone(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def parse_iso(s):
    if not s:
        return None
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00"))
    except Exception:
        return None


def read_lock():
    if not LOCK_PATH.exists():
        return None
    try:
        return json.loads(LOCK_PATH.read_text())
    except Exception as e:
        raise SystemExit(f"bad lock file: {e}")


def lock_expired(lock):
    exp = parse_iso(lock.get("expires_at")) if lock else None
    return bool(exp and exp <= now())


def write_lock(lock):
    tmp = LOCK_PATH.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(lock, indent=2, sort_keys=True) + "\n")
    os.chmod(tmp, 0o600)
    tmp.replace(LOCK_PATH)


def owner_required(owner):
    if owner not in VALID_OWNERS:
        raise SystemExit(f"invalid owner={owner}; expected one of {sorted(VALID_OWNERS)}")


def cmd_status(_args):
    lock = read_lock()
    if not lock:
        print(json.dumps({"locked": False, "path": str(LOCK_PATH)}, indent=2, sort_keys=True))
        return 0
    out = dict(lock)
    out["locked"] = not lock_expired(lock)
    out["expired"] = lock_expired(lock)
    out["path"] = str(LOCK_PATH)
    print(json.dumps(out, indent=2, sort_keys=True))
    return 0 if out["locked"] else 2


def cmd_acquire(args):
    owner_required(args.owner)
    existing = read_lock()
    if existing and not lock_expired(existing):
        if existing.get("owner") != args.owner:
            print(json.dumps({"acquired": False, "reason": "locked_by_other_owner", "lock": existing}, indent=2, sort_keys=True))
            return 3
        if not args.renew:
            print(json.dumps({"acquired": False, "reason": "already_locked_by_owner", "lock": existing}, indent=2, sort_keys=True))
            return 4
    if existing and lock_expired(existing) and not args.force_expired:
        print(json.dumps({"acquired": False, "reason": "expired_lock_requires_force_expired", "lock": existing}, indent=2, sort_keys=True))
        return 5

    started = now()
    lock = {
        "owner": args.owner,
        "task": args.task,
        "started_at": iso(started),
        "expires_at": iso(started + timedelta(minutes=args.ttl_min)),
        "ttl_min": args.ttl_min,
        "host": socket.gethostname(),
        "pid": os.getpid(),
        "force_expired": bool(args.force_expired),
        "allowed_actions": args.allowed_action or [],
        "forbidden_actions": args.forbidden_action or [],
    }
    write_lock(lock)
    print(json.dumps({"acquired": True, "lock": lock}, indent=2, sort_keys=True))
    return 0


def cmd_release(args):
    owner_required(args.owner)
    lock = read_lock()
    if not lock:
        print(json.dumps({"released": False, "reason": "no_lock"}, indent=2, sort_keys=True))
        return 0
    if lock.get("owner") != args.owner and not args.force:
        print(json.dumps({"released": False, "reason": "owner_mismatch", "lock_owner": lock.get("owner")}, indent=2, sort_keys=True))
        return 3
    LOCK_PATH.unlink(missing_ok=True)
    print(json.dumps({"released": True, "owner": args.owner}, indent=2, sort_keys=True))
    return 0


def cmd_write_state(args):
    owner_required(args.owner)
    lock = read_lock()
    lock_summary = "none"
    if lock:
        lock_summary = f"owner={lock.get('owner')} task={lock.get('task')} expires_at={lock.get('expires_at')} expired={lock_expired(lock)}"
    body = f"""# Huragan Operator State

Updated UTC: {iso(now())}
Updated by: {args.owner}

## Runtime

```text
mode={args.mode}
head={args.head}
build={args.build}
tests={args.tests}
services={args.services}
key_status={args.key_status}
open_blockers={args.open_blockers}
```

## Coordination

```text
lock={lock_summary}
next_allowed_action={args.next_action}
blocked_actions={args.blocked_actions}
```

## Data Status

```text
{args.data_status}
```

## Notes

{args.notes}
"""
    tmp = STATE_PATH.with_suffix(".md.tmp")
    tmp.write_text(body)
    os.chmod(tmp, 0o600)
    tmp.replace(STATE_PATH)
    print(json.dumps({"written": True, "path": str(STATE_PATH), "next_allowed_action": args.next_action}, indent=2, sort_keys=True))
    return 0


def main():
    ap = argparse.ArgumentParser(description="Huragan operator handoff lock")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("status").set_defaults(func=cmd_status)

    acq = sub.add_parser("acquire")
    acq.add_argument("--owner", required=True)
    acq.add_argument("--task", required=True)
    acq.add_argument("--ttl-min", type=int, default=30)
    acq.add_argument("--allowed-action", action="append", default=[])
    acq.add_argument("--forbidden-action", action="append", default=[])
    acq.add_argument("--force-expired", action="store_true")
    acq.add_argument("--renew", action="store_true")
    acq.set_defaults(func=cmd_acquire)

    rel = sub.add_parser("release")
    rel.add_argument("--owner", required=True)
    rel.add_argument("--force", action="store_true")
    rel.set_defaults(func=cmd_release)

    ws = sub.add_parser("write-state")
    ws.add_argument("--owner", required=True)
    ws.add_argument("--next-action", required=True)
    ws.add_argument("--mode", default="paper-only")
    ws.add_argument("--head", default="unknown")
    ws.add_argument("--build", default="unknown")
    ws.add_argument("--tests", default="unknown")
    ws.add_argument("--services", default="unknown")
    ws.add_argument("--key-status", default="unknown")
    ws.add_argument("--open-blockers", default="unknown")
    ws.add_argument("--blocked-actions", default="live_arm,private_key_insert,multi_position")
    ws.add_argument("--data-status", default="not updated")
    ws.add_argument("--notes", default="")
    ws.set_defaults(func=cmd_write_state)

    args = ap.parse_args()
    raise SystemExit(args.func(args))


if __name__ == "__main__":
    main()
