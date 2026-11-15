//! Request generators and the oracle that judges hub responses during fuzzing.

use super::http::{h, Resp};

/// A fixed request, and what a correct hub's answer to it looks like.
pub(super) type Case = (&'static str, Req, Box<dyn Fn(&Resp) -> bool>);

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15); // splitmix64: nearby seeds must not give nearby streams
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn pick<T: Copy>(&mut self, s: &[T]) -> T {
        s[self.below(s.len())]
    }
    pub fn pct(&mut self, p: usize) -> bool {
        self.below(100) < p
    }
}

pub(super) const ROUTES: &[&str] = &[
    "/",
    "/pair",
    "/join",
    "/i/{id}",
    "/icon.svg",
    "/manifest.webmanifest",
    "/sw.js",
    "/auth.js",
    "/api/me",
    "/api/rooms",
    "/api/rooms/{id}",
    "/api/rooms/{id}/invite",
    "/api/rooms/{id}/grants",
    "/api/rooms/{id}/agent-tokens",
    "/api/invites",
    "/api/invites/{id}",
    "/api/knocks",
    "/api/knocks/{id}/approve",
    "/api/knocks/{id}/deny",
    "/api/knock",
    "/api/knock/out",
    "/api/knock/out/{id}",
    "/api/agents",
    "/api/agents/{id}/connect",
    "/api/integrations",
    "/api/integrations/key",
    "/api/integrations/connect",
    "/api/integrations/toolkits",
    "/api/integrations/accounts/{id}",
    "/api/grants/{id}",
    "/api/agent-tokens/{id}",
    "/api/devices",
    "/api/devices/pair",
    "/api/devices/{id}",
    "/api/sessions/{id}",
    "/api/push/subscribe",
    "/api/push/unsubscribe",
    "/api/attention",
    "/api/system",
    "/api/system/verify",
    "/r/{id}/api/state",
    "/r/{id}/api/tool",
    "/r/{id}/",
    "/x/grants",
    "/x/tools",
    "/x/execute",
    "/api/auth/state",
    "/api/auth/weak",
    "/api/auth/register/begin",
    "/api/auth/register/finish",
    "/api/auth/login/begin",
    "/api/auth/login/finish",
    "/api/auth/stepup/begin",
    "/api/auth/stepup/finish",
    "/api/auth/logout",
    "/api/invite/{id}",
    "/api/invite/{id}/knock",
    "/api/invite/{id}/poll",
];
pub(super) const IDS: &[&str] = &[
    "a",
    "abc123abc123",
    "0",
    "zzzzzzzzzzzz",
    "..",
    "%2e%2e",
    "é",
    "default",
    "00000000000000000000000000000000",
    "x",
];
pub(super) const SUFFIX: &[&str] = &[
    "",
    "",
    "",
    "/",
    "//",
    "/..",
    "/%00",
    "?a=b",
    "?q=%ff",
    "?since=-1",
    "?toolkit=../x",
    "/.",
    "/%2e%2e/%2e%2e",
    ";x",
    "?q=\u{1F600}",
];
pub(super) const KEYS: &[&str] = &[
    "label",
    "machine",
    "kind",
    "repo",
    "key",
    "toolkit",
    "tool",
    "account",
    "level",
    "token",
    "code",
    "name",
    "text",
    "arguments",
    "ttl",
    "uses",
    "approve",
    "guest",
    "id",
    "link",
    "ticket",
    "response",
    "endpoint",
    "keys",
    "invite_ttl_secs",
    "local",
];
pub(super) const STRS: &[&str] = &[
    "",
    "A",
    "../../../etc/passwd",
    "\\u0000",
    "'; DROP TABLE x;--",
    "<script>alert(1)</script>",
    "${jndi:ldap://x/a}",
    "%s%s%s%n",
    "\u{1F600}",
    "-1",
    "NaN",
    "null",
    "read",
    "write",
    "admin",
    "gmail",
    "GMAIL_SEND_EMAIL",
    "http://169.254.169.254/",
    "file:///etc/passwd",
];
pub(super) const CTYPES: &[&str] = &[
    "application/json",
    "application/json",
    "application/json; charset=utf-8",
    "text/plain",
    "",
    "multipart/form-data; boundary=x",
    "application/x-www-form-urlencoded",
];

