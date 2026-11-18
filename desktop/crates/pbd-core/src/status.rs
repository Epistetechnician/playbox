//! What the tray says. Pure text from states, so the wording is tested and the GUI only displays it.

use crate::supervisor::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Warn,
    Bad,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub severity: Severity,
    pub text: String,
}

pub fn describe(name: &str, state: &State) -> Line {
    let label = {
        let mut c = name.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };
    let (severity, text) = match state {
        State::Healthy => (Severity::Ok, format!("{label} running")),
        State::Attached => (Severity::Ok, format!("{label} running (started elsewhere)")),
        State::Starting => (Severity::Warn, format!("{label} starting…")),
        State::Backoff { attempt, retry_in } => (
            Severity::Warn,
            format!(
                "{label} restarting in {}s (attempt {attempt})",
                retry_in.as_secs().max(1)
            ),
        ),
        State::Failed { .. } => (Severity::Bad, format!("{label} is down: it keeps failing")),
        State::Stopped => (Severity::Warn, format!("{label} stopped")),
    };
    Line { severity, text }
}

/// The worst severity among the lines, for the tray icon.
pub fn overall(lines: &[Line]) -> Severity {
    lines
        .iter()
        .map(|l| l.severity)
        .max()
        .unwrap_or(Severity::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wording_matches_the_state() {
        assert_eq!(describe("hub", &State::Healthy).text, "Hub running");
        assert_eq!(describe("hub", &State::Attached).severity, Severity::Ok);
        let b = describe(
            "hub",
            &State::Backoff {
                attempt: 3,
                retry_in: Duration::from_secs(8),
            },
        );
        assert_eq!(b.text, "Hub restarting in 8s (attempt 3)");
        assert_eq!(b.severity, Severity::Warn);
        assert_eq!(
            describe("hub", &State::Failed { reason: "x".into() }).severity,
            Severity::Bad
        );
    }

    #[test]
    fn the_failure_text_never_leaks_the_raw_reason() {
        // reasons can carry paths or output; the tray shows a plain sentence and the log keeps the detail
        let t = describe(
            "hub",
            &State::Failed {
                reason: "/Users/a/secret path".into(),
            },
        )
        .text;
        assert!(!t.contains("secret"));
    }

    #[test]
    fn overall_is_the_worst_line() {
        let l = [
            describe("hub", &State::Healthy),
            describe(
                "tunnel",
                &State::Failed {
                    reason: String::new(),
                },
            ),
        ];
        assert_eq!(overall(&l), Severity::Bad);
        assert_eq!(overall(&[]), Severity::Ok);
    }
}
