//! In-process MCP sidecar: `call`, `shell`, `mcp` and the memory commands all drive the same tools an agent sees.

use crate::cli::{MemoryCmd, ShellArgs};
use anyhow::{anyhow, bail, Result};
use playbox_core::store;
use playbox_mcp::Server;
use serde_json::{json, Map, Value};
use std::io::{IsTerminal, Write};

fn full_name(tool: &str) -> String {
    if tool.starts_with("playbox_") { tool.to_string() } else { format!("playbox_{tool}") }
}

/// `key=value` pairs (value is JSON when it parses, else text), merged over an optional JSON object.
pub fn parse_args(pairs: &[String], json: Option<&str>) -> Result<Value> {
    let mut obj = match json {
        Some(j) => match serde_json::from_str::<Value>(j)? {
            Value::Object(m) => m,
            _ => bail!("--json takes an object"),
        },
        None => Map::new(),
    };
    for p in pairs {
        let (k, v) = p.split_once('=').ok_or_else(|| anyhow!("expected key=value, got {p:?}"))?;
        // `key=@file` reads the value from a file (capabilities and receipts are multi-line).
        let v = match v.strip_prefix('@') {
            Some(path) => std::fs::read_to_string(path).map_err(|e| anyhow!("{path}: {e}"))?.trim_end().to_string(),
            None => v.to_string(),
        };
        obj.insert(k.to_string(), serde_json::from_str(&v).unwrap_or(Value::String(v)));
    }
    Ok(Value::Object(obj))
}

/// Split a line into words, honouring single and double quotes.
fn words(line: &str) -> Result<Vec<String>> {
    let (mut out, mut cur, mut quote, mut any) = (Vec::new(), String::new(), None::<char>, false);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        bail!("unterminated quote");
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// Run one tool and print its text. Returns whether it succeeded.
async fn run(server: &mut Server, tool: &str, args: &Value) -> bool {
    let r = server.call(&full_name(tool), args).await;
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    let ok = !r["isError"].as_bool().unwrap_or(false);
    if ok { println!("{text}") } else { eprintln!("error: {text}") }
    ok
}

fn server() -> Result<Server> {
    Server::new(&store::home())
}

pub async fn call(tool: &str, pairs: &[String], json: Option<&str>) -> Result<()> {
    let mut s = server()?;
    if run(&mut s, tool, &parse_args(pairs, json)?).await { Ok(()) } else { std::process::exit(1) }
}

fn help() {
    println!("tools (key=value arguments; `help TOOL` for its arguments):");
    for t in playbox_mcp::tool_list().as_array().into_iter().flatten() {
        let name = t["name"].as_str().unwrap_or("").trim_start_matches("playbox_");
        let desc = t["description"].as_str().unwrap_or("");
        println!("  {name:<18} {}", desc.split(". ").next().unwrap_or(desc).trim_end_matches('.'));
    }
    println!("  help [TOOL]        this list\n  quit               leave (closes the room connection)");
}

fn help_tool(name: &str) {
    let n = full_name(name);
    match playbox_mcp::tool_list().as_array().and_then(|a| a.iter().find(|t| t["name"] == n.as_str())) {
        None => println!("no tool {name}"),
        Some(t) => {
            println!("{}", t["description"].as_str().unwrap_or(""));
            let req: Vec<&str> = t["inputSchema"]["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            for (k, v) in t["inputSchema"]["properties"].as_object().into_iter().flatten() {
                println!("  {k}{} ({}): {}", if req.contains(&k.as_str()) { " *" } else { "" }, v["type"].as_str().unwrap_or("?"), v["description"].as_str().unwrap_or(""));
            }
        }
    }
}

pub async fn shell(a: ShellArgs) -> Result<()> {
    let mut s = server()?;
    if let Some(link) = &a.link {
        let mut args = json!({"link": link, "trust_host_device": a.trust_host_device});
        if let Some(t) = &a.min_host_tier {
            args["min_host_tier"] = json!(t);
        }
        if let Some(t) = a.transport.transport.clone().or_else(|| a.transport.local.then(|| "local".to_string())) {
            args["transport"] = json!(t);
        }
        if !run(&mut s, "join", &args).await {
            bail!("could not join");
        }
    }
    let tty = std::io::stdin().is_terminal();
    if tty {
        eprintln!("playbox shell: `help` lists commands. History log: {}", s.log_path().display());
    }
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin());
    let mut buf = String::new();
    loop {
        if tty {
            eprint!("playbox> ");
            std::io::stderr().flush().ok();
        }
        buf.clear();
        if tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut buf).await? == 0 {
            return Ok(());
        }
        let w = match words(buf.trim()) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("error: {e}");
                continue;
            }
        };
        match w.first().map(String::as_str) {
            None => {}
            Some("quit" | "exit") => return Ok(()),
            Some("help" | "?") => match w.get(1) {
                Some(t) => help_tool(t),
                None => help(),
            },
            Some(tool) => match parse_args(&w[1..], None) {
                Ok(args) => {
                    run(&mut s, tool, &args).await;
                }
                Err(e) => eprintln!("error: {e}"),
            },
        }
    }
}

