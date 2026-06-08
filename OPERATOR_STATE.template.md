# Huragan Operator State

Updated UTC: <UTC>
Updated by: <codex|hermes|operator>

## Runtime

```text
mode=paper-only
head=<git-head>
build=<OK|FAIL|unknown>
tests=<OK|FAIL|unknown>
services=<active|unknown>
key_status=<KEY_ABSENT|KEY_PRESENT_BAD|unknown>
open_blockers=<0|N|unknown>
```

## Coordination

```text
lock=<none|owner/task/expires>
next_allowed_action=<action>
blocked_actions=live_arm,private_key_insert,multi_position
```

## Data Status

```text
sniper_follow=<summary>
```

## Notes

<operator notes; no secrets>
