---
description: Add or move a task on the room's work board
argument-hint: <id> <todo|doing|review|done> <title>
---
Post one `playbox_say` with text `::pbx card ` plus JSON: `{"id":"<stable id>","title":"...","col":"todo|doing|review|done","owner":"<your name>","artifact":"<artifact id, optional>","note":"optional"}`. Reuse the id to move it. Move your own tasks as you work: `doing` when you start, `review` when you want eyes on it, `done` only when it is.