pub async fn mcp() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut server = server()?;
    eprintln!("playbox mcp {} ready; history log: {}", env!("CARGO_PKG_VERSION"), server.log_path().display());
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(msg).await,
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

pub async fn memory(cmd: MemoryCmd) -> Result<()> {
    match cmd {
        MemoryCmd::Remember { kind, topic, body, evidence, supersedes } => {
            let mut a = json!({"kind": kind, "topic": topic, "body": body, "evidence": evidence});
            if let Some(s) = supersedes {
                a["supersedes"] = json!(s);
            }
            call("remember", &[], Some(&a.to_string())).await
        }
        MemoryCmd::Recall { kind } => {
            let a = kind.map(|k| json!({"kind": k})).unwrap_or_else(|| json!({}));
            call("recall", &[], Some(&a.to_string())).await
        }
        MemoryCmd::Export => call("memory_export", &[], None).await,
        MemoryCmd::Ingest { events } => call("memory_ingest", &[], Some(&json!({"events": events}).to_string())).await,
        MemoryCmd::Review { id, accept, reject, key } => crate::trust::memory_review(&id, accept, reject, key.key),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_args, words};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn words_split_plain_words_on_whitespace() {
        assert_eq!(
            words("one two\tthree").unwrap(),
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
    }

    #[test]
    fn words_honor_single_and_double_quotes() {
        assert_eq!(
            words(r#"one "two words" 'three words'"#).unwrap(),
            vec![
                "one".to_string(),
                "two words".to_string(),
                "three words".to_string()
            ]
        );
    }

    #[test]
    fn words_keep_an_empty_quoted_word() {
        assert_eq!(
            words(r#"before "" after"#).unwrap(),
            vec!["before".to_string(), String::new(), "after".to_string()]
        );
    }

    #[test]
    fn words_reject_unterminated_quotes() {
        assert!(words(r#"before "unfinished"#).is_err());
    }

    #[test]
    fn parse_args_distinguish_json_values_from_text() {
        let pairs = [
            "count=42".to_string(),
            "enabled=true".to_string(),
            "items=[1,\"two\",false]".to_string(),
            "label=plain text".to_string(),
        ];

        assert_eq!(
            parse_args(&pairs, None).unwrap(),
            json!({
                "count": 42,
                "enabled": true,
                "items": [1, "two", false],
                "label": "plain text"
            })
        );
    }

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    struct TempFile(PathBuf);

    impl TempFile {
        fn with_contents(contents: &str) -> Self {
            let id = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "playbox-session-parse-args-{}-{id}.txt",
                std::process::id()
            ));
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn parse_args_read_at_file_values_and_trim_trailing_whitespace() {
        let file = TempFile::with_contents("first line\nsecond line\n");
        let pair = format!("value=@{}", file.0.display());

        assert_eq!(
            parse_args(&[pair], None).unwrap()["value"],
            "first line\nsecond line"
        );
    }

    #[test]
    fn parse_args_reject_pairs_without_equals() {
        assert!(parse_args(&["missing-value".to_string()], None).is_err());
    }

    #[test]
    fn parse_args_merge_json_object_with_pairs_taking_precedence() {
        let pairs = ["mode=pair".to_string(), "enabled=false".to_string()];

        assert_eq!(
            parse_args(&pairs, Some(r#"{"mode":"json","kept":7}"#)).unwrap(),
            json!({"mode": "pair", "kept": 7, "enabled": false})
        );
    }
}
