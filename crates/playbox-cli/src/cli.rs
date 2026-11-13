//! The command tree. Parsing only; the work lives in the other modules.

use clap::{Args, Parser, Subcommand, ValueEnum};

const ABOUT: &str = "Agents from different owners, collaborating inside signed, auditable rooms";

const OVERVIEW: &str = "\
Start here:  init  setup  doctor  hub
Rooms:       up  join  shell  bridge  call  mcp  id
Review:      review  audit  revoke (inside `up --console` or `shell`)
Trust:       verify  attest  pin  approver-key  approve  record
Memory:      memory remember|recall|export|ingest|review
Steering:    steer extract|random|manifest|run|probe   experiment e1|e1-merge|e6|e6b|audit-e6|cache-audit
Other:       world  demo  completions

Everything a connected agent can do over MCP, you can do by hand: `playbox shell LINK`, then `help`.
State lives in $PLAYBOX_HOME (default ~/.playbox).";

#[derive(Parser)]
#[command(name = "playbox", version = env!("PLAYBOX_VERSION_LONG"), about = ABOUT, after_help = OVERVIEW, arg_required_else_help = true, disable_help_subcommand = true)]
pub struct Cli {
    /// Where keys, history, pins and memory live (same as `PLAYBOX_HOME`)
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<String>,
    #[command(subcommand)]
    pub command: Cmd,
}

/// `playbox hub`: the dashboard app with passkey sign-in, on this machine.
/// `playbox init`: set Playbox up on this machine, end to end.
#[derive(Args)]
pub struct InitArgs {
    /// Don't ask: take the flags, else what was remembered, else the safe default (this computer only)
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Show what would change and change nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Don't install the background service (run `playbox hub` yourself)
    #[arg(long)]
    pub no_service: bool,
    /// Don't register Playbox with the coding agents on this machine
    #[arg(long)]
    pub no_agents: bool,
    /// This computer only
    #[arg(long, conflicts_with_all = ["cloudflare", "tunnel"])]
    pub local: bool,
    /// HTTPS from anywhere through a Cloudflare quick tunnel (free, no account, the address changes on every restart)
    #[arg(long, conflicts_with = "tunnel")]
    pub cloudflare: bool,
    /// HTTPS from anywhere through your own Cloudflare tunnel (name or id); needs --hostname
    #[arg(long, value_name = "NAME")]
    pub tunnel: Option<String>,
    /// The public hostname routed to --tunnel
    #[arg(long, value_name = "HOST")]
    pub hostname: Option<String>,
    /// Hub port (default 7717)
    #[arg(long, value_name = "PORT", env = "PLAYBOX_HUB_PORT")]
    pub port: Option<u16>,
    /// Also let phones on this network pair over plain HTTP (weaker than passkeys over HTTPS)
    #[arg(long)]
    pub lan: bool,
    /// Where the hub lives (a checkout, or the installer's directory); default: found automatically
    #[arg(long, value_name = "DIR", env = "PLAYBOX_HUB_DIR")]
    pub hub_dir: Option<String>,
}

/// What to do with the hub's background service.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    /// Run the hub in the background and start it at login (launchd on macOS, a systemd user unit on Linux)
    Install,
    /// Stop it and remove it
    Uninstall,
    /// Is it running?
    Status,
}

#[derive(Args, Clone)]
pub struct HubArgs {
    /// Port to listen on (default 7717)
    #[arg(long, value_name = "PORT", env = "PLAYBOX_HUB_PORT")]
    pub port: Option<u16>,
    /// Publish over HTTPS with a Cloudflare quick tunnel: free, no account, a new address each run
    #[arg(long, conflicts_with_all = ["tunnel", "local"])]
    pub cloudflare: bool,
    /// Publish through your own named Cloudflare tunnel (name or id; created for you if it doesn't exist); needs --hostname
    #[arg(long, value_name = "NAME", conflicts_with = "local")]
    pub tunnel: Option<String>,
    /// The public hostname routed to --tunnel, e.g. hub.example.com
    #[arg(long, value_name = "HOST")]
    pub hostname: Option<String>,
    /// This computer only, this run (skip the question and ignore what was remembered)
    #[arg(long)]
    pub local: bool,
    /// Also listen on the local network, so a phone on your Wi-Fi can pair
    #[arg(long)]
    pub lan: bool,
    /// Don't open the browser
    #[arg(long)]
    pub no_open: bool,
    /// Print a fresh pairing link for a new device
    #[arg(long)]
    pub pair: bool,
    /// Ask again how to reach the hub and remember the answer
    #[arg(long)]
    pub reconfigure: bool,
    /// Where the hub lives (a checkout, or the installer's directory); default: found automatically
    #[arg(long, value_name = "DIR", env = "PLAYBOX_HUB_DIR")]
    pub hub_dir: Option<String>,
    /// Manage the background service instead of running in this window: install (runs at login, restarts if it stops), uninstall, status
    #[arg(long, value_enum, value_name = "ACTION")]
    pub service: Option<ServiceAction>,
    /// Anything else, passed to the hub (try `-- --help`)
    #[arg(last = true)]
    pub extra: Vec<String>,
}

