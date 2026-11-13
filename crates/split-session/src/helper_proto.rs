// SPDX-License-Identifier: GPL-3.0-only
//! Protocol between the service and `pulsar-split-rdp-helper.exe` (pure; shared by both).
//!
//! - argv carries no secret (spec 30 §5.2).
//! - stdin carries exactly one password frame, then EOF.
//! - stdout carries JSON Lines, one [`HelperEvent`] per line.

use crate::SessionError;
use serde::{Deserialize, Serialize};
use seat_proto::remote::Secret;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use zeroize::Zeroizing;

pub const HELPER_PROTOCOL: u32 = 1;
/// Longest password frame payload in bytes.
pub const PASSWORD_FRAME_MAX: usize = 512;
pub const MIN_SIZE: u32 = 200;
pub const MAX_SIZE: u32 = 8192;

/// Exit codes of the helper.
pub mod exit {
    /// Server ended the connection normally.
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const BAD_ARGS: i32 = 2;
}

/// Parsed helper command line:
/// `--protocol 1 --port <u16> --address 127.0.0.1|::1 --client-name <PSS…> --user <sam>
///  --width <px> --height <px> --layout <KLID hex> --cert-sha1 <40 hex>
///  [--audio-playback] [--audio-capture] [--dry-run]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    pub protocol: u32,
    pub port: u16,
    pub address: IpAddr,
    pub client_name: String,
    pub user: String,
    pub width: u32,
    pub height: u32,
    /// Keyboard layout id (KLID), e.g. 0x0000041F (Turkish Q).
    pub layout: u32,
    pub cert_sha1: [u8; 20],
    pub audio_playback: bool,
    pub audio_capture: bool,
    /// Validate input, print `Connecting` then `Failed{bad_args}` or `Exited{0}`, no network.
    pub dry_run: bool,
}

fn bad(msg: impl Into<String>) -> SessionError {
    SessionError::HelperProtocol(msg.into())
}

