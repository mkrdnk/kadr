#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::PermissionsExt, process::Command, time::Duration};

use kadr_media::players::{PlayRequest, PlayerKind, PlayerOptions, PlayerSession, Telemetry};
use tokio_util::sync::CancellationToken;

// Environment changes happen only in a fresh process, never in a Tokio worker
// or the parallel test harness. The fake bridge records argv without a shell.
#[test]
fn flatpak_host_bridge_and_shared_ipc() {
    for mode in [
        "mpv",
        "vlc",
        "precreated",
        "symlink",
        "permissions",
        "missing",
        "long",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let bridge = root.join("flatpak-spawn");
        fs::write(&bridge, r#"#!/usr/bin/python3
import json, os, socket, sys
args = sys.argv[1:]
assert args[:2] == ['--host', '--watch-bus']
assert args[2] == 'player with spaces'
assert args[-2:] == ['--', 'http://127.0.0.1:1234/private;$(false)?secret=x']
path = next(a.split('=', 1)[1] for a in args if a.startswith(('--input-ipc-server=', '--rc-unix=')))
assert path.startswith(os.environ['XDG_RUNTIME_DIR'] + '/kadr/')
assert os.stat(os.path.dirname(path)).st_mode & 0o777 == 0o700
assert os.stat(os.path.dirname(os.path.dirname(path))).st_mode & 0o777 == 0o700
with open(os.environ['RECORD'], 'w') as f:
    json.dump(args, f)
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)
peer, _ = server.accept()
for line in peer.makefile('rb'):
    if line == b'quit\n':
        break
    request = json.loads(line)
    if request['command'][0] == 'quit':
        break
    peer.sendall((json.dumps(dict(request_id=request['request_id'], error='success', data=5)) + '\n').encode())
"#).unwrap();
        fs::set_permissions(&bridge, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = if mode == "long" {
            root.join("x".repeat(100))
        } else {
            root.join("runtime")
        };
        fs::create_dir(&runtime).unwrap();
        if mode == "symlink" {
            std::os::unix::fs::symlink(root, runtime.join("kadr")).unwrap();
        } else if matches!(mode, "permissions" | "precreated") {
            fs::create_dir(runtime.join("kadr")).unwrap();
            fs::set_permissions(
                runtime.join("kadr"),
                fs::Permissions::from_mode(if mode == "permissions" { 0o777 } else { 0o755 }),
            )
            .unwrap();
        }
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "sandbox_child", "--nocapture"])
            .env("KADR_FLATPAK_TEST", mode)
            .env("FLATPAK_ID", "dev.kadr.Kadr")
            .env("PATH", root)
            .env("RECORD", root.join("record.json"))
            .env("XDG_RUNTIME_DIR", &runtime);
        if mode == "missing" {
            child.env_remove("XDG_RUNTIME_DIR");
        }
        assert!(child.status().unwrap().success(), "{mode}");
        assert_eq!(
            root.join("record.json").exists(),
            matches!(mode, "mpv" | "vlc" | "precreated")
        );
        if matches!(mode, "mpv" | "vlc" | "precreated") {
            assert_eq!(fs::read_dir(runtime.join("kadr")).unwrap().count(), 0);
        }
    }
}

#[test]
fn sandbox_child() {
    let Ok(mode) = std::env::var("KADR_FLATPAK_TEST") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let kind = if mode == "vlc" {
            PlayerKind::Vlc
        } else {
            PlayerKind::Mpv
        };
        let mut options = PlayerOptions::for_player(kind);
        options.executable = "player with spaces".into();
        options.connect_timeout = Duration::from_secs(2);
        let cancel = CancellationToken::new();
        let result = PlayerSession::launch(
            kind,
            PlayRequest {
                media: "http://127.0.0.1:1234/private;$(false)?secret=x".into(),
                resume: Some(Duration::from_secs(2)),
            },
            options,
            &cancel,
        )
        .await;
        if !matches!(mode.as_str(), "mpv" | "vlc" | "precreated") {
            assert!(result.is_err());
            return;
        }
        let mut session = result.unwrap();
        let snapshot = session.snapshot(&cancel).await.unwrap();
        assert_eq!(
            snapshot.position,
            if kind == PlayerKind::Mpv {
                Telemetry::Seconds(5.0)
            } else {
                Telemetry::Unsupported
            }
        );
        assert!(session.stop().await.unwrap().success());
    });
}
