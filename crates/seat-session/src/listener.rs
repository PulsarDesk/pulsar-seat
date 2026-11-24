// SPDX-License-Identifier: GPL-3.0-only
//! Loopback listener check before a seat host hands a seat's credentials to
//! `127.0.0.2:<port>`. While TermService is stopped, starting or restarting (boot, a
//! termwrap restart, a crash), any local user can bind that address and would receive the
//! credentials (plaintext with TLS-only security, a crackable NTLMv2 response with
//! CredSSP). This is a local-user attack, not a network one.
//!
//! The rule (pure, [`listener_untrusted_reason`]): TermService must be running with a known
//! process id, at least one TCP listener must accept connections to the target address
//! and port, and **every** listener that could accept them (the exact address or
//! `0.0.0.0`) must belong to TermService's process. The Windows side is
//! `crate::win::listener::verify_rdp_listener` (`QueryServiceStatusEx` +
//! `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_LISTENER)`). The seat host runs it right
//! before every `Connect`; the control's own auto-reconnect is off, so no reconnect can
//! skip it.

use std::net::Ipv4Addr;

/// Reason when TermService's process id is unknown. Before gaming PC E2E round 2 this read
/// only "TermService is not running", also when it ran and the SCM refused the query.
const TERMSERVICE_UNKNOWN: &str = "TermService is not running or its process id could not be read";

/// One IPv4 TCP listener: local address, local port, owning process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpListenerRow {
    pub addr: Ipv4Addr,
    pub port: u16,
    pub pid: u32,
}

/// Pure: TermService's process id for the checks below. The caller's own SCM query wins;
/// when it found nothing (service stopped, or the SCM refused a contained host the
/// `SERVICE_QUERY_STATUS` right) the PID the launching service passed in
/// [`seat_core::consts::TERMSERVICE_PID_ENV`] is used. A stale value (TermService restarted
/// after the launch) fails closed: the new listener's owner no longer matches it.
pub fn termservice_pid(scm: Option<u32>, from_service: Option<&str>) -> Option<u32> {
    scm.filter(|p| *p != 0).or_else(|| from_service.and_then(|s| s.trim().parse::<u32>().ok()).filter(|p| *p != 0))
}

/// Pure: why connecting to `addr:port` may reach someone other than TermService, or
/// `None` when it is safe. `termservice_pid` is `None` when the service is not running
/// or its process id could not be read.
pub fn listener_untrusted_reason(rows: &[TcpListenerRow], addr: Ipv4Addr, port: u16, termservice_pid: Option<u32>) -> Option<String> {
    let Some(ts) = termservice_pid.filter(|p| *p != 0) else {
        return Some(TERMSERVICE_UNKNOWN.into());
    };
    let reach: Vec<&TcpListenerRow> = rows.iter().filter(|r| r.port == port && (r.addr == addr || r.addr.is_unspecified())).collect();
    if reach.is_empty() {
        return Some(format!("nothing listens on {addr}:{port}"));
    }
    reach
        .iter()
        .find(|r| r.pid != ts)
        .map(|r| format!("{}:{} is owned by process {} (TermService is {ts})", r.addr, r.port, r.pid))
}

/// One established IPv4 TCP connection: both endpoints and the owning process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpConnRow {
    pub local: (Ipv4Addr, u16),
    pub remote: (Ipv4Addr, u16),
    pub pid: u32,
}

