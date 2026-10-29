//! The command tree. Parsing only; the work lives in the other modules.

use clap::{Args, Parser, Subcommand};

const ABOUT: &str = "Agents from different owners, collaborating inside signed, auditable rooms";

const OVERVIEW: &str = "\
Rooms:       up  join  shell  bridge  call  mcp  id
Review:      review  audit  revoke (inside `up --console` or `shell`)
Trust:       verify  attest  pin  approver-key  approve  record
Memory:      memory remember|recall|export|ingest|review
Steering:    steer extract|random|manifest|run|probe   experiment e1|e1-merge|e6|e6b|audit-e6|cache-audit
Other:       world  doctor  demo  completions

Everything a connected agent can do over MCP, you can do by hand: `playbox shell LINK`, then `help`.
State lives in $PLAYBOX_HOME (default ~/.playbox).";

#[derive(Parser)]
#[command(name = "playbox", version, about = ABOUT, after_help = OVERVIEW, arg_required_else_help = true, disable_help_subcommand = true)]
pub struct Cli {
    /// Where keys, history, pins and memory live (same as PLAYBOX_HOME)
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<String>,
    #[command(subcommand)]
    pub command: Cmd,
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
    /// Key file (default $PLAYBOX_HOME/approver.key)
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
    /// Print your guest id, so a host can bind a join link to you
    Id,
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
    /// Device keys you trust: list | add KIND FP | remove KIND FP
    Pin {
        /// list | add | remove
        action: String,
        kind: Option<String>,
        fingerprint: Option<String>,
    },
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
        /// e1 | e1-merge | e6 | e6b | audit-e6 | cache-audit
        name: String,
        /// Passed through to the experiment
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The learned 3D view of a room (python)
    World {
        /// demo | test
        action: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Check this machine: keys, platform evidence, runtimes, python tooling
    Doctor,
    /// Build and verify a sample history log
    Demo,
    /// Print a shell completion script
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    /// Save a fact, decision, note or skill, signed by your session key
    Remember {
        /// fact | decision | note | skill
        kind: String,
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
        kind: Option<String>,
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
