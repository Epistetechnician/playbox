//! The command tree. Parsing only; the work lives in the other modules.

use clap::{Args, Parser, Subcommand, ValueEnum};

const ABOUT: &str = "Agents from different owners, collaborating inside signed, auditable rooms";

const OVERVIEW: &str = "\
Start here:  init  secure  setup  doctor  hub
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
/// `playbox secure`: check the machine, the agents and the hub, and fuzz a throwaway copy of the hub.
#[derive(Args)]
pub struct SecureArgs {
    /// Fix what can be fixed safely (file and folder permissions); everything else is only reported
    #[arg(long)]
    pub fix: bool,
    /// Machine-readable report on stdout
    #[arg(long)]
    pub json: bool,
    /// Skip the fuzz run (the checks alone take about a second)
    #[arg(long)]
    pub no_fuzz: bool,
    /// Fuzz much longer: 20 000 requests instead of 1 500
    #[arg(long)]
    pub deep: bool,
    /// Repeat a fuzz run exactly; a report prints the seed and request number of every problem
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,
    /// Also ask the release server whether an update exists
    #[arg(long)]
    pub online: bool,
    /// The running hub's port (default 7717)
    #[arg(long, value_name = "PORT", env = "PLAYBOX_HUB_PORT")]
    pub port: Option<u16>,
    /// Where the hub lives, for the fuzz run; default: found automatically
    #[arg(long, value_name = "DIR", env = "PLAYBOX_HUB_DIR")]
    pub hub_dir: Option<String>,
    /// Exit 3 when something only needs watching (otherwise only a failure is a non-zero exit)
    #[arg(long)]
    pub strict: bool,
}

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
    /// Zero-friction room template: swe, research, review, or ops (pre-configures runner, egress allowlist, capability approvals, memory, and Canvas board)
    #[arg(long, value_enum, value_name = "TEMPLATE")]
    pub template: Option<RoomTemplate>,
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
    /// Weakest task isolation the host accepts from its runner: container (default), microvm, or confidential. Values above container require --runtime or --docker.
    #[arg(long, value_enum, default_value = "container", value_name = "LEVEL")]
    pub min_isolation: MinIsolation,
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
    #[arg(
        long,
        value_name = "LEVEL",
        default_value = "none",
        requires = "confine"
    )]
    pub confine_net: String,
    /// With --confine: skip the syscall filter (for debugging a guest that needs a denied call)
    #[arg(long, requires = "confine")]
    pub no_seccomp: bool,
    /// Let members' tasks read this secret from the host's secrets directory ($PLAYBOX_HOME/secrets) at /run/secrets; repeatable
    #[arg(long = "allow-secret", value_name = "NAME")]
    pub allow_secret: Vec<String>,
    /// Named model-API upstreams for the host-side egress proxy (JSON file: array of upstreams or {upstreams, log_file})
    #[arg(long, value_name = "FILE")]
    pub egress: Option<String>,
    /// Let members' tasks reach this upstream from --egress through the host-side proxy (injects secret header host-side); repeatable
    #[arg(long = "allow-egress", value_name = "NAME")]
    pub allow_egress: Vec<String>,
    /// Let members' tasks use this container image, besides the default (alpine:3); repeatable. Members still cannot name any other image.
    #[arg(long, value_name = "IMAGE")]
    pub allow_image: Vec<String>,
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
    /// Maximum number of members the room admits (default 64)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_members: Option<u64>,
    /// Tasks one member may have running at once (default 2)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_concurrent: Option<u64>,
    /// Tasks one member may start per minute (default 30)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_tasks_per_minute: Option<u64>,
    /// Total tasks the room may start over the host's lifetime (default 100000)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub task_budget: Option<u64>,
    /// CPU cores each task may use (0.1..=256, default 2.0)
    #[arg(long, value_name = "CPUS", value_parser = parse_task_cpus)]
    pub task_cpus: Option<f32>,
    /// Memory each task may use, in MiB (at least 16, default 1024)
    #[arg(long, value_name = "MIB", value_parser = clap::value_parser!(u32).range(16..))]
    pub task_mem_mib: Option<u32>,
    /// Time limit for each task, in seconds (1..=2592000, default 300)
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..=2_592_000))]
    pub task_timeout_secs: Option<u64>,
    /// How long each member's grant lasts, in seconds (default 3600, at most 30 days). A grant that runs out mid-session is refused,
    /// so agents that run for hours need a longer one; the invite link's own lifetime is separate.
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..=2_592_000))]
    pub grant_ttl: Option<u64>,
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
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub latent_posts_per_minute: Option<u64>,
    /// How long a latent message stays readable, and an identical one is refused as a replay (default 3600)
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
    pub latent_max_age_secs: Option<u64>,
    /// Be another host of an existing room (its root public key, 64 hex); needs --state
    #[arg(long, value_name = "ROOTKEY")]
    pub replica_of: Option<String>,
    /// Another host to share revocations with (peer address); repeatable
    #[arg(long = "sync-peer", value_name = "ADDR")]
    pub sync_peer: Vec<String>,
    #[arg(long, value_name = "SECS", default_value_t = 15)]
    pub sync_interval: u64,
    /// How long the first invite link works, in seconds (at most 24 hours)
    #[arg(long, value_name = "SECS", default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86_400))]
    pub link_ttl: u64,
    /// How many members the first invite link admits (at most 100)
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub link_uses: u32,
    /// Primary: replicate a signed room log to this host (peer address); repeatable. A log entry counts once more
    /// than half of the hosts (these plus this one) have cosigned it. Needs --state. See docs/host-less-rooms.md
    #[arg(long = "room-log-peer", value_name = "ADDR", requires = "state")]
    pub room_log_peer: Vec<String>,
    /// Replica: follow the room log of this leader (peer address) and cosign what checks out
    #[arg(long = "room-log-leader", value_name = "ADDR", requires = "replica_of")]
    pub room_log_leader: Option<String>,
    /// The program this host runs to measure a candidate when the room asks for a promotion. It gets the problem
    /// hash and the candidate hash (64 hex characters each) and prints one JSON line:
    /// {"metric": micro-units (higher is better), "weights": hex, "env": hex, "tier": n}. Without one this host
    /// cannot sign receipts
    #[arg(long = "room-log-evaluator", value_name = "PROGRAM")]
    pub room_log_evaluator: Option<String>,
    /// A follower that has not heard from the room log's leader for this many seconds (plus a turn per place in
    /// the host list) bids to take over, and leads once more than half of the hosts sign. Give a follower every
    /// other host as --room-log-peer so it has someone to ask
    #[arg(long = "room-log-failover", value_name = "SECS", default_value_t = 30)]
    pub room_log_failover: u64,
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
    /// Join without asking, after showing what the host is
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Refuse the host unless its attestation reaches this tier (t0..t3)
    #[arg(long, value_name = "TIER")]
    pub min_host_tier: Option<String>,
    /// Pin the host's device key if new (trust on first use)
    #[arg(long)]
    pub trust_host_device: bool,
}