/// Pure: second check, made once the control reports `OnConnected`. The pre-`Connect`
/// listener snapshot leaves a short window (TOCTOU) in which another process could bind
/// the endpoint. Here every connection `client_pid` holds to `addr:port` must end at a
/// socket owned by TermService. `None` when safe.
pub fn connection_untrusted_reason(
    rows: &[TcpConnRow],
    client_pid: u32,
    addr: Ipv4Addr,
    port: u16,
    termservice_pid: Option<u32>,
) -> Option<String> {
    let Some(ts) = termservice_pid.filter(|p| *p != 0) else {
        return Some(TERMSERVICE_UNKNOWN.into());
    };
    let mine: Vec<&TcpConnRow> = rows.iter().filter(|r| r.pid == client_pid && r.remote == (addr, port)).collect();
    if mine.is_empty() {
        return Some(format!("no connection from this process to {addr}:{port}"));
    }
    for c in mine {
        match rows.iter().find(|r| r.local == c.remote && r.remote == c.local) {
            Some(peer) if peer.pid == ts => {}
            Some(peer) => return Some(format!("{addr}:{port} peer is process {} (TermService is {ts})", peer.pid)),
            None => return Some(format!("peer of {}:{} not found", c.local.0, c.local.1)),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_peer_must_be_termservice() {
        let me = 77;
        let c = |l: (Ipv4Addr, u16), r: (Ipv4Addr, u16), pid| TcpConnRow { local: l, remote: r, pid };
        let ep = (LO2, 3390);
        let mine = (Ipv4Addr::LOCALHOST, 50000);
        let ok = [c(mine, ep, me), c(ep, mine, 900)];
        assert_eq!(connection_untrusted_reason(&ok, me, LO2, 3390, Some(900)), None);
        let rogue = [c(mine, ep, me), c(ep, mine, 4242)];
        assert!(connection_untrusted_reason(&rogue, me, LO2, 3390, Some(900)).unwrap().contains("4242"));
        assert!(connection_untrusted_reason(&ok, me, LO2, 3390, None).is_some());
        assert!(connection_untrusted_reason(&ok[1..], me, LO2, 3390, Some(900)).unwrap().contains("no connection"));
        assert!(connection_untrusted_reason(&ok[..1], me, LO2, 3390, Some(900)).unwrap().contains("not found"));
    }

    #[test]
    fn termservice_pid_falls_back_to_the_service_value() {
        // Own SCM query wins.
        assert_eq!(termservice_pid(Some(900), Some("901")), Some(900));
        // Host account: the SCM refused the query, the service passed the PID (bug A).
        assert_eq!(termservice_pid(None, Some(" 901 ")), Some(901));
        assert_eq!(termservice_pid(Some(0), Some("901")), Some(901));
        // Nothing usable: unknown, so every check fails closed.
        assert_eq!(termservice_pid(None, None), None);
        assert_eq!(termservice_pid(None, Some("0")), None);
        assert_eq!(termservice_pid(None, Some("x")), None);
    }

    const LO2: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

    fn row(a: Ipv4Addr, port: u16, pid: u32) -> TcpListenerRow {
        TcpListenerRow { addr: a, port, pid }
    }

    #[test]
    fn only_termservice_may_listen() {
        let ok = [row(Ipv4Addr::UNSPECIFIED, 3390, 900), row(Ipv4Addr::UNSPECIFIED, 445, 4)];
        assert_eq!(listener_untrusted_reason(&ok, LO2, 3390, Some(900)), None);
        assert!(listener_untrusted_reason(&ok, LO2, 3390, None).unwrap().contains("not running"));
        assert!(listener_untrusted_reason(&ok, LO2, 3390, Some(0)).is_some());
        assert!(listener_untrusted_reason(&[], LO2, 3390, Some(900)).unwrap().contains("nothing listens"));
        // A more specific bind by another process next to TermService's wildcard bind.
        let squat = [row(Ipv4Addr::UNSPECIFIED, 3390, 900), row(LO2, 3390, 4242)];
        assert!(listener_untrusted_reason(&squat, LO2, 3390, Some(900)).unwrap().contains("4242"));
        // TermService gone, a user process holds the port.
        assert!(listener_untrusted_reason(&[row(LO2, 3390, 4242)], LO2, 3390, Some(900)).is_some());
        // Other addresses / ports do not matter.
        let other = [row(LO2, 3390, 900), row(Ipv4Addr::new(127, 0, 0, 3), 3390, 4242), row(LO2, 3391, 4242)];
        assert_eq!(listener_untrusted_reason(&other, LO2, 3390, Some(900)), None);
    }
}
