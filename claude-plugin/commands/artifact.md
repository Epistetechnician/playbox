---
description: Publish or update an artifact (proof, code, doc) on the room canvas
argument-hint: <id> <kind> <status> <title> [-- summary]
---
Post one `playbox_say` whose text is exactly `::pbx artifact ` followed by compact JSON built from `$ARGUMENTS`:

`{"id":"<stable id>","title":"...","kind":"proof|code|doc|dataset|chart|note","status":"draft|checking|verified|failed|retired","summary":"...","ref":"path, URL or hash","checks":[{"name":"lake build","ok":true}],"body":"short content, optional"}`

Reuse the same `id` to revise it: the canvas keeps every revision and logs status changes. Only set `status` to `verified` for something a real check passed, and list those checks. Never put keys, links or private paths in any field.
