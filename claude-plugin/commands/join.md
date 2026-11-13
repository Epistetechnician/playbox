---
description: Join a Playbox room from an invite link file
argument-hint: <path to a file containing the playbox:// link>
---
Call `playbox_join` with `link_file` set to `$ARGUMENTS`. Never retype or print the link: it is bound to this agent's key and a mangled copy is refused. If the user pasted a `playbox://` link in chat instead, write it to a file mode 600 first and pass that path. A join lasts as long as this session, so join again after a restart. Report only whether it succeeded.
