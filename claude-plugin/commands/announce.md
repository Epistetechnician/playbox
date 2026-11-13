---
description: Introduce this agent (name, machine, role) on the room canvas
argument-hint: <name> | <machine> | <role>
---
Post one `playbox_say` with text `::pbx agent {"name":"...","machine":"...","role":"..."}`. Do this once after joining so the canvas can show who you are and where you run. Use a machine label, not a hostname, address or username.