pub fn hex20(bytes: &[u8; 20]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

pub fn parse_hex20(s: &str) -> Option<[u8; 20]> {
    let s = s.trim();
    if s.len() != 40 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl HelperArgs {
    pub fn to_argv(&self) -> Vec<String> {
        let mut v = vec![
            "--protocol".into(),
            self.protocol.to_string(),
            "--port".into(),
            self.port.to_string(),
            "--address".into(),
            self.address.to_string(),
            "--client-name".into(),
            self.client_name.clone(),
            "--user".into(),
            self.user.clone(),
            "--width".into(),
            self.width.to_string(),
            "--height".into(),
            self.height.to_string(),
            "--layout".into(),
            format!("{:08X}", self.layout),
            "--cert-sha1".into(),
            hex20(&self.cert_sha1),
        ];
        for (on, flag) in [(self.audio_playback, "--audio-playback"), (self.audio_capture, "--audio-capture"), (self.dry_run, "--dry-run")] {
            if on {
                v.push(flag.into());
            }
        }
        v
    }

    /// Parses argv *without* the program name.
    pub fn parse(argv: &[String]) -> Result<Self, SessionError> {
        let mut protocol = None;
        let mut port = None;
        let mut address = None;
        let mut client_name = None;
        let mut user = None;
        let mut width = None;
        let mut height = None;
        let mut layout = None;
        let mut cert = None;
        let (mut ap, mut ac, mut dry) = (false, false, false);
        let mut it = argv.iter();
        while let Some(flag) = it.next() {
            let mut val = || it.next().cloned().ok_or_else(|| bad(format!("{flag} needs a value")));
            match flag.as_str() {
                "--protocol" => protocol = Some(val()?.parse::<u32>().map_err(|_| bad("bad --protocol"))?),
                "--port" => port = Some(val()?.parse::<u16>().map_err(|_| bad("bad --port"))?),
                "--address" => address = Some(val()?.parse::<IpAddr>().map_err(|_| bad("bad --address"))?),
                "--client-name" => client_name = Some(val()?),
                "--user" => user = Some(val()?),
                "--width" => width = Some(val()?.parse::<u32>().map_err(|_| bad("bad --width"))?),
                "--height" => height = Some(val()?.parse::<u32>().map_err(|_| bad("bad --height"))?),
                "--layout" => {
                    let v = val()?;
                    if v.is_empty() || v.len() > 8 {
                        return Err(bad("bad --layout"));
                    }
                    layout = Some(u32::from_str_radix(&v, 16).map_err(|_| bad("bad --layout"))?);
                }
                "--cert-sha1" => cert = Some(parse_hex20(&val()?).ok_or_else(|| bad("bad --cert-sha1"))?),
                "--audio-playback" => ap = true,
                "--audio-capture" => ac = true,
                "--dry-run" => dry = true,
                "--password" | "-p" | "/p" => return Err(bad("passwords are never accepted on the command line")),
                other => return Err(bad(format!("unknown argument '{other}'"))),
            }
        }
        let a = Self {
            protocol: protocol.ok_or_else(|| bad("--protocol missing"))?,
            port: port.ok_or_else(|| bad("--port missing"))?,
            address: address.ok_or_else(|| bad("--address missing"))?,
            client_name: client_name.ok_or_else(|| bad("--client-name missing"))?,
            user: user.ok_or_else(|| bad("--user missing"))?,
            width: width.ok_or_else(|| bad("--width missing"))?,
            height: height.ok_or_else(|| bad("--height missing"))?,
            layout: layout.ok_or_else(|| bad("--layout missing"))?,
            cert_sha1: cert.ok_or_else(|| bad("--cert-sha1 missing"))?,
            audio_playback: ap,
            audio_capture: ac,
            dry_run: dry,
        };
        a.validate()?;
        Ok(a)
    }

    pub fn validate(&self) -> Result<(), SessionError> {
        if self.protocol != HELPER_PROTOCOL {
            return Err(bad(format!("protocol {} not supported (want {HELPER_PROTOCOL})", self.protocol)));
        }
        if self.port == 0 {
            return Err(bad("port 0"));
        }
        if self.address != IpAddr::V4(Ipv4Addr::LOCALHOST) && self.address != IpAddr::V6(Ipv6Addr::LOCALHOST) {
            return Err(bad("address must be 127.0.0.1 or ::1"));
        }
        let cn = &self.client_name;
        if cn.is_empty() || cn.len() > 15 || !cn.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(bad("client name must be 1-15 ASCII letters/digits"));
        }
        if self.user.is_empty() || seat_core::remote::validate::user_name(&self.user).is_err() {
            return Err(bad("bad user name"));
        }
        for (n, v) in [("width", self.width), ("height", self.height)] {
            if !(MIN_SIZE..=MAX_SIZE).contains(&v) {
                return Err(bad(format!("{n} must be {MIN_SIZE}-{MAX_SIZE}")));
            }
        }
        if self.layout == 0 {
            return Err(bad("layout 0"));
        }
        Ok(())
    }
}

/// stdin: one frame = u32 LE length + UTF-8 password, then EOF. Max 512 bytes.
pub fn write_password_frame(w: &mut impl Write, pw: &Secret) -> std::io::Result<()> {
    let bytes = pw.0.as_bytes();
    if bytes.is_empty() || bytes.len() > PASSWORD_FRAME_MAX {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "password length out of range"));
    }
    let mut frame = Zeroizing::new(Vec::with_capacity(4 + bytes.len()));
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    w.write_all(&frame)?;
    w.flush()
}