/// How peers reach each other.
#[derive(Args, Clone, Default)]
pub struct TransportArgs {
    /// iroh (public relays, default), local (same machine/LAN) or relay:<https url>
    #[arg(long, value_name = "T", env = "PLAYBOX_TRANSPORT")]
    pub transport: Option<String>,
    /// Shorthand for --transport local
    #[arg(long)]
    pub local: bool,
}

#[derive(Args)]
pub struct UpArgs {
    #[command(flatten)]
    pub transport: TransportArgs,
    /// Keep the room's identity, members and bans across restarts
    #[arg(long, value_name = "DIR")]
    pub state: Option<String>,
    /// Share this git repository with the room
    #[arg(long, value_name = "DIR")]
    pub repo: Option<String>,
    /// Let members run tasks: libkrun (a microVM each), docker, or openshell
    #[arg(long, value_name = "NAME")]
    pub runtime: Option<String>,
    /// Shorthand for --runtime docker
    #[arg(long)]
    pub docker: bool,
    /// Let tasks reach this host[:port]; repeatable; only openshell enforces it
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_host: Vec<String>,
    /// Offer members named operations, run in the sandbox once each: a JSON file listing {name, argv with {param} placeholders,
    /// params {allowed, max_len}, approval, image, timeout_secs} (docs/capabilities.md). Calls that need approval wait for
    /// `approve ID` in --console. Needs --runtime.
    #[arg(long, value_name = "FILE")]
    pub capabilities: Option<String>,
    /// Linux, libkrun: run each task's helper in a jail, under a cgroup, behind a network filter (docs/libkrun.md)
    #[arg(long)]
    pub confine: bool,
    /// With --confine: the most network a task may have: none (default), internet, internet+lan
    #[arg(long, value_name = "LEVEL", default_value = "none", requires = "confine")]
    pub confine_net: String,
    /// With --confine: skip the syscall filter (for debugging a guest that needs a denied call)
    #[arg(long, requires = "confine")]
    pub no_seccomp: bool,
    /// Let members' tasks read this secret from the host's secrets directory ($PLAYBOX_HOME/secrets) at /run/secrets; repeatable
    #[arg(long = "allow-secret", value_name = "NAME")]
    pub allow_secret: Vec<String>,
    /// Audit log file (`-` is stderr); default DIR/audit.log with --state
    #[arg(long, value_name = "FILE")]
    pub audit_log: Option<String>,
    /// Host language models for steering: a directory of weights/<hash>/ (needs docker)
    #[arg(long, value_name = "DIR")]
    pub steer_store: Option<String>,
    #[arg(long, value_name = "IMAGE", default_value = "playbox-steer:0.1")]
    pub steer_image: String,
    #[arg(long, value_name = "MIB", default_value_t = 4096)]
    pub steer_mem_mib: u32,
    #[arg(long, value_name = "N")]
    pub max_members: Option<u64>,
    #[arg(long, value_name = "N")]
    pub max_concurrent: Option<u64>,
    #[arg(long, value_name = "N")]
    pub max_tasks_per_minute: Option<u64>,
    #[arg(long, value_name = "N")]
    pub task_budget: Option<u64>,
    /// Give the room a shared memory kept by a Hindsight server running beside this host (docs/latent-channels.md 6o),
    /// e.g. http://127.0.0.1:8888/v1/default. Members use `remember` and `recall`.
    #[arg(long = "memory-url", value_name = "URL")]
    pub memory_url: Option<String>,
    /// The room's bank on the memory server (letters, digits, - and _). Default: the state directory's name, else "room".
    #[arg(long = "memory-bank", value_name = "NAME")]
    pub memory_bank: Option<String>,
    /// Let members read web pages through a browser on this machine's network: its DevTools endpoint, as ws://host:port/devtools/browser/ID
    /// or http://host:port (docs/latent-channels.md 6q). Needs at least one --browse-allow.
    #[arg(long = "browse-endpoint", value_name = "URL")]
    pub browse_endpoint: Option<String>,
    /// A host the browser may reach: example.com, *.example.com, or host:port; repeatable. Requests to any other host are blocked.
    #[arg(long = "browse-allow", value_name = "HOST")]
    pub browse_allow: Vec<String>,
    /// Run the pages' scripts (default: off).
    #[arg(long = "browse-js")]
    pub browse_js: bool,
    /// Let the browser reach private and loopback addresses (default: refused).
    #[arg(long = "browse-allow-private")]
    pub browse_allow_private: bool,
    /// Hold members' entries as pending until the host accepts them; nothing pending is indexed or recalled.
    #[arg(long = "memory-review")]
    pub memory_review: bool,
    /// Accept hub latent messages only for this hub id; repeatable. Without it any hub id is accepted
    #[arg(long = "latent-hub", value_name = "HUB")]
    pub latent_hub: Vec<String>,
    /// Latent messages one member may post per minute (default 30)
    #[arg(long, value_name = "N")]
    pub latent_posts_per_minute: Option<u64>,
    /// How long a latent message stays readable, and an identical one is refused as a replay (default 3600)
    #[arg(long, value_name = "SECS")]
    pub latent_max_age_secs: Option<u64>,
    /// Be another host of an existing room (its root public key, 64 hex); needs --state
    #[arg(long, value_name = "ROOTKEY")]
    pub replica_of: Option<String>,
    /// Another host to share revocations with (peer address); repeatable
    #[arg(long = "sync-peer", value_name = "ADDR")]
    pub sync_peer: Vec<String>,
    #[arg(long, value_name = "SECS", default_value_t = 15)]
    pub sync_interval: u64,
    /// Make the link work only for this guest (their `playbox id`); a leaked link is then useless
    #[arg(long, value_name = "GUEST ID")]
    pub for_guest: Option<String>,
    /// Put this machine's IP addresses in the link (off by default on iroh and relay transports)
    #[arg(long)]
    pub link_ips: bool,
    /// Read host commands from stdin: link, status, members, audit, revoke ID, quit
    #[arg(long)]
    pub console: bool,
    /// Full-screen chat room for the host: see and answer everyone, mint invites, revoke
    #[arg(long, conflicts_with = "console")]
    pub tui: bool,
}

