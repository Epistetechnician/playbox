mod cli;
mod control;
mod doctor;
mod hub;
mod wizard;
mod latent;
mod python;
mod release;
mod rooms;
mod secure;
mod session;
mod service;
mod setup;
mod trust;
mod tui;
mod update;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use cli::{Cli, Cmd, LatentCmd, ReleaseCmd, SteerCmd};
use std::process::ExitCode;

fn main() -> ExitCode {
    // A closed pipe (`playbox audit log | head`) ends the process quietly instead of a write panic.
    #[cfg(unix)]
    // SAFETY: restoring the default disposition of SIGPIPE before any thread exists.
    unsafe {
        libc_sigpipe_default();
    }
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            for cause in e.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
unsafe fn libc_sigpipe_default() {
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    signal(SIGPIPE, SIG_DFL);
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    if let Some(h) = &cli.home {
        std::env::set_var("PLAYBOX_HOME", h);
    }
    let r: Result<()> = match cli.command {
        Cmd::Up(a) => rooms::up(a).await,
        Cmd::Join(a) => rooms::join(a).await,
        Cmd::Shell(a) => session::shell(a).await,
        Cmd::Chat {
            link,
            transport,
            name,
        } => rooms::chat(&link, rooms::join_choice(&transport)?, name).await,
        Cmd::Bridge {
            link,
            transport,
            listen,
            token,
        } => rooms::bridge(&link, rooms::join_choice(&transport)?, listen, token).await,
        Cmd::Call { tool, args, json } => session::call(&tool, &args, json.as_deref()).await,
        Cmd::Mcp => session::mcp().await,
        Cmd::Id => rooms::guest_id(),
        Cmd::Latent(LatentCmd::Session { link, transport, key }) => latent::session(&link, &transport, key).await,
        Cmd::Update { check, version, rollback, force, json, pubkey } => {
            let code = update::run(&update::Opts { check, version, rollback, force, json, pubkey })?;
            if code != 0 {
                return Ok(std::process::ExitCode::from(code));
            }
            Ok(())
        }
        Cmd::Release(r) => match r {
            ReleaseCmd::Keygen { out } => release::keygen(&out),
            ReleaseCmd::Sign { file, key } => release::sign(&file, &key),
            ReleaseCmd::Notes { version, changelog } => release::notes(&version, &changelog),
            ReleaseCmd::Check { version, changelog, cargo_toml } => release::check(&version, &changelog, &cargo_toml),
            ReleaseCmd::Prepare { version, changelog, cargo_toml } => release::prepare(&version, &changelog, &cargo_toml),
            ReleaseCmd::Manifest { version, dir, changelog, out } => release::manifest(&version, &dir, &changelog, &out),
            ReleaseCmd::Verify { file, pubkey, sig } => {
                release::verify(&file, sig.as_deref(), &pubkey)
            }
        },
        Cmd::Setup {
            dry_run,
            remove,
            only,
        } => setup::run_setup(setup::Opts {
            dry_run,
            remove,
            only,
        }),
        Cmd::Review { repo, action, into } => trust::review_cmd(&repo, &action, into).await,
        Cmd::Audit { file, tail } => trust::audit_cmd(&file, tail),
        Cmd::Verify { log } => trust::verify(&log),
        Cmd::Attest { log, key, platform } => trust::attest_cmd(log, key, platform),
        Cmd::Pin(p) => trust::pin_cmd(p),
        Cmd::ApproverKey(k) => trust::approver_key(k.key),
        Cmd::Approve {
            action,
            args,
            key,
            ttl,
        } => {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            trust::approve(&action, &refs, key.key, ttl)
        }
        Cmd::Record { kind, log, key } => trust::record(kind, log, key),
        Cmd::Memory(m) => session::memory(m).await,
        Cmd::MemoryReview {
            id,
            accept,
            reject,
            key,
        } => trust::memory_review(&id, accept, reject, key.key),
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
        Cmd::Experiment { name, args } => python::experiment(name, &args),
        Cmd::World { action, args } => python::world(action, &args),
        Cmd::Init(a) => wizard::run(&a),
        Cmd::Secure(a) => return secure::run(&a).map(ExitCode::from),
        Cmd::Hub(a) => hub::run(&a),
        Cmd::Doctor { json } => return doctor::run(json),
        Cmd::Demo => trust::demo(),
        Cmd::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "playbox",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    };
    r.map(|()| ExitCode::SUCCESS)
}