pub fn read_password_frame(r: &mut impl Read) -> Result<Zeroizing<String>, SessionError> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).map_err(|_| bad("password frame missing"))?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > PASSWORD_FRAME_MAX {
        return Err(bad("password frame length out of range"));
    }
    let mut buf = Zeroizing::new(vec![0u8; len]);
    r.read_exact(&mut buf).map_err(|_| bad("password frame truncated"))?;
    let mut extra = [0u8; 1];
    match r.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return Err(bad("data after the password frame")),
        Err(_) => return Err(bad("stdin read failed after the password frame")),
    }
    let s = std::str::from_utf8(&buf).map_err(|_| bad("password is not UTF-8"))?;
    if s.contains('\0') {
        return Err(bad("NUL in password"));
    }
    Ok(Zeroizing::new(s.to_owned()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperFailure {
    ClientAuth,
    CertMismatch,
    Tls,
    Network,
    Protocol,
    BadArgs,
}

/// stdout: JSON Lines, one event per line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HelperEvent {
    Connecting,
    Authenticated,
    Connected { session_id: Option<u32> },
    Disconnected { reason: String },
    Failed { code: HelperFailure, detail: String },
    Exited { code: i32 },
}

impl HelperEvent {
    pub fn to_line(&self) -> String {
        // Serialising this enum cannot fail.
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"state":"exited","code":1}"#.into())
    }

    pub fn parse_line(line: &str) -> Result<Self, SessionError> {
        serde_json::from_str(line.trim()).map_err(|e| bad(format!("bad helper event: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> HelperArgs {
        HelperArgs {
            protocol: 1,
            port: 3389,
            address: "127.0.0.1".parse().unwrap(),
            client_name: "PSS0011AABB".into(),
            user: "seat1".into(),
            width: 1920,
            height: 1080,
            layout: 0x41F,
            cert_sha1: [0xAB; 20],
            audio_playback: true,
            audio_capture: false,
            dry_run: false,
        }
    }

    #[test]
    fn argv_round_trip_without_secrets() {
        let a = args();
        let argv = a.to_argv();
        assert!(argv.contains(&"0000041F".to_string()));
        assert!(argv.contains(&"AB".repeat(20)));
        assert!(!argv.iter().any(|s| s.to_lowercase().contains("pass")));
        assert_eq!(HelperArgs::parse(&argv).unwrap(), a);
        let mut b = a.clone();
        b.address = "::1".parse().unwrap();
        b.audio_capture = true;
        b.dry_run = true;
        assert_eq!(HelperArgs::parse(&b.to_argv()).unwrap(), b);
    }

    #[test]
    fn parse_rejects_bad_input() {
        let base = args().to_argv();
        let with = |flag: &str, v: &str| {
            let mut a = base.clone();
            let i = a.iter().position(|x| x == flag).unwrap();
            a[i + 1] = v.into();
            HelperArgs::parse(&a)
        };
        assert!(with("--protocol", "2").is_err());
        assert!(with("--address", "10.0.0.1").is_err());
        assert!(with("--address", "0.0.0.0").is_err());
        assert!(with("--port", "0").is_err());
        assert!(with("--client-name", "THIS-NAME-IS-TOO-LONG").is_err());
        assert!(with("--width", "50").is_err());
        assert!(with("--layout", "zz").is_err());
        assert!(with("--cert-sha1", "abcd").is_err());
        assert!(with("--user", "a/b").is_err());
        let mut a = base.clone();
        a.push("--password".into());
        a.push("x".into());
        assert!(HelperArgs::parse(&a).unwrap_err().to_string().contains("never"));
        assert!(HelperArgs::parse(&base[2..]).is_err(), "missing --protocol");
        let mut a = base.clone();
        a.push("--port".into());
        assert!(HelperArgs::parse(&a).is_err(), "flag without value");
    }

    #[test]
    fn password_frame_round_trip() {
        let mut buf = Vec::new();
        write_password_frame(&mut buf, &Secret("päss wörd".into())).unwrap();
        assert_eq!(&buf[..4], &(("päss wörd".len()) as u32).to_le_bytes());
        let pw = read_password_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(pw.as_str(), "päss wörd");
    }

    #[test]
    fn password_frame_rejects_bad_frames() {
        assert!(write_password_frame(&mut Vec::new(), &Secret(String::new())).is_err());
        assert!(write_password_frame(&mut Vec::new(), &Secret("x".repeat(513))).is_err());
        assert!(read_password_frame(&mut [1u8, 0].as_slice()).is_err());
        assert!(read_password_frame(&mut [0u8, 0, 0, 0].as_slice()).is_err());
        assert!(read_password_frame(&mut [5u8, 0, 0, 0, b'a'].as_slice()).is_err());
        let mut big = 513u32.to_le_bytes().to_vec();
        big.extend(vec![b'a'; 513]);
        assert!(read_password_frame(&mut big.as_slice()).is_err());
        let mut trailing = Vec::new();
        write_password_frame(&mut trailing, &Secret("pw".into())).unwrap();
        trailing.push(b'x');
        assert!(read_password_frame(&mut trailing.as_slice()).is_err());
        assert!(read_password_frame(&mut [2u8, 0, 0, 0, 0xFF, 0xFE].as_slice()).is_err());
    }

    #[test]
    fn event_lines() {
        let evs = [
            HelperEvent::Connecting,
            HelperEvent::Authenticated,
            HelperEvent::Connected { session_id: Some(3) },
            HelperEvent::Connected { session_id: None },
            HelperEvent::Disconnected { reason: "server initiated disconnect".into() },
            HelperEvent::Failed { code: HelperFailure::CertMismatch, detail: "x".into() },
            HelperEvent::Exited { code: 0 },
        ];
        for e in evs {
            let l = e.to_line();
            assert!(!l.contains('\n'));
            assert_eq!(HelperEvent::parse_line(&l).unwrap(), e);
        }
        assert_eq!(HelperEvent::Connected { session_id: Some(3) }.to_line(), r#"{"state":"connected","session_id":3}"#);
        assert_eq!(
            HelperEvent::Failed { code: HelperFailure::ClientAuth, detail: "d".into() }.to_line(),
            r#"{"state":"failed","code":"client_auth","detail":"d"}"#
        );
        assert!(HelperEvent::parse_line("garbage").is_err());
    }
}