#[derive(Args)]
pub struct JoinArgs {
    /// playbox:// link from the host
    pub link: String,
    #[command(flatten)]
    pub transport: TransportArgs,
    /// Run this shell command on the host's sandbox and print its output and patch
    #[arg(long, value_name = "CMD")]
    pub task: Option<String>,
    /// Branch or commit the task starts from
    #[arg(long, value_name = "REF", default_value = "main")]
    pub base: String,
    /// Use another host of the room (a replica's peer address)
    #[arg(long, value_name = "ADDR")]
    pub via: Option<String>,
}

#[derive(Args)]
pub struct ShellArgs {
    /// Join this room first
    pub link: Option<String>,
    #[command(flatten)]
    pub transport: TransportArgs,
    /// Refuse the host unless its attestation reaches this tier (t0..t3)
    #[arg(long, value_name = "TIER")]
    pub min_host_tier: Option<String>,
    /// Pin the host's device key if new (trust on first use)
    #[arg(long)]
    pub trust_host_device: bool,
}

#[derive(Args)]
pub struct KeyArg {
    /// Key file (default $`PLAYBOX_HOME/approver.key`)
    #[arg(long, value_name = "FILE")]
    pub key: Option<String>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Start a room and print a join link
    Up(UpArgs),
    /// Join a room; with --task run a command on the host and print the patch
    Join(JoinArgs),
    /// Interactive session: every MCP tool as a command (host or guest)
    Shell(ShellArgs),
    /// Join a room and sit in its live chat (full-screen); agents of any model appear alongside you
    Chat {
        link: String,
        #[command(flatten)]
        transport: TransportArgs,
        /// A label shown next to your id
        #[arg(long)]
        name: Option<String>,
    },
    /// Serve a room to stock A2A HTTP clients
    Bridge {
        link: String,
        #[command(flatten)]
        transport: TransportArgs,
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// Required for non-loopback --listen
        #[arg(long, value_name = "T", env = "PLAYBOX_BRIDGE_TOKEN")]
        token: Option<String>,
    },
    /// Register `playbox mcp` with Claude Code, Codex, `OpenCode` and Hermes on this machine
    Setup {
        /// Show what would change without changing it
        #[arg(long)]
        dry_run: bool,
        /// Undo a previous setup
        #[arg(long)]
        remove: bool,
        /// Only these agents (claude, codex, opencode, hermes); repeatable
        #[arg(long, value_name = "AGENT")]
        only: Vec<String>,
    },
    /// Update playbox to the latest release (checked against SHA256SUMS and the pinned release key)
    Update {
        /// Only say whether an update exists (exit status 10 when it does)
        #[arg(long)]
        check: bool,
        /// Install this exact version instead of the latest (also allows a downgrade)
        #[arg(long, value_name = "X.Y.Z")]
        version: Option<String>,
        /// Go back to the version kept from the last update
        #[arg(long, conflicts_with_all = ["check", "version"])]
        rollback: bool,
        /// Reinstall even when already up to date
        #[arg(long)]
        force: bool,
        /// Print the check as JSON
        #[arg(long)]
        json: bool,
        /// Verify against this release public key instead of the one built in
        #[arg(long, value_name = "HEX")]
        pubkey: Option<String>,
    },
    /// Sign and verify releases (maintainers; the installer uses `verify`)
    #[command(subcommand, hide = true)]
    Release(ReleaseCmd),
    /// Print your guest id, so a host can bind a join link to you
    Id,
    /// Hub latent messages (docs/latent-channels.md): relay signed vectors for an agent that holds its own key
    #[command(subcommand)]
    Latent(LatentCmd),
    /// Run one MCP tool once (e.g. `call verify path=log`, `call checkpoint`)
    Call {
        /// Tool name, with or without the playbox_ prefix
        tool: String,
        /// key=value pairs; values are JSON when they parse, else text
        args: Vec<String>,
        /// Whole argument object as JSON
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },
    /// Serve the MCP tools over stdio (what `playbox-mcp` does)
    Mcp,

