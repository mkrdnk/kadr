#![cfg(target_os = "linux")]

use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn missing_display_exits_with_an_error_instead_of_hanging() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kadr-app"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("ZED_HEADLESS")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start kadr without a display");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().expect("check kadr process").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("terminate hung kadr process");
            child.wait().expect("reap hung kadr process");
            panic!("kadr did not exit within 10 seconds without a display");
        }
        thread::sleep(Duration::from_millis(10));
    }

    let output = child.wait_with_output().expect("collect kadr error output");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Не удалось открыть окно kadr"),
        "expected a startup error, got: {stderr}"
    );
}
