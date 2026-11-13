// SPDX-License-Identifier: GPL-3.0-only
//! Input validation (spec 10 §3.1, §4.2, §5.1; spec 20 §10). Pure functions: every check
//! takes the data it needs and returns a [`ProtoError`] naming the offending field.

use crate::remote::ports;
use crate::error::{ErrorCode, ProtoError};
use crate::remote::{RemoteSeatRecord, RemoteSettings};

pub const NAME_MAX: usize = 128;
pub const DISPLAY_NAME_MAX: usize = 128;
/// SAM account names are limited to 20 characters.
pub const USER_NAME_MAX: usize = 20;
pub const SCALE_RANGE: (u16, u16) = (100, 500);
pub const SUPER_SAMPLING_RANGE: (u16, u16) = (100, 500);
pub const SDR_WHITE_RANGE: (u16, u16) = (80, 480);
/// `display.refresh_hz` (spec 40 §4.3).
pub const REFRESH_RANGE: (u32, u32) = (15, 500);
/// `stream.min_fps` (`minimum_fps_target`).
pub const MIN_FPS_RANGE: (u32, u32) = (1, 240);

fn err(code: ErrorCode, field: &str, msg: impl Into<String>) -> ProtoError {
    ProtoError::new(code, msg).with_field(field)
}

/// Seat name: 1–128 ASCII letters or digits. It doubles as RDP client name and file prefix.
pub fn seat_name(name: &str) -> Result<(), ProtoError> {
    if name.is_empty() || name.len() > NAME_MAX || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(err(ErrorCode::InvalidName, "name", "seat name must be 1-128 ASCII letters/digits"));
    }
    Ok(())
}

/// Display name: not empty, no leading/trailing whitespace, no control characters (it is
/// written into a `key = value` config line).
pub fn display_name(name: &str) -> Result<(), ProtoError> {
    let bad = name.is_empty()
        || name.chars().count() > DISPLAY_NAME_MAX
        || name.trim() != name
        || name.chars().any(char::is_control);
    if bad {
        return Err(err(ErrorCode::InvalidDisplayName, "display_name", "invalid display name"));
    }
    Ok(())
}

pub fn port(port: u16) -> Result<(), ProtoError> {
    if !ports::is_valid_base(port) {
        return Err(err(
            ErrorCode::InvalidPort,
            "port",
            format!("port must be {}-{}", ports::PORT_MIN, ports::PORT_MAX),
        ));
    }
    Ok(())
}

/// Windows local account name rules (subset that matters here). Empty = not chosen, valid.
pub fn user_name(name: &str) -> Result<(), ProtoError> {
    if name.is_empty() {
        return Ok(());
    }
    const FORBIDDEN: &str = "\"/\\[]:;|=,+*?<>@";
    let bad = name.chars().count() > USER_NAME_MAX
        || name.chars().any(|c| c.is_control() || FORBIDDEN.contains(c))
        || name.chars().all(|c| c == '.' || c == ' ');
    if bad {
        return Err(err(ErrorCode::InvalidUser, "user_name", "invalid Windows user name"));
    }
    Ok(())
}

fn in_range(v: u16, (lo, hi): (u16, u16), field: &str) -> Result<(), ProtoError> {
    if v < lo || v > hi {
        return Err(err(ErrorCode::InvalidValue, field, format!("{field} must be {lo}-{hi}")));
    }
    Ok(())
}

/// Checks of one seat on its own.
pub fn seat_alone(seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
    seat_name(&seat.name)?;
    display_name(&seat.display_name)?;
    port(seat.port)?;
    user_name(&seat.user_name)?;
    let d = &seat.display;
    in_range(d.scale_percent, SCALE_RANGE, "display.scale_percent")?;
    in_range(d.super_sampling_percent, SUPER_SAMPLING_RANGE, "display.super_sampling_percent")?;
    in_range(d.sdr_white_level_nits, SDR_WHITE_RANGE, "display.sdr_white_level_nits")?;
    if !(REFRESH_RANGE.0..=REFRESH_RANGE.1).contains(&d.refresh_hz) {
        return Err(err(ErrorCode::InvalidValue, "display.refresh_hz", "refresh rate must be 15-500 Hz"));
    }
    let l = &d.luminance;
    if l.max_nits == 0 || l.max_nits > 10_000 || l.max_full_frame_nits > l.max_nits {
        return Err(err(ErrorCode::InvalidValue, "display.luminance", "max nits 1-10000, full-frame <= max"));
    }
    if let Some(id) = &d.render_adapter_id
        && (id.is_empty() || id.len() > 512 || id.chars().any(char::is_control))
    {
        return Err(err(ErrorCode::InvalidValue, "display.render_adapter_id", "invalid adapter id"));
    }
    if !(MIN_FPS_RANGE.0..=MIN_FPS_RANGE.1).contains(&seat.stream.min_fps) {
        return Err(err(ErrorCode::InvalidValue, "stream.min_fps", "minimum fps must be 1-240"));
    }
    rdp_client_name(&seat.rdp_client_name)?;
    for app in &seat.autostart {
        if app.path.trim().is_empty() || app.path.chars().any(char::is_control) {
            return Err(err(ErrorCode::InvalidValue, "autostart.path", "autostart path must not be empty"));
        }
        if app.arguments.chars().any(|c| c == '\0') {
            return Err(err(ErrorCode::InvalidValue, "autostart.arguments", "NUL in arguments"));
        }
    }
    Ok(())
}