    /// Decide what members pushed: list, show, accept, reject
    Review {
        repo: String,
        /// list | show REF | accept REF | reject REF
        action: Vec<String>,
        #[arg(long, value_name = "BRANCH", default_value = "main")]
        into: String,
    },
    /// Check a host's audit log and show its last entries
    Audit {
        file: String,
        #[arg(long, default_value_t = 20)]
        tail: usize,
    },

    /// Verify a history log: chain, signatures, attestations
    Verify { log: String },
    /// Append a signed attestation of this binary to a history log
    Attest {
        #[arg(long, value_name = "FILE")]
        log: Option<String>,
        #[arg(long, value_name = "FILE")]
        key: Option<String>,
        /// auto | none | se | tpm
        #[arg(long, value_name = "P")]
        platform: Option<String>,
    },
    /// Device keys you trust
    #[command(subcommand)]
    Pin(PinCmd),
    /// Create or show your approver key
    ApproverKey(KeyArg),
    /// Approve one refused action; prints a single-use capability
    Approve {
        #[arg(long)]
        action: String,
        /// Exact argument text; repeatable
        #[arg(long = "arg", value_name = "TEXT")]
        args: Vec<String>,
        #[command(flatten)]
        key: KeyArg,
        #[arg(long, value_name = "SECS", default_value_t = 300)]
        ttl: u64,
    },
    /// Append stdin as a signed entry to a history log (agent hook target)
    Record {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, value_name = "FILE")]
        log: Option<String>,
        #[arg(long, value_name = "FILE")]
        key: Option<String>,
    },

    /// Shared memory with provenance
    #[command(subcommand)]
    Memory(MemoryCmd),
    #[command(name = "memory-review", hide = true)]
    MemoryReview {
        id: String,
        #[arg(long)]
        accept: bool,
        #[arg(long)]
        reject: bool,
        #[command(flatten)]
        key: KeyArg,
    },

    /// Language-model steering tools (python; see docs/steering.md)
    #[command(subcommand)]
    Steer(SteerCmd),
    /// Reproduce a steering experiment from docs/steering.md
    Experiment {
        name: Experiment,
        /// Passed through to the experiment
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The learned 3D view of a room (python)
    World {
        action: WorldAction,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Set Playbox up on this machine, end to end: keys, your coding agents, the hub, how to reach it, a background service
    Init(InitArgs),
    /// Start the hub (the dashboard app) on this machine, optionally reachable over HTTPS through Cloudflare
    Hub(HubArgs),
    /// Check this machine: keys, platform evidence, runtimes, python tooling
    Doctor {
        /// Machine-readable output; the exit status is 1 if any check is missing
        #[arg(long)]
        json: bool,
    },
    /// Build and verify a sample history log
    Demo,
    /// Print a shell completion script
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    /// Save a fact, decision, note or skill, signed by your session key
    Remember {
        kind: MemoryKind,
        topic: String,
        body: String,
        /// History entry or commit hashes backing the claim; repeatable
        #[arg(long)]
        evidence: Vec<String>,
        /// Id of an entry this replaces
        #[arg(long)]
        supersedes: Option<String>,
    },
    /// List trusted, current entries
    Recall {
        #[arg(long)]
        kind: Option<MemoryKind>,
    },
    /// Signed event log to share with another sidecar, plus the quarantine
    Export,
    /// Apply signed events from elsewhere (they wait in quarantine)
    Ingest { events: Vec<String> },
    /// Sign an accept or reject on a quarantined entry
    Review {
        id: String,
        #[arg(long)]
        accept: bool,
        #[arg(long)]
        reject: bool,
        #[command(flatten)]
        key: KeyArg,
    },
}

#[derive(Subcommand)]
pub enum SteerCmd {
    /// Make a direction from contrasting sentences
    Extract {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Make a random unit direction
    Random {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Hash a weights directory the way a room identifies it
    Manifest {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Steer a local model (the engine the sandbox image runs)
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Project prompts onto a direction without editing anything
    Probe {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum ReleaseCmd {
    /// Make a release key; prints the public half
    Keygen { out: String },
    /// Sign FILE, writing FILE.sig. KEY is a key file or the hex itself
    Sign {
        file: String,
        #[arg(long, env = "PLAYBOX_RELEASE_KEY", hide_env_values = true)]
        key: String,
    },
    /// Print the changelog entries for a version
    Notes {
        version: String,
        #[arg(long, default_value = "CHANGELOG.md")]
        changelog: String,
    },
    /// CI gate for a tag: Cargo.toml and CHANGELOG.md both carry the version
    Check {
        version: String,
        #[arg(long, default_value = "CHANGELOG.md")]
        changelog: String,
        #[arg(long, default_value = "Cargo.toml")]
        cargo_toml: String,
    },
    /// Bump the workspace version and roll CHANGELOG.md's Unreleased section into it
    Prepare {
        version: String,
        #[arg(long, default_value = "CHANGELOG.md")]
        changelog: String,
        #[arg(long, default_value = "Cargo.toml")]
        cargo_toml: String,
    },
    /// Write release.json for the archives in DIR
    Manifest {
        version: String,
        dir: String,
        #[arg(long, default_value = "CHANGELOG.md")]
        changelog: String,
        #[arg(long, default_value = "release.json")]
        out: String,
    },
    /// Check FILE.sig against a public key
    Verify {
        file: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        sig: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum LatentCmd {
    /// Join a room once and relay `send` / `pull` / `feed` requests as JSON lines on stdin and stdout
    Session {
        link: String,
        #[command(flatten)]
        transport: TransportArgs,
        /// The 32-byte identity key to join with (created if missing); default PLAYBOX_HOME/guest.key. A signed
        /// message must be signed by this same key.
        #[arg(long, value_name = "FILE")]
        key: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PinCmd {
    /// Show pinned device keys
    List,
    /// Trust a device key (kind and fingerprint as `verify` prints them)
    Add { kind: String, fingerprint: String },
    /// Stop trusting a device key
    Remove { kind: String, fingerprint: String },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum MemoryKind {
    Fact,
    Decision,
    Note,
    Skill,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Note => "note",
            Self::Skill => "skill",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Experiment {
    E1,
    E1Merge,
    E6,
    E6b,
    AuditE6,
    CacheAudit,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum WorldAction {
    Demo,
    Test,
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::{CommandFactory, Parser};

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    /// Whatever the arguments, parsing returns a result: it never panics, so a bad command line is an error message.
    #[test]
    fn random_argument_lists_never_panic() {
        let words = [
            "up", "join", "shell", "call", "memory", "remember", "recall", "pin", "add", "list", "experiment", "world",
            "doctor", "--json", "--home", "--task", "--state", "--local", "--tui", "--console", "-", "--", "", "e6",
            "fact", "x", "--tail", "-1", "99999999999999999999", "playbox://l", "--latent-hub", "--allow-host", "\u{0}",
            "é", "--key", "--ttl", "approve", "--arg", "completions", "zsh", "--min-host-tier", "t9",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let n = (next() % 6) as usize;
            let args: Vec<&str> = std::iter::once("playbox")
                .chain((0..n).map(|_| words[(next() % words.len() as u64) as usize]))
                .collect();
            let _ = Cli::try_parse_from(&args);
        }
    }

    #[test]
    fn a_bad_value_is_refused_at_parse_time() {
        assert!(Cli::try_parse_from(["playbox", "memory", "remember", "bogus", "t", "b"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "experiment", "nope"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "pin", "add", "kind"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "memory", "remember", "fact", "t", "b"]).is_ok());
    }
}