pub(super) fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub(super) fn gen_json(r: &mut Rng, depth: usize) -> String {
    match r.below(if depth > 2 { 7 } else { 10 }) {
        0 => "null".into(),
        1 => "true".into(),
        2 => "false".into(),
        3 => r
            .pick(&[
                "0",
                "-1",
                "3.14",
                "1e999",
                "99999999999999999999999",
                "-0",
                "2147483648",
                "1e-400",
            ])
            .to_string(),
        4 | 5 => {
            let v = if r.pct(10) {
                "x".repeat(1 + r.below(5000))
            } else {
                (r.pick(STRS)).to_string()
            };
            json_str(&v)
        }
        6 => json_str(r.pick(KEYS)),
        7 => format!(
            "[{}]",
            (0..r.below(4))
                .map(|_| gen_json(r, depth + 1))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => format!(
            "{{{}}}",
            (0..r.below(5))
                .map(|_| format!("{}:{}", json_str(r.pick(KEYS)), gen_json(r, depth + 1)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

pub(super) fn gen_body(r: &mut Rng) -> Vec<u8> {
    match r.below(10) {
        0 | 1 => Vec::new(),
        2..=6 => format!(
            "{{{}}}",
            (0..1 + r.below(5))
                .map(|_| format!("{}:{}", json_str(r.pick(KEYS)), gen_json(r, 0)))
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes(),
        7 => {
            let n = r.pick(&[50usize, 500, 5000, 30000]);
            format!("{}{}", "[".repeat(n), "]".repeat(n)).into_bytes()
        }
        8 => (0..r.below(1500)).map(|_| r.below(256) as u8).collect(),
        _ => {
            let full = gen_json(r, 0);
            full.as_bytes()[..full.len() / 2].to_vec()
        }
    }
}

pub(super) fn encode_path(p: &str) -> String {
    let mut o = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/%?=&:;,+@!$'()*".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

pub struct Req {
    pub method: &'static str,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub cookie_valid: bool,
}

pub fn gen_request(r: &mut Rng, port: u16, cookie: Option<&str>) -> Req {
    let method = r.pick(&[
        "GET", "GET", "GET", "POST", "POST", "POST", "POST", "DELETE", "PATCH", "PUT", "HEAD",
        "OPTIONS",
    ]);
    let mut path = r.pick(ROUTES).replace("{id}", r.pick(IDS));
    path.push_str(r.pick(SUFFIX));
    let path = encode_path(&path);
    let body = if matches!(method, "POST" | "PATCH" | "PUT" | "DELETE") {
        gen_body(r)
    } else {
        Vec::new()
    };
    let mut hs = vec![h(
        "Host",
        &if r.pct(5) {
            "evil.example".to_string()
        } else {
            format!("127.0.0.1:{port}")
        },
    )];
    match r.below(4) {
        0 | 1 => {}
        2 => hs.push(h("Origin", &format!("http://127.0.0.1:{port}"))),
        _ => hs.push(h("Origin", "https://evil.example")),
    }
    if r.pct(10) {
        hs.push(h(
            "Sec-Fetch-Site",
            r.pick(&["cross-site", "same-site", "same-origin", "none"]),
        ));
    }
    let ct = r.pick(CTYPES);
    if !ct.is_empty() {
        hs.push(h("Content-Type", ct));
    }
    let mut valid = false;
    match (cookie, r.below(10)) {
        (Some(c), 0..=5) => {
            hs.push(h("Cookie", &format!("pbs={c}")));
            valid = true;
        }
        (_, 6) => hs.push(h("Cookie", &format!("pbs={:016x}", r.next()))),
        (_, 7) => hs.push(h("Cookie", "pbs=; ;;=")),
        _ => {}
    }
    if r.pct(10) {
        hs.push(h(
            "Authorization",
            &format!(
                "Bearer pbx_{:012x}.{:032x}",
                r.next() & 0xffff_ffff_ffff,
                r.next()
            ),
        ));
    }
    if r.pct(8) {
        hs.push(h(
            r.pick(&["X-Forwarded-For", "Cf-Connecting-Ip", "X-Real-Ip"]),
            &format!("203.0.113.{}", r.below(255)),
        ));
    }
    let cl = match r.below(40) {
        0 => "abc".to_string(),
        1 => "-5".to_string(),
        2 => "99999999999".to_string(),
        3 => (body.len() + 50).to_string(),
        _ => body.len().to_string(),
    };
    if method != "GET" && method != "HEAD" || !body.is_empty() {
        hs.push(h("Content-Length", &cl));
    }
    Req {
        method,
        path,
        headers: hs,
        body,
        cookie_valid: valid,
    }
}

/// The routes anyone may reach without signing in.
pub fn is_public(path: &str) -> bool {
    let p = path.split(['?', '#', ';']).next().unwrap_or(""); // the hub reads the path before any ;params
    let p = format!("/{}", p.trim_start_matches('/')); // and Python's server folds leading slashes into one
    matches!(
        p.as_str(),
        "/" | "/pair"
            | "/join"
            | "/icon.svg"
            | "/icon-192.png"
            | "/icon-512.png"
            | "/icon-maskable.png"
            | "/manifest.webmanifest"
            | "/auth.js"
            | "/space.js"
            | "/sw.js"
    ) || p.starts_with("/i/")
        || p.starts_with("/api/invite/")
        || p.starts_with("/api/auth/")
}

/// What is wrong with this answer, if anything.
pub fn judge(req: &Req, resp: &std::result::Result<Resp, &'static str>) -> Option<(String, u16)> {
    let r = match resp {
        Err("timeout") => return Some(("hung: no answer within 3 s".into(), 0)),
        Err(_) => return Some(("refused the connection".into(), 0)),
        Ok(r) => r,
    };
    if r.status == 0 {
        return Some(("dropped the connection without an answer".into(), 0));
    }
    let supported = matches!(req.method, "GET" | "POST" | "DELETE" | "PATCH" | "HEAD");
    if r.status >= 500 && (supported || r.status != 501) {
        return Some((format!("server error {}", r.status), r.status));
    }
    if r.head.contains("access-control-allow-origin") {
        return Some((
            "opens the API to other sites (CORS header)".into(),
            r.status,
        ));
    }
    if (200..300).contains(&r.status) && !req.cookie_valid && !is_public(&req.path) {
        return Some((
            "answered 2xx to a caller who isn't signed in".into(),
            r.status,
        ));
    }
    None
}
