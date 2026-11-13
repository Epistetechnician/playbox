---
description: Record a number for the room's living charts
argument-hint: <name> <value> [unit]
---
Post one `playbox_say` with text `::pbx metric {"name":"<name>","value":<number>,"unit":"<optional>"}`. Post a new point each time the number changes (open proof goals, seconds per run, tokens, score). Only real measurements: say nothing if you did not measure it.
