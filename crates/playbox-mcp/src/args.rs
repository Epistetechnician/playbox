//! Argument parsing and result shaping shared by the tool handlers.

use anyhow::{anyhow, bail, Result};
use playbox_core::memory::Kind;
use playbox_net::Transport;
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn parse_kind(k: &str) -> Result<Kind> {
    Ok(match k {
        "fact" => Kind::Fact,
        "decision" => Kind::Decision,
        "note" => Kind::Note,
        "skill" => Kind::Skill,
        o => bail!("unknown memory kind {o:?}"),
    })
}

pub(crate) fn s_arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

pub(crate) fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

pub(crate) fn req<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    s(a, k).ok_or_else(|| anyhow!("missing required argument `{k}`"))
}

pub(crate) fn b(a: &Value, k: &str) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Room limits from `playbox_create` arguments, the same ones `playbox up` takes. Anything absent or not a positive integer
/// keeps its default.
pub(crate) fn abuse_from_args(a: &Value) -> playbox_net::limits::AbuseLimits {
    let num = |k: &str| a.get(k).and_then(Value::as_u64).filter(|v| *v > 0);
    let mut l = playbox_net::limits::AbuseLimits::default();
    if let Some(v) = num("max_members") {
        l.max_members = usize::try_from(v).unwrap_or(usize::MAX);
    }
    if let Some(v) = num("max_concurrent") {
        l.max_concurrent_per_member = usize::try_from(v).unwrap_or(usize::MAX);
    }
    if let Some(v) = num("max_tasks_per_minute") {
        l.max_tasks_per_window = usize::try_from(v).unwrap_or(usize::MAX);
    }
    if let Some(v) = num("task_budget") {
        l.task_budget = v;
    }
    l
}

/// Latent-message policy from `playbox_create` arguments, the same ones `playbox up` takes.
pub(crate) fn latent_from_args(a: &Value) -> playbox_net::latent::LatentPolicy {
    let num = |k: &str| a.get(k).and_then(Value::as_u64).filter(|v| *v > 0);
    let mut l = playbox_net::latent::LatentPolicy::default();
    let hubs: Vec<String> = a
        .get("latent_hubs")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !hubs.is_empty() {
        l.hubs = Some(hubs);
    }
    if let Some(v) = num("latent_posts_per_minute") {
        l.posts_per_window = usize::try_from(v).unwrap_or(usize::MAX);
    }
    if let Some(v) = num("latent_max_age_secs") {
        l.max_age = Duration::from_secs(v);
    }
    l
}

pub(crate) fn transport_arg(a: &Value) -> Result<Option<Transport>> {
    if let Some(t) = s(a, "transport") {
        return Ok(Some(Transport::parse(t)?));
    }
    Ok(b(a, "local").then_some(Transport::Local))
}

pub(crate) fn ok_text(v: &Value) -> Value {
    json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default()}], "isError": false})
}

pub(crate) fn err_text(msg: &str) -> Value {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}