/// Case-insensitive (Unicode lower case) equality of two display names.
pub fn same_display_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Checks of one seat against the other seats (`others` must not contain the seat itself).
/// Display names must be unique too (spec 80 §7.3): they become the mDNS instance name.
pub fn seat_against(seat: &RemoteSeatRecord, others: &[RemoteSeatRecord]) -> Result<(), ProtoError> {
    seat_alone(seat)?;
    for o in others {
        if o.name.eq_ignore_ascii_case(&seat.name) {
            return Err(err(ErrorCode::DuplicateName, "name", format!("seat '{}' already exists", o.name)));
        }
        if same_display_name(&o.display_name, &seat.display_name) {
            return Err(err(
                ErrorCode::DuplicateName,
                "display_name",
                format!("display name is used by seat '{}'", o.name),
            ));
        }
        if ports::is_valid_base(o.port) && ports::blocks_overlap(o.port, seat.port) {
            return Err(err(ErrorCode::PortOverlap, "port", format!("port block overlaps seat '{}'", o.name)));
        }
        if !seat.user_name.is_empty() && o.user_name.eq_ignore_ascii_case(&seat.user_name) {
            return Err(err(ErrorCode::UserInUse, "user_name", format!("user is used by seat '{}'", o.name)));
        }
        if !seat.rdp_client_name.is_empty() && o.rdp_client_name.eq_ignore_ascii_case(&seat.rdp_client_name) {
            return Err(err(ErrorCode::DuplicateName, "rdp_client_name", format!("client name used by seat '{}'", o.name)));
        }
    }
    Ok(())
}

/// The seat's RDP client name (`PSS` + 8 upper-case hex, contracts §1.1). Empty = not
/// generated yet (seat from protocol v1); the service fills it in before the first start.
pub fn rdp_client_name(name: &str) -> Result<(), ProtoError> {
    if name.is_empty() {
        return Ok(());
    }
    let ok = name.len() == 11
        && name.starts_with("PSS")
        && name[3..].bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
    if !ok {
        return Err(err(ErrorCode::InvalidValue, "rdp_client_name", "client name must be PSS + 8 hex digits"));
    }
    Ok(())
}

pub fn global(g: &RemoteSettings) -> Result<(), ProtoError> {
    if g.steam_isolation && !g.process_patching {
        return Err(err(ErrorCode::InvalidValue, "steam_isolation", "steam isolation needs process patching"));
    }
    Ok(())
}

/// What the service learned about a Windows account (gathered by platform code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserFacts {
    pub exists: bool,
    pub enabled: bool,
    pub locked_out: bool,
    pub password_expired: bool,
}

