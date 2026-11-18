//! Three hosts of one room keep the same signed log: the primary leads, two replicas that know only the room's
//! public key replay it and cosign, and an entry counts only once two of three have.

// REQ: ROOM-9, ROOM-10
use playbox_net::{join_link, new_secret_key, Host, HostConfig, Transport};
use std::{path::PathBuf, time::Duration};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-roomlog-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn replica(name: &str, root: [u8; 32]) -> Host {
    Host::open_replica(&dir(name), Transport::Local, HostConfig::default(), root)
        .await
        .unwrap()
}

#[tokio::test]
async fn three_hosts_converge_and_a_commit_needs_two() {
    let p = Host::open(&dir("p"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let (r1, r2) = (
        replica("r1", p.root_public()).await,
        replica("r2", p.root_public()).await,
    );
    p.start_room_log(&[r1.addr().id, r2.addr().id], 2).unwrap();

    // A member joins and is recorded by the leader.
    let link = p.create_link(Duration::from_secs(60), 1).unwrap().encode();
    let guest = join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let who = guest.id().to_string();

    // Hosts serve each other only once they know each other (the same rule as revocation sync). A first pull does it.
    for r in [&r1, &r2] {
        r.pull_room_log(&p.addr()).await.unwrap();
    }

    // Nothing is committed before anyone else has cosigned.
    let s = p.room_log_status().unwrap();
    assert_eq!(s.committed, 0);
    assert_eq!(s.members, vec![who.clone()]);

    // With both replicas up, the leader commits everything.
    let committed = p.commit_room_log(&[r1.addr(), r2.addr()]).await.unwrap();
    assert_eq!(committed, s.len);

    // Followers hold the same head and roster, having verified it from the root public key alone.
    for r in [&r1, &r2] {
        let t = r.room_log_status().unwrap();
        assert_eq!((t.head.as_str(), &t.members), (s.head.as_str(), &s.members));
        assert_eq!(
            t.committed, 0,
            "a follower learns the certificate when it pulls"
        );
        assert_eq!(r.pull_room_log(&p.addr()).await.unwrap(), s.len);
    }

    // Revoking is a log entry; with one replica down, the other plus the leader still make two.
    assert!(p.revoke_member(&guest.id()));
    r2.shutdown().await.unwrap();
    let committed = p.commit_room_log(&[r1.addr()]).await.unwrap();
    let s2 = p.room_log_status().unwrap();
    assert_eq!(committed, s2.len);
    assert!(s2.members.is_empty() && s2.banned == vec![who.clone()]);
    r1.pull_room_log(&p.addr()).await.unwrap();
    assert_eq!(r1.room_log_status().unwrap().banned, vec![who]);

    // With both replicas down, new entries stay uncommitted: the leader alone is not a quorum.
    r1.shutdown().await.unwrap();
    let link = p.create_link(Duration::from_secs(60), 1).unwrap().encode();
    join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let before = p.room_log_status().unwrap().committed;
    let after = p.commit_room_log(&[]).await.unwrap();
    assert_eq!(after, before, "no quorum, no commit");
    assert!(p.room_log_status().unwrap().len > after);
}

#[tokio::test]
async fn a_replica_with_another_rooms_key_refuses_the_leaders_log() {
    let p = Host::open(&dir("q"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let other = Host::open(&dir("o"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let stranger = replica("s", other.root_public()).await;
    p.start_room_log(&[stranger.addr().id], 2).unwrap();
    assert!(stranger.pull_room_log(&p.addr()).await.is_err());
    // The stranger knows a different room key, so the genesis does not verify and nothing is cosigned.
    assert_eq!(p.commit_room_log(&[stranger.addr()]).await.unwrap(), 0);
    assert!(stranger.room_log_status().is_none());
}

// REQ: ROOM-11
#[tokio::test]
async fn a_restarted_host_keeps_its_log_and_commit_point_and_refuses_a_tampered_file() {
    let (pd, rd) = (dir("persist-p"), dir("persist-r"));
    let p = Host::open(&pd, Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let root = p.root_public();
    let r = Host::open_replica(&rd, Transport::Local, HostConfig::default(), root)
        .await
        .unwrap();
    p.start_room_log(&[r.addr().id], 2).unwrap();
    r.pull_room_log(&p.addr()).await.unwrap();
    let link = p.create_link(Duration::from_secs(60), 1).unwrap().encode();
    join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    p.commit_room_log(&[r.addr()]).await.unwrap();
    let before = p.room_log_status().unwrap();
    assert_eq!(before.committed, before.len);
    p.shutdown().await.unwrap();
    r.shutdown().await.unwrap();

    // The same state directory brings the same log, head and commit point back.
    let p = Host::open(&pd, Transport::Local, HostConfig::default())
        .await
        .unwrap();
    assert_eq!(p.room_log_status().unwrap(), before);
    p.shutdown().await.unwrap();

    // The offline check reads the same file the same way.
    let st = playbox_net::roomsync::read_status(root, &pd.join("roomlog.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(st, before);

    // One changed byte in a signed entry and the host refuses to start on the file.
    let f = pd.join("roomlog.jsonl");
    let raw = std::fs::read_to_string(&f).unwrap();
    std::fs::write(&f, raw.replacen("member.join", "member.joiX", 1)).unwrap();
    assert!(Host::open(&pd, Transport::Local, HostConfig::default())
        .await
        .is_err());
    assert!(playbox_net::roomsync::read_status(root, &f).is_err());
}

async fn reopen(d: &std::path::Path, root: [u8; 32]) -> Host {
    Host::open_replica(d, Transport::Local, HostConfig::default(), root)
        .await
        .unwrap()
}

// REQ: ROOM-12
#[tokio::test]
async fn when_the_leader_dies_a_follower_takes_over_and_nothing_committed_is_lost() {
    let (pd, d1, d2) = (dir("fo-p"), dir("fo-r1"), dir("fo-r2"));
    let p = Host::open(&pd, Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let root = p.root_public();
    let (r1, r2) = (reopen(&d1, root).await, reopen(&d2, root).await);
    p.start_room_log(&[r1.addr().id, r2.addr().id], 2).unwrap();
    for r in [&r1, &r2] {
        r.pull_room_log(&p.addr()).await.unwrap();
    }
    let link = p.create_link(Duration::from_secs(60), 1).unwrap().encode();
    join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let committed = p.commit_room_log(&[r1.addr(), r2.addr()]).await.unwrap();
    assert!(committed >= 2);
    for r in [&r1, &r2] {
        r.pull_room_log(&p.addr()).await.unwrap();
    }
    let old_addr = p.addr();
    p.shutdown().await.unwrap();

    // A follower that is not the leader cannot commit; one that bids with the leader dead and a peer up wins.
    assert!(r1.commit_room_log(&[]).await.is_err());
    assert_eq!(
        r1.propose_room_log_epoch(&[r2.addr(), old_addr.clone()])
            .await
            .unwrap(),
        1
    );
    assert!(r1.room_log_is_leader() && !r2.room_log_is_leader());

    // The new leader commits with r2; everything committed before is still there, and the epoch entry is committed too.
    let after = r1.commit_room_log(&[r2.addr()]).await.unwrap();
    assert!(after > committed);
    r2.pull_room_log(&r1.addr()).await.unwrap();
    let (s1, s2) = (r1.room_log_status().unwrap(), r2.room_log_status().unwrap());
    assert_eq!((s1.epoch, s2.epoch), (1, 1));
    assert_eq!(
        (s1.head.as_str(), &s1.members),
        (s2.head.as_str(), &s2.members)
    );
    assert_eq!(s2.committed, after);

    // The old leader comes back in epoch 0, hears the epoch entry and becomes a follower.
    let p = Host::open(&pd, Transport::Local, HostConfig::default())
        .await
        .unwrap();
    assert_eq!(p.room_log_status().unwrap().epoch, 0);
    r1.commit_room_log(&[p.addr(), r2.addr()]).await.unwrap();
    let sp = p.room_log_status().unwrap();
    assert_eq!((sp.epoch, sp.leader.clone()), (1, s1.leader.clone()));
    assert!(
        p.commit_room_log(&[]).await.is_err(),
        "the old leader no longer leads"
    );

    // A follower that restarts keeps its promise and its log.
    r2.shutdown().await.unwrap();
    let r2 = reopen(&d2, root).await;
    assert_eq!(r2.room_log_status().unwrap().epoch, 1);
    assert!(r2.propose_room_log_epoch(&[]).await.is_err());
}

// REQ: ROOM-14
#[tokio::test]
async fn the_leader_can_swap_a_host_and_the_removed_one_stops_counting() {
    let p = Host::open(&dir("m3-p"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let root = p.root_public();
    let (r1, r2, r3) = (
        replica("m3-r1", root).await,
        replica("m3-r2", root).await,
        replica("m3-r3", root).await,
    );
    p.start_room_log(&[r1.addr().id, r2.addr().id], 2).unwrap();
    for r in [&r1, &r2] {
        r.pull_room_log(&p.addr()).await.unwrap();
    }
    let committed = p.commit_room_log(&[r1.addr(), r2.addr()]).await.unwrap();

    // The newcomer is let in by the leader, catches up, and then the swap is voted: r2 out, r3 in.
    p.commit_room_log(&[r3.addr()]).await.unwrap();
    r3.pull_room_log(&p.addr()).await.unwrap();
    let epoch = p
        .reconfigure_room_log(&[r1.addr(), r2.addr()], &[r3.addr()], &[r2.addr().id])
        .await
        .unwrap();
    assert_eq!(epoch, 1);
    let s = p.room_log_status().unwrap();
    let hex = |h: &Host| {
        h.addr()
            .id
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    assert!(s.hosts.contains(&hex(&r3)) && !s.hosts.contains(&hex(&r2)) && s.k == 2);

    // The new set commits the change; the removed host's old commit point still stands.
    let after = p.commit_room_log(&[r1.addr(), r3.addr()]).await.unwrap();
    assert!(after > committed);
    for r in [&r1, &r3] {
        let t = r.room_log_status().unwrap();
        assert_eq!((t.epoch, t.head.as_str()), (1, s.head.as_str()));
    }

    // With r1 and r3 gone, a new entry commits with nobody but the removed r2: it does not count.
    r1.shutdown().await.unwrap();
    r3.shutdown().await.unwrap();
    let link = p.create_link(Duration::from_secs(60), 1).unwrap().encode();
    join_link(&link, new_secret_key().unwrap(), true)
        .await
        .unwrap();
    let still = p.commit_room_log(&[r2.addr()]).await.unwrap();
    assert_eq!(
        still, after,
        "a removed host's cosignature counts for nothing"
    );
    assert!(p.room_log_status().unwrap().len > after);
}

// REQ: ROOM-15
#[tokio::test]
async fn a_guest_reads_the_signed_host_set_and_moves_to_another_host_when_its_host_dies() {
    let p = Host::open(&dir("m4-p"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let root = p.root_public();
    let (r1, r2) = (replica("m4-r1", root).await, replica("m4-r2", root).await);
    p.start_room_log(&[r1.addr().id, r2.addr().id], 2).unwrap();
    for r in [&r1, &r2] {
        r.pull_room_log(&p.addr()).await.unwrap();
    }
    let link = p.create_link(Duration::from_secs(60), 1).unwrap();
    assert_eq!(link.hosts.len(), 2, "the link names the other hosts");
    assert_eq!(
        playbox_net::Link::parse(&link.encode())
            .unwrap()
            .hosts
            .len(),
        2
    );
    let guest = join_link(&link.encode(), new_secret_key().unwrap(), true)
        .await
        .unwrap();
    p.commit_room_log(&[r1.addr(), r2.addr()]).await.unwrap();

    // The guest reads the host set from the log and checks it from the room key.
    let h = guest.room_hosts(None).await.unwrap();
    assert_eq!(h.root, root);
    assert_eq!((h.hosts.len(), h.k, h.epoch), (3, 2, 0));
    assert!(h.committed >= 2 && h.committed == h.len);
    assert!(guest.room_hosts(Some(root)).await.is_ok());
    let err = guest.room_hosts(Some([9; 32])).await.unwrap_err();
    assert!(format!("{err}").contains("room key"), "{err}");

    // The guest's host dies. The guest goes to another host of the room with the same grant and member id.
    let dead = p.addr();
    p.shutdown().await.unwrap();
    let fresh = guest.failover(&[dead, r1.addr(), r2.addr()]).await.unwrap();
    assert_ne!(fresh.host_id(), guest.host_id());
    assert_eq!(fresh.grant(), guest.grant());
    // It reads the same host set there, and a replica's reply is checked the same way.
    assert_eq!(fresh.room_hosts(Some(root)).await.unwrap().hosts.len(), 3);
    // With nobody to go to, it says so.
    assert!(guest.failover(&[]).await.is_err());
}

#[cfg(unix)]
fn evaluator(dir: &std::path::Path, name: &str, metric: i64, tier: u8) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    let (w, e) = ("8".repeat(64), "e".repeat(64));
    std::fs::write(
        &p,
        format!("#!/bin/sh\necho '{{\"metric\":{metric},\"weights\":\"{w}\",\"env\":\"{e}\",\"tier\":{tier}}}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

// REQ: ROOM-16
#[cfg(unix)]
#[tokio::test]
async fn a_promotion_needs_two_hosts_that_each_ran_the_evaluation_and_agree() {
    let ev = dir("m5-ev");
    let p = Host::open(&dir("m5-p"), Transport::Local, HostConfig::default())
        .await
        .unwrap();
    let root = p.root_public();
    let (r1, r2) = (replica("m5-r1", root).await, replica("m5-r2", root).await);
    p.set_room_evaluator(Some(evaluator(&ev, "p", 1_200_000, 3)));
    r1.set_room_evaluator(Some(evaluator(&ev, "r1", 1_180_000, 1)));
    r2.set_room_evaluator(Some(evaluator(&ev, "r2", 1_600_000, 2))); // far from the others
    p.start_room_log(&[r1.addr().id, r2.addr().id], 2).unwrap();
    for r in [&r1, &r2] {
        r.pull_room_log(&p.addr()).await.unwrap();
    }
    let problem = [7u8; 32];
    // One host's word is never enough, even as a rule.
    assert!(p
        .register_room_problem(problem, 1, 50_000, 50_000, 1_000_000)
        .is_err());
    p.register_room_problem(problem, 2, 50_000, 50_000, 1_000_000)
        .unwrap();
    p.commit_room_log(&[r1.addr(), r2.addr()]).await.unwrap();

    // r2 measured something very different: it is left out, and p and r1 carry it. The claim is the lower number.
    let m = p
        .promote_in_room(&[r1.addr(), r2.addr()], problem, [1u8; 32])
        .await
        .unwrap();
    assert_eq!(m, 1_180_000);
    let s = p.room_log_status().unwrap();
    let f = &s.problems[0];
    assert_eq!((f.metric, f.strength, f.j), (1_180_000, 1, 2), "{f:?}");
    assert!(f.candidate.is_some());

    // The replicas replay the promotion from the log and check the receipts themselves.
    p.commit_room_log(&[r1.addr(), r2.addr()]).await.unwrap();
    r1.pull_room_log(&p.addr()).await.unwrap();
    assert_eq!(r1.room_log_status().unwrap().problems, s.problems);

    // Without a second host answering, there is no promotion.
    r1.shutdown().await.unwrap();
    let e = p
        .promote_in_room(&[r2.addr()], problem, [2u8; 32])
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("distinct host"), "{e}");
    assert_eq!(p.room_log_status().unwrap().problems[0].metric, 1_180_000);
}
