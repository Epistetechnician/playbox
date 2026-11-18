//! Native OS notifications when a room needs the owner (witness holds, capability approvals, join knocks, or unread messages while hidden).
//!
//! All text from rooms or peers travels strictly in environment variables (`macos`, `windows`) or discrete `argv` elements (`linux`),
//! never interpolated into shell or script syntax.

use pbd_core::watch::AttentionItem;
use std::process::{Command, Stdio};

pub fn send(item: &AttentionItem) {
    let title: String = item
        .title
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let body: String = item
        .body
        .chars()
        .filter(|c| !c.is_control())
        .take(180)
        .collect();
    let title = if title.is_empty() {
        "Playbox".to_string()
    } else {
        title
    };
    dispatch(&title, &body);
}

#[cfg(target_os = "macos")]
fn dispatch(title: &str, body: &str) {
    const JXA: &str = r#"
ObjC.import("Foundation");
var env = $.NSProcessInfo.processInfo.environment;
var t = env.objectForKey("PB_NOTIFY_TITLE");
var b = env.objectForKey("PB_NOTIFY_BODY");
var title = (t && !t.isNil()) ? ObjC.unwrap(t) : "Playbox";
var body = (b && !b.isNil()) ? ObjC.unwrap(b) : "";
var app = Application.currentApplication();
app.includeStandardAdditions = true;
app.displayNotification(body, {withTitle: "Playbox", subtitle: title});
"#;
    let _ = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", JXA])
        .env("PB_NOTIFY_TITLE", title)
        .env("PB_NOTIFY_BODY", body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(target_os = "linux")]
fn dispatch(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=Playbox", "--", title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn dispatch(_title: &str, _body: &str) {}
