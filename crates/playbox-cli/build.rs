//! Puts the commit and date into `playbox --version`, so a bug report says exactly which build it came from.
use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git").args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
    };
    let commit = std::env::var("PLAYBOX_COMMIT").ok().or_else(|| git(&["rev-parse", "--short=9", "HEAD"])).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    let long = format!("{} ({}{})", std::env::var("CARGO_PKG_VERSION").unwrap_or_default(), commit, if dirty { "+dirty" } else { "" });
    println!("cargo:rustc-env=PLAYBOX_VERSION_LONG={long}");
    println!("cargo:rerun-if-env-changed=PLAYBOX_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
