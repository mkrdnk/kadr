#![cfg(unix)]

use std::{fs, io, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use kadr_media::players::{PlayRequest, PlayerKind, PlayerOptions, PlayerSession, Telemetry};
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

struct Fake {
    directory: TempDir,
    executable: PathBuf,
}

impl Fake {
    fn new(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("fake player");
        let record = directory.path().join("record.json");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, socket, sys, time
mode = {mode:?}
args = sys.argv[1:]
path = next(a.split('=', 1)[1] for a in args if a.startswith(('--input-ipc-server=', '--rc-unix=')))
with open({record}, 'w') as f:
    json.dump(dict(pid=os.getpid(), args=args, socket=path), f)
if mode == 'early_exit':
    sys.exit(9)
if mode == 'chatty':
    os.write(1, b'x' * 1000000)
    os.write(2, b'x' * 1000000)
if mode == 'no_connect':
    time.sleep(30)
    sys.exit()
s = socket.socket(socket.AF_UNIX)
s.bind(path)
s.listen(1)
c, _ = s.accept()
if mode == 'exit':
    time.sleep(0.1)
    sys.exit(7)
if mode == 'stalled':
    time.sleep(30)
    sys.exit()
if mode.startswith('queued_'):
    time.sleep(0.05)
    if mode == 'queued_partial':
        c.sendall(b'{{"event":"end-file","reason":"eof"')
    elif mode == 'queued_flood':
        c.sendall(b'{{"event":"tick"}}\n' * 5000 + b'{{"event":"end-file","reason":"eof"}}\n')
    elif mode != 'queued_exit0':
        reason = dict(queued_eof='eof', queued_quit='quit', queued_error='error', queued_nonzero='error')[mode]
        c.sendall(json.dumps(dict(event='end-file', reason=reason)).encode() + b'\n')
    sys.exit(7 if mode == 'queued_nonzero' else 0)
for line in c.makefile('rb'):
    if line == b'quit\n':
        sys.exit()
    request = json.loads(line)
    if request['command'][0] == 'quit':
        if mode == 'ignore_quit':
            time.sleep(30)
        sys.exit()
    if mode == 'malformed':
        c.sendall(b'not JSON\n')
        continue
    if mode == 'oversized':
        c.sendall(b'x' * 70000 + b'\n')
        continue
    if mode == 'eof':
        c.shutdown(socket.SHUT_RDWR)
        c.close()
        time.sleep(30)
        continue
    if mode == 'exit_on_query':
        sys.exit(7)
    if mode == 'eof_on_query':
        c.sendall(b'{{"event":"end-file","reason":"eof"}}\n')
        sys.exit()
    if mode == 'partial':
        c.sendall(b'{{"request_id":')
        time.sleep(30)
        continue
    ident = request['request_id']
    if mode == 'flood':
        while True:
            c.sendall(b'{{"event":"tick"}}\n')
    # Events and wrong IDs must not be mistaken for position/duration.
    c.sendall(b'{{"event":"file-loaded"}}\n')
    if mode in ('eof_event', 'quit_event', 'error_event', 'unrelated_eof'):
        reason = dict(eof_event='eof', quit_event='quit', error_event='error', unrelated_eof='eof')[mode]
        event = dict(event='playback-restart' if mode == 'unrelated_eof' else 'end-file', reason=reason, request_id=ident+100)
        c.sendall(json.dumps(event).encode() + b'\n')
    c.sendall(json.dumps(dict(request_id=ident+100, error='success', data=999)).encode() + b'\n')
    prop = request['command'][1]
    reply = dict(request_id=ident, error='success', data=12.5 if prop == 'time-pos' else 60)
    if mode == 'near_end':
        reply['data'] = 60
    if mode == 'unavailable':
        reply = dict(request_id=ident, error='property unavailable')
    if mode == 'negative':
        reply['data'] = -1
    c.sendall(json.dumps(reply).encode() + b'\n')
"#,
            record = serde_json::to_string(record.to_str().unwrap()).unwrap()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            executable,
        }
    }

    fn options(&self, kind: PlayerKind) -> PlayerOptions {
        PlayerOptions {
            executable: self.executable.clone(),
            connect_timeout: Duration::from_secs(1),
            command_timeout: Duration::from_millis(120),
            stop_timeout: Duration::from_millis(120),
            ..PlayerOptions::for_player(kind)
        }
    }

    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(self.directory.path().join("record.json")).unwrap())
            .unwrap()
    }

    async fn wait_for_record(&self) -> Value {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(bytes) = fs::read(self.directory.path().join("record.json"))
                    && let Ok(record) = serde_json::from_slice(&bytes)
                {
                    return record;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn launch(&self, kind: PlayerKind) -> PlayerSession {
        PlayerSession::launch(
            kind,
            request(),
            self.options(kind),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
    }
}

fn request() -> PlayRequest {
    PlayRequest {
        media: "--not-an-option; $(touch should-not-exist) & \"private\"".into(),
        resume: Some(Duration::from_millis(3500)),
    }
}

async fn assert_reaped(pid: u32) {
    timeout(Duration::from_secs(3), async {
        loop {
            // Signal zero only probes existence; it does not signal a process.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } == -1 {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child must be killed and reaped");
}

#[tokio::test]
async fn mpv_argv_private_socket_correlated_telemetry_and_graceful_stop() {
    let fake = Fake::new("normal");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    let record = fake.record();
    let args = record["args"].as_array().unwrap();
    assert_eq!(args.last().unwrap(), request().media.to_str().unwrap());
    assert_eq!(args[args.len() - 2], "--");
    assert!(args.iter().any(|arg| arg == "--start=3.500000"));
    assert!(args.iter().any(|arg| arg == "--no-config"));
    let path = PathBuf::from(record["socket"].as_str().unwrap());
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert_eq!(snapshot.position, Telemetry::Seconds(12.5));
    assert_eq!(snapshot.duration, Telemetry::Seconds(60.0));
    assert!(snapshot.exit.is_none());
    assert!(!snapshot.completed);
    assert!(session.stop().await.unwrap().success());
    assert!(session.stop().await.unwrap().success());
    assert!(
        !session
            .snapshot(&CancellationToken::new())
            .await
            .unwrap()
            .completed
    );
    drop(session);
    assert!(!path.parent().unwrap().exists());
    assert_reaped(record["pid"].as_u64().unwrap() as u32).await;
}

#[tokio::test]
async fn vlc_resume_is_argv_and_telemetry_is_explicitly_unsupported() {
    let fake = Fake::new("normal");
    let mut session = fake.launch(PlayerKind::Vlc).await;
    let record = fake.record();
    let args = record["args"].as_array().unwrap();
    assert!(args.iter().any(|arg| arg == "--start-time=3.500000"));
    assert!(args.iter().any(|arg| arg == "--play-and-exit"));
    assert_eq!(args.last().unwrap(), request().media.to_str().unwrap());
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert_eq!(snapshot.position, Telemetry::Unsupported);
    assert_eq!(snapshot.duration, Telemetry::Unsupported);
    assert!(!snapshot.completed);
    assert!(session.stop().await.unwrap().success());
}

#[tokio::test]
async fn unavailable_is_not_zero_or_estimated_progress() {
    let fake = Fake::new("unavailable");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert_eq!(snapshot.position, Telemetry::Unavailable);
    assert_eq!(snapshot.duration, Telemetry::Unavailable);
    session.stop().await.unwrap();
}

#[tokio::test]
async fn invalid_disconnected_oversized_or_stalled_ipc_fails_closed() {
    for mode in [
        "malformed",
        "oversized",
        "negative",
        "eof",
        "partial",
        "flood",
    ] {
        let fake = Fake::new(mode);
        let mut session = fake.launch(PlayerKind::Mpv).await;
        timeout(Duration::from_secs(2), async {
            assert!(session.snapshot(&CancellationToken::new()).await.is_err());
            // No stale reply can be treated as a new property.
            if session.try_wait().unwrap().is_none() {
                match session.snapshot(&CancellationToken::new()).await {
                    Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotConnected),
                    Ok(snapshot) => {
                        // A flood producer can exit on BrokenPipe between
                        // try_wait above and snapshot's own process check.
                        assert!(snapshot.exit.is_some());
                        assert_eq!(snapshot.position, Telemetry::Unavailable);
                    }
                }
            }
            session.stop().await.unwrap();
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn startup_timeout_and_cancel_join_the_child() {
    for cancel_launch in [false, true] {
        let fake = Fake::new("no_connect");
        let cancel = CancellationToken::new();
        let mut options = fake.options(PlayerKind::Mpv);
        options.connect_timeout = Duration::from_millis(150);
        let launch = PlayerSession::launch(PlayerKind::Mpv, request(), options, &cancel);
        let cancellation = async {
            let record = fake.wait_for_record().await;
            if cancel_launch {
                cancel.cancel();
            }
            record
        };
        let (result, record) = tokio::join!(launch, cancellation);
        let error = result.err().expect("launch must fail");
        assert_eq!(
            error.kind(),
            if cancel_launch {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::TimedOut
            }
        );
        let path = PathBuf::from(record["socket"].as_str().unwrap());
        assert!(!path.parent().unwrap().exists());
        assert_reaped(record["pid"].as_u64().unwrap() as u32).await;
    }
}

#[tokio::test]
async fn dropped_launch_future_kills_child_and_removes_directory() {
    let fake = Fake::new("no_connect");
    let cancel = CancellationToken::new();
    let mut launch = Box::pin(PlayerSession::launch(
        PlayerKind::Mpv,
        request(),
        fake.options(PlayerKind::Mpv),
        &cancel,
    ));
    let record = tokio::select! {
        _ = &mut launch => panic!("fake should not connect"),
        record = fake.wait_for_record() => record,
    };
    drop(launch);
    assert_reaped(record["pid"].as_u64().unwrap() as u32).await;
    let path = PathBuf::from(record["socket"].as_str().unwrap());
    assert!(!path.parent().unwrap().exists());
}

#[tokio::test]
async fn snapshot_cancellation_and_future_drop_poison_ipc_but_preserve_stop() {
    for use_token in [false, true] {
        let fake = Fake::new("partial");
        let mut session = fake.launch(PlayerKind::Mpv).await;
        let cancel = CancellationToken::new();
        if use_token {
            cancel.cancel();
            assert_eq!(
                session.snapshot(&cancel).await.unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
        } else {
            assert!(
                timeout(Duration::from_millis(30), session.snapshot(&cancel))
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            session
                .snapshot(&CancellationToken::new())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotConnected
        );
        session.stop().await.unwrap();
    }
}

#[tokio::test]
async fn stop_escalates_and_drop_reaps_without_stop() {
    let fake = Fake::new("ignore_quit");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    let status = timeout(Duration::from_secs(2), session.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    let fake = Fake::new("normal");
    let session = fake.launch(PlayerKind::Mpv).await;
    let record = fake.record();
    drop(session);
    assert_reaped(record["pid"].as_u64().unwrap() as u32).await;
    assert!(!PathBuf::from(record["socket"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn dropping_stop_future_allows_retry_and_does_not_detach_the_child() {
    let fake = Fake::new("ignore_quit");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    let record = fake.record();
    assert!(
        timeout(Duration::from_millis(30), session.stop())
            .await
            .is_err()
    );
    session.stop().await.unwrap();
    assert_reaped(record["pid"].as_u64().unwrap() as u32).await;
}

#[tokio::test]
async fn noisy_child_cannot_block_on_stdout_or_stderr_pipes() {
    let fake = Fake::new("chatty");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    assert_eq!(
        session
            .snapshot(&CancellationToken::new())
            .await
            .unwrap()
            .position,
        Telemetry::Seconds(12.5)
    );
    session.stop().await.unwrap();
}

#[tokio::test]
async fn early_exit_and_precancelled_launch_leave_no_resources() {
    let fake = Fake::new("early_exit");
    let result = PlayerSession::launch(
        PlayerKind::Mpv,
        request(),
        fake.options(PlayerKind::Mpv),
        &CancellationToken::new(),
    )
    .await;
    assert!(result.err().unwrap().to_string().contains("exited"));
    let record = fake.record();
    assert_reaped(record["pid"].as_u64().unwrap() as u32).await;
    assert!(
        !PathBuf::from(record["socket"].as_str().unwrap())
            .parent()
            .unwrap()
            .exists()
    );
    let fake = Fake::new("normal");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = PlayerSession::launch(
        PlayerKind::Mpv,
        request(),
        fake.options(PlayerKind::Mpv),
        &cancel,
    )
    .await;
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
    assert!(!fake.directory.path().join("record.json").exists());
}

#[tokio::test]
async fn natural_exit_is_detected_and_not_reported_as_successful_completion() {
    for kind in [PlayerKind::Mpv, PlayerKind::Vlc] {
        let fake = Fake::new("exit");
        let mut session = fake.launch(kind).await;
        timeout(Duration::from_secs(2), async {
            while session.try_wait().unwrap().is_none() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
        assert_eq!(snapshot.exit.unwrap().code(), Some(7));
        assert!(!snapshot.completed);
        assert_eq!(session.stop().await.unwrap().code(), Some(7));
    }
}

#[tokio::test]
async fn ipc_closing_just_before_process_exit_is_resolved_without_fake_progress() {
    let fake = Fake::new("exit_on_query");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert_eq!(snapshot.position, Telemetry::Unavailable);
    assert_eq!(snapshot.duration, Telemetry::Unavailable);
    assert_eq!(snapshot.exit.unwrap().code(), Some(7));
    assert!(!snapshot.completed);
    assert_eq!(session.stop().await.unwrap().code(), Some(7));
}

#[tokio::test]
async fn eof_during_a_property_request_survives_the_transport_exit_race() {
    let fake = Fake::new("eof_on_query");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert!(snapshot.completed);
    assert!(snapshot.exit.unwrap().success());
    assert_eq!(snapshot.position, Telemetry::Unavailable);
    assert_eq!(snapshot.duration, Telemetry::Unavailable);
    session.stop().await.unwrap();
}

#[tokio::test]
async fn completion_uses_only_exact_mpv_eof_events_independent_of_request_id() {
    for mode in [
        "eof_event",
        "quit_event",
        "error_event",
        "unrelated_eof",
        "near_end",
    ] {
        let fake = Fake::new(mode);
        let mut session = fake.launch(PlayerKind::Mpv).await;
        let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
        assert_eq!(snapshot.completed, mode == "eof_event", "{mode}");
        assert_eq!(snapshot.duration, Telemetry::Seconds(60.0));
        if mode == "near_end" {
            assert_eq!(snapshot.position, snapshot.duration);
            assert!(!snapshot.completed, "near/end position is not an EOF event");
        }
        assert!(
            snapshot.exit.is_none(),
            "event is not process-exit evidence"
        );
        session.stop().await.unwrap();
        assert_eq!(
            session
                .snapshot(&CancellationToken::new())
                .await
                .unwrap()
                .completed,
            mode == "eof_event",
            "observed EOF stays latched"
        );
    }
}

#[tokio::test]
async fn queued_eof_is_drained_after_child_exit_but_quit_error_or_zero_exit_are_not_completion() {
    for mode in [
        "queued_eof",
        "queued_quit",
        "queued_error",
        "queued_nonzero",
        "queued_exit0",
        "queued_partial",
    ] {
        let fake = Fake::new(mode);
        let mut session = fake.launch(PlayerKind::Mpv).await;
        timeout(Duration::from_secs(2), async {
            while session.try_wait().unwrap().is_none() {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
        assert_eq!(snapshot.completed, mode == "queued_eof", "{mode}");
        assert_eq!(snapshot.position, Telemetry::Unavailable);
        assert_eq!(snapshot.duration, Telemetry::Unavailable);
        assert_eq!(
            snapshot.exit.unwrap().code(),
            Some(if mode == "queued_nonzero" { 7 } else { 0 })
        );
        session.stop().await.unwrap();
    }
}

#[tokio::test]
async fn exit_drain_has_an_aggregate_budget_and_never_skips_a_flood_to_find_eof() {
    let fake = Fake::new("queued_flood");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    timeout(Duration::from_secs(2), async {
        while session.try_wait().unwrap().is_none() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let snapshot = timeout(
        Duration::from_millis(300),
        session.snapshot(&CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !snapshot.completed,
        "EOF beyond drain budget was not observed"
    );
    assert!(
        !session
            .snapshot(&CancellationToken::new())
            .await
            .unwrap()
            .completed,
        "partial drain must close IPC rather than reuse its suffix"
    );
    session.stop().await.unwrap();
}

#[tokio::test]
async fn cancelled_exit_drain_closes_ipc_without_inventing_completion() {
    let fake = Fake::new("queued_eof");
    let mut session = fake.launch(PlayerKind::Mpv).await;
    timeout(Duration::from_secs(2), async {
        while session.try_wait().unwrap().is_none() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(!session.snapshot(&cancel).await.unwrap().completed);
    assert!(
        !session
            .snapshot(&CancellationToken::new())
            .await
            .unwrap()
            .completed,
        "cancelled drain cannot leave unread events reusable"
    );
    session.stop().await.unwrap();
}

#[tokio::test]
async fn missing_executable_error_is_clear_and_does_not_leak_media() {
    let mut options = PlayerOptions::for_player(PlayerKind::Mpv);
    options.executable = "/definitely/missing/kadr-player".into();
    let error = PlayerSession::launch(
        PlayerKind::Mpv,
        request(),
        options,
        &CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains("installation"));
    assert!(!error.to_string().contains("private"));
}