#[derive(Args)]
pub struct LaunchArgs {
    /// A running host on this machine: its label or the start of its id
    #[arg(long, value_name = "ROOM")]
    pub room: String,
    /// What the agent is, for the record (for example Hermes, claude-code)
    #[arg(long, value_name = "NAME")]
    pub harness: String,
    /// The model it runs, for the record
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// Directory to run the command in
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<std::path::PathBuf>,
    /// The command to run, after `--`. It gets PLAYBOX_HOME (its own key), PLAYBOX_JOIN_LINK_FILE (a link only that key can use) and
    /// PLAYBOX_AGENT_HARNESS / PLAYBOX_AGENT_MODEL.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
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

// One value of this enum is built per process, so the size of the largest variant costs nothing worth boxing the arguments for.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Cmd {
    /// Start a room and print a join link
    Up(UpArgs),
    /// Join a room; with --task run a command on the host and print the patch
    Join(JoinArgs),
    /// Start an agent on this machine that the room's host vouches for: its own key, a link bound to it, and a host-signed launch record
    Launch(LaunchArgs),
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
        /// Verify against this ML-DSA-65 release public key (hybrid PQ check)
        #[arg(long, value_name = "HEX")]
        pq_pubkey: Option<String>,
    },
    /// Sign and verify releases (maintainers; the installer uses `verify`)
    #[command(subcommand, hide = true)]
    Release(ReleaseCmd),
    /// Print your guest id, so a host can bind a join link to you
    Id,
    /// Check a host's saved room log from scratch: every signature, the chain, the roster and the commit point
    RoomLog {
        /// The host's state directory (or the roomlog.jsonl in it)
        state: String,
        /// The room's root public key (64 hex characters)
        #[arg(long, value_name = "HEX")]
        root: String,
        /// Print the result as JSON
        #[arg(long)]
        json: bool,
    },
    /// Export and offline-verify v0.4 Attested Gate compliance receipt bundles
    #[command(subcommand)]
    Gate(GateCmd),
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
    /// Sign or verify an Attested Gate Receipt with hybrid Ed25519 + ML-DSA-65 (FIPS 204)
    #[command(subcommand)]
    Receipt(ReceiptCmd),
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

    /// Check this machine, your agents and the hub for security, fuzz a throwaway copy of the hub, and fix permissions with --fix
    Secure(SecureArgs),
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
pub enum GateCmd {
    /// Export an offline-verifiable Attested Gate compliance receipt bundle for the 5 runtime-native gates
    Export {
        /// Host state directory (`--state DIR`): discovers audit.log, roomlog.jsonl, root.key, and endpoint.key
        #[arg(long, value_name = "DIR")]
        state: Option<String>,
        /// Host audit log file (default: STATE/audit.log if present)
        #[arg(long, value_name = "FILE")]
        audit: Option<String>,
        /// Replicated room log file (default: STATE/roomlog.jsonl if present)
        #[arg(long, value_name = "FILE")]
        room_log: Option<String>,
        /// Room root public key (64 hex chars; default: derived from STATE/root.key if present)
        #[arg(long, value_name = "HEX")]
        root: Option<String>,
        /// BOX-7 egress proxy JSONL log file
        #[arg(long, value_name = "FILE")]
        egress_log: Option<String>,
        /// Challenge gate receipts JSONL file (from challenge/gate.py or room_eval.py)
        #[arg(long, value_name = "FILE")]
        challenge_log: Option<String>,
        /// Signed Attested Gate run receipt JSON (`playbox.attested_gate_receipt.v1`); repeatable
        #[arg(long = "receipt", value_name = "FILE")]
        receipts: Vec<String>,
        /// Signed host attestation statement file (from `playbox attest`)
        #[arg(long, value_name = "FILE")]
        attestation: Option<String>,
        /// Host/custodian 32-byte Ed25519 hex seed file to sign the bundle (default: STATE/endpoint.key if present)
        #[arg(long, value_name = "FILE")]
        sign_key: Option<String>,
        /// Optional FIPS 204 ML-DSA-65 private key hex file for hybrid PQ bundle signing
        #[arg(long, value_name = "FILE")]
        pq_key: Option<String>,
        /// Omit bundle signature even if STATE/endpoint.key is present
        #[arg(long)]
        no_sign: bool,
        /// Write bundle JSON to this file (default: stdout)
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// Sign a Playbox Attested Gate run receipt JSON (`playbox.attested_gate_receipt.v1`) with hybrid Ed25519 + ML-DSA-65
    Sign {
        /// Path to the receipt JSON file (updated in place unless --out is set)
        file: String,
        /// 32-byte hex seed file (created if missing; default $PLAYBOX_HOME/attested_gate.key)
        #[arg(long, value_name = "FILE")]
        key: Option<String>,
        /// Optional output path (defaults to updating FILE in place)
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// Verify an Attested Gate compliance bundle (`playbox.attested_gate.v1`) or run receipt (`playbox.attested_gate_receipt.v1`) offline
    Verify {
        /// Path to the compliance bundle or run receipt JSON file (or `-` for stdin)
        bundle: String,
        /// Require bundle signature by this 64-hex Ed25519 public key
        #[arg(long, value_name = "HEX")]
        pubkey: Option<String>,
        /// Require hybrid ML-DSA-65 signature by this hex public key
        #[arg(long, value_name = "HEX")]
        pq_pubkey: Option<String>,
        /// Fail if the bundle itself carries no Ed25519 signature
        #[arg(long)]
        require_signature: bool,
        /// Require ML-DSA-65 post-quantum enforcement (bundle pq_sig and/or PQ-committed RoomLog checkpoints)
        #[arg(long)]
        require_pq: bool,
        /// Require verified host attestation reaching at least this tier (t0..t3)
        #[arg(long, value_name = "TIER")]
        require_tier: Option<String>,
        /// Machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
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
    /// Make an ML-DSA-65 release keypair; writes secret to OUT and prints the public key hex
    PqKeygen { out: String },
    /// Sign FILE, writing FILE.sig (and FILE.pq.sig when --pq-key is given)
    Sign {
        file: String,
        #[arg(long, env = "PLAYBOX_RELEASE_KEY", hide_env_values = true)]
        key: String,
        #[arg(long, env = "PLAYBOX_RELEASE_PQ_KEY", hide_env_values = true)]
        pq_key: Option<String>,
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
    /// Check FILE.sig against a public key (and optional FILE.pq.sig against --pq-pubkey)
    Verify {
        file: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        sig: Option<String>,
        #[arg(long)]
        pq_pubkey: Option<String>,
        #[arg(long)]
        pq_sig: Option<String>,
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

#[derive(Subcommand)]
pub enum ReceiptCmd {
    /// Sign a Playbox Attested Gate Receipt JSON file with hybrid Ed25519 + ML-DSA-65 (FIPS 204)
    Sign {
        /// Path to the receipt JSON file (updated in place unless --out is set)
        file: String,
        /// 32-byte hex seed file (created if missing; default $PLAYBOX_HOME/attested_gate.key)
        #[arg(long, value_name = "FILE")]
        key: Option<String>,
        /// Optional output path (defaults to updating FILE in place)
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// Verify a Playbox Attested Gate Receipt JSON file (canonical SHA-256 + Ed25519 + ML-DSA-65)
    Verify {
        /// Path to the receipt JSON file
        file: String,
        /// Optional expected Ed25519 public key hex (64 hex chars)
        #[arg(long, value_name = "HEX")]
        pubkey: Option<String>,
        /// Optional expected ML-DSA-65 public key hex (3904 hex chars)
        #[arg(long, value_name = "HEX")]
        pq_pubkey: Option<String>,
        /// Require a valid ML-DSA-65 post-quantum signature (reject Ed25519-only receipts)
        #[arg(long)]
        require_pq: bool,
    },
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

/// Built-in room templates for `playbox up --template`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RoomTemplate {
    Swe,
    Research,
    Review,
    Ops,
}

impl From<RoomTemplate> for playbox_net::templates::TemplateKind {
    fn from(t: RoomTemplate) -> Self {
        match t {
            RoomTemplate::Swe => Self::Swe,
            RoomTemplate::Research => Self::Research,
            RoomTemplate::Review => Self::Review,
            RoomTemplate::Ops => Self::Ops,
        }
    }
}

/// Minimum task isolation accepted by `playbox up --min-isolation`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum MinIsolation {
    #[default]
    Container,
    #[value(name = "microvm")]
    MicroVm,
    Confidential,
}

fn parse_task_cpus(s: &str) -> Result<f32, String> {
    let v: f32 = s
        .parse()
        .map_err(|e| format!("invalid CPU limit {s:?}: {e}"))?;
    if !v.is_finite() || !(0.1..=256.0).contains(&v) {
        return Err(format!("--task-cpus must be between 0.1 and 256 (got {s})"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::{Cli, Cmd};
    use clap::{CommandFactory, Parser};

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    /// Whatever the arguments, parsing returns a result: it never panics, so a bad command line is an error message.
    #[test]
    fn random_argument_lists_never_panic() {
        let words = [
            "up",
            "join",
            "shell",
            "call",
            "memory",
            "remember",
            "recall",
            "pin",
            "add",
            "list",
            "experiment",
            "world",
            "doctor",
            "--json",
            "--home",
            "--task",
            "--state",
            "--local",
            "--tui",
            "--console",
            "-",
            "--",
            "",
            "e6",
            "fact",
            "x",
            "--tail",
            "-1",
            "99999999999999999999",
            "playbox://l",
            "--latent-hub",
            "--allow-host",
            "\u{0}",
            "é",
            "--key",
            "--ttl",
            "approve",
            "--arg",
            "completions",
            "zsh",
            "--min-host-tier",
            "t9",
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

    #[test]
    fn extra_images_can_be_allowed_and_repeat() {
        let c = Cli::try_parse_from([
            "playbox",
            "up",
            "--allow-image",
            "python:3.12-alpine",
            "--allow-image",
            "alpine:3",
        ])
        .unwrap();
        let Cmd::Up(a) = c.command else {
            panic!("not up")
        };
        assert_eq!(a.allow_image, ["python:3.12-alpine", "alpine:3"]);
    }

    #[test]
    fn the_grant_lifetime_is_bounded_at_parse_time() {
        assert!(Cli::try_parse_from(["playbox", "up", "--grant-ttl", "21600"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--grant-ttl", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--grant-ttl", "2592001"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--grant-ttl", "6h"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-budget", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--max-members", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--max-members", "5"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--max-concurrent", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--max-tasks-per-minute", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--latent-posts-per-minute", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--latent-max-age-secs", "0"]).is_err());
    }

    #[test]
    fn the_initial_link_ttl_and_uses_default_and_are_bounded_at_parse_time() {
        let Cmd::Up(a) = Cli::try_parse_from(["playbox", "up"]).unwrap().command else {
            panic!("not up")
        };
        assert_eq!((a.link_ttl, a.link_uses), (3600, 1));

        let Cmd::Up(a) =
            Cli::try_parse_from(["playbox", "up", "--link-ttl", "600", "--link-uses", "5"])
                .unwrap()
                .command
        else {
            panic!("not up")
        };
        assert_eq!((a.link_ttl, a.link_uses), (600, 5));

        assert!(Cli::try_parse_from(["playbox", "up", "--link-ttl", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--link-ttl", "86401"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--link-uses", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--link-uses", "101"]).is_err());
    }

    #[test]
    fn min_isolation_accepts_known_levels_and_refuses_none() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(args).map(|c| match c.command {
                Cmd::Up(a) => a.min_isolation,
                _ => panic!("not up"),
            })
        };
        assert_eq!(
            parse(&["playbox", "up"]).unwrap(),
            super::MinIsolation::Container
        );
        assert_eq!(
            parse(&["playbox", "up", "--min-isolation", "container"]).unwrap(),
            super::MinIsolation::Container
        );
        assert_eq!(
            parse(&["playbox", "up", "--min-isolation", "microvm"]).unwrap(),
            super::MinIsolation::MicroVm
        );
        assert_eq!(
            parse(&["playbox", "up", "--min-isolation", "confidential"]).unwrap(),
            super::MinIsolation::Confidential
        );
        assert!(parse(&["playbox", "up", "--min-isolation", "none"]).is_err());
        assert!(parse(&["playbox", "up", "--min-isolation", "bogus"]).is_err());
    }

    #[test]
    fn task_limit_flags_are_bounded_at_parse_time() {
        let Cmd::Up(a) = Cli::try_parse_from([
            "playbox",
            "up",
            "--task-cpus",
            "0.5",
            "--task-mem-mib",
            "256",
            "--task-timeout-secs",
            "60",
        ])
        .unwrap()
        .command
        else {
            panic!("not up")
        };
        assert_eq!(a.task_cpus, Some(0.5));
        assert_eq!(a.task_mem_mib, Some(256));
        assert_eq!(a.task_timeout_secs, Some(60));

        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "0.1"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "256"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "0.05"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "-1"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "256.1"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "NaN"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-cpus", "inf"]).is_err());

        assert!(Cli::try_parse_from(["playbox", "up", "--task-mem-mib", "16"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-mem-mib", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-mem-mib", "15"]).is_err());

        assert!(Cli::try_parse_from(["playbox", "up", "--task-timeout-secs", "1"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-timeout-secs", "2592000"]).is_ok());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-timeout-secs", "0"]).is_err());
        assert!(Cli::try_parse_from(["playbox", "up", "--task-timeout-secs", "2592001"]).is_err());
    }
}
