mod cli;
mod doctor;
mod python;
mod release;
mod rooms;
mod session;
mod setup;
mod trust;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use cli::{Cli, Cmd, ReleaseCmd, SteerCmd};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(h) = &cli.home {
        std::env::set_var("PLAYBOX_HOME", h);
    }
    match cli.command {
        Cmd::Up(a) => rooms::up(a).await,
        Cmd::Join(a) => rooms::join(a).await,
        Cmd::Shell(a) => session::shell(a).await,
        Cmd::Bridge { link, transport, listen, token } => rooms::bridge(&link, rooms::join_choice(&transport)?, listen, token).await,
        Cmd::Call { tool, args, json } => session::call(&tool, &args, json.as_deref()).await,
        Cmd::Mcp => session::mcp().await,
        Cmd::Id => rooms::guest_id(),
        Cmd::Release(r) => match r {
            ReleaseCmd::Keygen { out } => release::keygen(&out),
            ReleaseCmd::Sign { file, key } => release::sign(&file, &key),
            ReleaseCmd::Verify { file, pubkey, sig } => release::verify(&file, sig.as_deref(), &pubkey),
        },
        Cmd::Setup { dry_run, remove, only } => setup::run_setup(setup::Opts { dry_run, remove, only }),
        Cmd::Review { repo, action, into } => trust::review_cmd(&repo, &action, into).await,
        Cmd::Audit { file, tail } => trust::audit_cmd(&file, tail),
        Cmd::Verify { log } => trust::verify(&log),
        Cmd::Attest { log, key, platform } => trust::attest_cmd(log, key, platform),
        Cmd::Pin { action, kind, fingerprint } => trust::pin_cmd(&action, kind.as_deref(), fingerprint.as_deref()),
        Cmd::ApproverKey(k) => trust::approver_key(k.key),
        Cmd::Approve { action, args, key, ttl } => {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            trust::approve(&action, &refs, key.key, ttl)
        }
        Cmd::Record { kind, log, key } => trust::record(kind, log, key),
        Cmd::Memory(m) => session::memory(m).await,
        Cmd::MemoryReview { id, accept, reject, key } => trust::memory_review(&id, accept, reject, key.key),
        Cmd::Steer(s) => {
            let (name, args) = match s {
                SteerCmd::Extract { args } => ("extract", args),
                SteerCmd::Random { args } => ("random", args),
                SteerCmd::Manifest { args } => ("manifest", args),
                SteerCmd::Run { args } => ("run", args),
                SteerCmd::Probe { args } => ("probe", args),
            };
            python::steer(name, &args)
        }
        Cmd::Experiment { name, args } => python::experiment(&name, &args),
        Cmd::World { action, args } => python::world(&action, &args),
        Cmd::Doctor => doctor::run(),
        Cmd::Demo => trust::demo(),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "playbox", &mut std::io::stdout());
            Ok(())
        }
    }
}
