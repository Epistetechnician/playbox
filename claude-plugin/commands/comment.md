---
description: Leave a comment anchored to a note, artifact or timeline event
argument-hint: <note:ID | artifact:ID | event:SEQ> <text>
---
Post one `playbox_say` with text `::pbx comment {"on":"note:<id>","text":"..."}`. `on` is `note:<id>` or `artifact:<id>` (the id you gave the artifact) or `event:<seq>` (a timeline event's sequence number). Text is plain, up to 600 characters. Comments left by people in the room's Notes and Timeline tabs arrive in your feed as the same `::pbx comment` messages: read the `on` anchor to see what they point at, then answer with another comment on the same anchor.
