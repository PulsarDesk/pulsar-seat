// SPDX-License-Identifier: GPL-3.0-only
//! Runs `pulsar-seat-host --headless-hold` without network (`--dry-run`) and checks the
//! argv / stdin / stdout protocol end to end (moved from Pulsar Split's helper).

use seat_core::Secret;
use seat_rdp::headless::proto::{HelperArgs, HelperEvent, HelperFailure, SUBCOMMAND, exit, write_password_frame};
use std::io::Write;
use std::process::{Command, Stdio};

fn args() -> HelperArgs {
    HelperArgs {
        protocol: 1,
        port: 3389,
        address: "127.0.0.1".parse().unwrap(),
        client_name: "PSS00AA11BB".into(),
        user: "seat1".into(),
        width: 1280,
        height: 720,
        layout: 0x41F,
        cert_sha1: [0x5A; 20],
        audio_playback: true,
        audio_capture: false,
        dry_run: true,
    }
}

fn run(argv: &[String], stdin: &[u8]) -> (Vec<HelperEvent>, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pulsar-seat-host"))
        .arg(SUBCOMMAND)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn helper");
    {
        let mut i = child.stdin.take().unwrap();
        let _ = i.write_all(stdin);
    } // EOF
    let out = child.wait_with_output().unwrap();
    let events = String::from_utf8(out.stdout).unwrap().lines().map(|l| HelperEvent::parse_line(l).unwrap()).collect();
    (events, out.status.code().unwrap())
}

fn frame(pw: &str) -> Vec<u8> {
    let mut v = Vec::new();
    write_password_frame(&mut v, &Secret(pw.into())).unwrap();
    v
}

#[test]
fn dry_run_accepts_valid_input() {
    let (ev, code) = run(&args().to_argv(), &frame("Gizli-Şifre"));
    assert_eq!(ev, vec![HelperEvent::Connecting, HelperEvent::Exited { code: 0 }]);
    assert_eq!(code, exit::OK);
}

#[test]
fn bad_args_are_reported() {
    let mut a = args().to_argv();
    let i = a.iter().position(|x| x == "--address").unwrap();
    a[i + 1] = "10.0.0.5".into();
    let (ev, code) = run(&a, &frame("pw"));
    assert!(matches!(&ev[0], HelperEvent::Failed { code: HelperFailure::BadArgs, .. }), "{ev:?}");
    assert_eq!(ev.last(), Some(&HelperEvent::Exited { code: exit::BAD_ARGS }));
    assert_eq!(code, exit::BAD_ARGS);
}

#[test]
fn password_on_argv_is_refused() {
    let mut a = args().to_argv();
    a.extend(["--password".into(), "secret".into()]);
    let (ev, code) = run(&a, &frame("pw"));
    assert!(matches!(&ev[0], HelperEvent::Failed { code: HelperFailure::BadArgs, detail } if !detail.contains("secret")));
    assert_eq!(code, exit::BAD_ARGS);
}

#[test]
fn missing_or_bad_password_frame() {
    let (ev, code) = run(&args().to_argv(), b"");
    assert!(matches!(&ev[0], HelperEvent::Failed { code: HelperFailure::BadArgs, .. }));
    assert_eq!(code, exit::BAD_ARGS);
    let mut f = frame("pw");
    f.push(b'!');
    let (_, code) = run(&args().to_argv(), &f);
    assert_eq!(code, exit::BAD_ARGS);
}

#[test]
fn nothing_secret_on_stdout() {
    let (ev, _) = run(&args().to_argv(), &frame("TopSecret99"));
    let text: String = ev.iter().map(HelperEvent::to_line).collect();
    assert!(!text.contains("TopSecret99"));
}

#[test]
fn stdin_password_round_trip_through_the_host() {
    // The frame the service writes is accepted once, and argv never needs a secret.
    let argv = args().to_argv();
    assert!(!argv.iter().any(|a| a.contains("Gizli")));
    let (ev, code) = run(&argv, &frame("Gizli-Şifre-ü"));
    assert_eq!(ev, vec![HelperEvent::Connecting, HelperEvent::Exited { code: exit::OK }]);
    assert_eq!(code, exit::OK);
    // Two frames (or trailing bytes) are refused: exactly one frame, then EOF.
    let mut two = frame("a");
    two.extend(frame("b"));
    let (ev, code) = run(&argv, &two);
    assert!(matches!(&ev[0], HelperEvent::Failed { code: HelperFailure::BadArgs, .. }), "{ev:?}");
    assert_eq!(code, exit::BAD_ARGS);
}

#[test]
fn without_the_subcommand_the_hold_mode_is_not_selected() {
    // Plain hold arguments are not a seat host command line: usage error, no JSON events.
    let out = Command::new(env!("CARGO_BIN_EXE_pulsar-seat-host"))
        .args(args().to_argv())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).lines().all(|l| HelperEvent::parse_line(l).is_err()));
}