/// Pre-start user checks (spec 20 §6.2, §10). We never enable/unlock accounts silently.
pub fn user_for_start(
    seat: &RemoteSeatRecord,
    has_password: bool,
    facts: UserFacts,
    console_user: Option<&str>,
) -> Result<(), ProtoError> {
    if seat.user_name.is_empty() || !has_password {
        return Err(err(ErrorCode::MissingCredentials, "user_name", "seat needs a user and a password"));
    }
    if console_user.is_some_and(|c| c.eq_ignore_ascii_case(&seat.user_name)) {
        return Err(err(ErrorCode::UserInUse, "user_name", "user is logged on at the console"));
    }
    let problem = if !facts.exists {
        Some("user does not exist")
    } else if !facts.enabled {
        Some("user account is disabled")
    } else if facts.locked_out {
        Some("user account is locked out")
    } else if facts.password_expired {
        Some("user password has expired")
    } else {
        None
    };
    match problem {
        Some(p) => Err(err(ErrorCode::InvalidUser, "user_name", p)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::AutostartApp;

    fn seat(name: &str, port: u16, user: &str) -> RemoteSeatRecord {
        RemoteSeatRecord {
            id: format!("id-{name}"),
            name: name.into(),
            display_name: name.into(),
            user_name: user.into(),
            port,
            ..Default::default()
        }
    }

    #[test]
    fn seat_names() {
        for ok in ["a", "seat1", "ABC123", &"x".repeat(128)] {
            assert!(seat_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "seat 1", "seat-1", "koltuk_1", "çay", &"x".repeat(129), " a"] {
            let e = seat_name(bad).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidName, "{bad}");
            assert_eq!(e.field.as_deref(), Some("name"));
        }
    }

    #[test]
    fn display_names() {
        assert!(display_name("Salon PC").is_ok());
        assert!(display_name("Çalışma odası").is_ok());
        for bad in ["", " a", "a ", "a\nport = 1", "\t"] {
            assert_eq!(display_name(bad).unwrap_err().code, ErrorCode::InvalidDisplayName, "{bad:?}");
        }
    }

    #[test]
    fn ports_range() {
        assert!(port(1029).is_ok() && port(65514).is_ok());
        assert_eq!(port(1028).unwrap_err().code, ErrorCode::InvalidPort);
        assert_eq!(port(65515).unwrap_err().code, ErrorCode::InvalidPort);
        assert_eq!(port(0).unwrap_err().code, ErrorCode::InvalidPort);
    }

    #[test]
    fn user_names() {
        assert!(user_name("").is_ok());
        assert!(user_name("ali").is_ok());
        assert!(user_name("Oyuncu.2").is_ok());
        for bad in ["a/b", "a\\b", "...", "  ", "a*", &"u".repeat(21)] {
            assert_eq!(user_name(bad).unwrap_err().code, ErrorCode::InvalidUser, "{bad}");
        }
    }

    #[test]
    fn display_ranges() {
        let mut s = seat("a", 47989, "");
        assert!(seat_alone(&s).is_ok());
        s.display.sdr_white_level_nits = 79;
        assert_eq!(seat_alone(&s).unwrap_err().field.as_deref(), Some("display.sdr_white_level_nits"));
        s.display.sdr_white_level_nits = 480;
        s.display.scale_percent = 501;
        assert_eq!(seat_alone(&s).unwrap_err().code, ErrorCode::InvalidValue);
        s.display.scale_percent = 100;
        s.autostart.push(AutostartApp { path: "  ".into(), ..Default::default() });
        assert_eq!(seat_alone(&s).unwrap_err().field.as_deref(), Some("autostart.path"));
    }

    #[test]
    fn duplicate_name_is_case_insensitive() {
        let others = [seat("SeatA", 47989, "")];
        let e = seat_against(&seat("seata", 50000, ""), &others).unwrap_err();
        assert_eq!(e.code, ErrorCode::DuplicateName);
    }

    #[test]
    fn duplicate_display_name_is_case_insensitive() {
        let mut a = seat("a", 48100, "");
        a.display_name = "Çalışma Odası".into();
        let mut b = seat("b", 48200, "");
        b.display_name = "çalışma odası".into();
        let e = seat_against(&b, std::slice::from_ref(&a)).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::DuplicateName, Some("display_name")));
        b.display_name = "Salon".into();
        assert!(seat_against(&b, &[a]).is_ok());
    }

    #[test]
    fn overlapping_port_blocks_rejected() {
        let others = [seat("a", 47989, "")];
        assert_eq!(seat_against(&seat("b", 48000, ""), &others).unwrap_err().code, ErrorCode::PortOverlap);
        assert_eq!(seat_against(&seat("b", 47963, ""), &others).unwrap_err().code, ErrorCode::PortOverlap);
        assert!(seat_against(&seat("b", 48016, ""), &others).is_ok());
        assert!(seat_against(&seat("b", 47962, ""), &others).is_ok());
    }

    #[test]
    fn same_user_on_two_seats_rejected() {
        let others = [seat("a", 47989, "Ali")];
        assert_eq!(seat_against(&seat("b", 48089, "ali"), &others).unwrap_err().code, ErrorCode::UserInUse);
        // Two seats without a user yet are fine.
        let others = [seat("a", 47989, "")];
        assert!(seat_against(&seat("b", 48089, ""), &others).is_ok());
    }

    #[test]
    fn steam_isolation_needs_patching() {
        let mut g = RemoteSettings { steam_isolation: true, ..Default::default() };
        assert!(global(&g).is_err());
        g.process_patching = true;
        assert!(global(&g).is_ok());
    }

    #[test]
    fn start_user_checks() {
        let ok = UserFacts { exists: true, enabled: true, ..Default::default() };
        let s = seat("a", 47989, "ali");
        assert!(user_for_start(&s, true, ok, Some("admin")).is_ok());
        assert_eq!(user_for_start(&s, false, ok, None).unwrap_err().code, ErrorCode::MissingCredentials);
        assert_eq!(
            user_for_start(&seat("a", 47989, ""), true, ok, None).unwrap_err().code,
            ErrorCode::MissingCredentials
        );
        assert_eq!(user_for_start(&s, true, ok, Some("ALI")).unwrap_err().code, ErrorCode::UserInUse);
        for facts in [
            UserFacts { exists: false, ..ok },
            UserFacts { enabled: false, ..ok },
            UserFacts { locked_out: true, ..ok },
            UserFacts { password_expired: true, ..ok },
        ] {
            assert_eq!(user_for_start(&s, true, facts, None).unwrap_err().code, ErrorCode::InvalidUser);
        }
    }
}
