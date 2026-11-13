// SPDX-License-Identifier: GPL-3.0-only
//! The RDP connection on top of IronRDP (MIT OR Apache-2.0): X.224 → TLS (pinned
//! certificate) → CredSSP/NLA → MCS/capabilities → active session that only answers the
//! server (responses, rdpsnd acks, reactivation). Frames are decoded and dropped.

use crate::emit;
use ironrdp_connector::connection_activation::{ConnectionActivationFactory, ConnectionActivationState};
use ironrdp_connector::sspi::generator::NetworkRequest;
use ironrdp_connector::sspi::network_client::NetworkClient;
use ironrdp_connector::{
    ClientConnector, Config, ConnectorError, ConnectorErrorKind, Credentials, DesktopSize, Sequence as _, ServerName,
};
use ironrdp_core::WriteBuf;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp_pdu::rdp::capability_sets::{MajorPlatformType, RailSupportLevel};
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_rdpsnd::client::{Rdpsnd, RdpsndClientHandler};
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use ironrdp_session::fast_path::ProcessorBuilder;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_blocking::Framed;
use sha1::{Digest, Sha1};
use split_session::helper_proto::{HelperArgs, HelperEvent, HelperFailure, hex20};
use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use zeroize::Zeroizing;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on every read until the session is active (a stuck handshake must not hang).
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(45);
/// Reported to the server as the client build.
const CLIENT_BUILD: u32 = 26100;

pub type Failure = (HelperFailure, String);

/// Plays nothing: offers plain PCM so the server runs its normal paced audio pipeline
/// (spec 70 §2.6); `Rdpsnd` itself acknowledges every wave PDU with its timestamp.
#[derive(Debug)]
struct DiscardAudio {
    formats: Vec<AudioFormat>,
}

pub(crate) fn pcm_formats() -> Vec<AudioFormat> {
    [(44_100u32, 2u16), (48_000, 2), (22_050, 2)]
        .into_iter()
        .map(|(rate, ch)| AudioFormat {
            format: WaveFormat::PCM,
            n_channels: ch,
            n_samples_per_sec: rate,
            n_avg_bytes_per_sec: rate * u32::from(ch) * 2,
            n_block_align: ch * 2,
            bits_per_sample: 16,
            data: None,
        })
        .collect()
}

impl RdpsndClientHandler for DiscardAudio {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }
    fn wave(&mut self, _format: &AudioFormat, _ts: u32, _data: Cow<'_, [u8]>) {}
    fn set_volume(&mut self, _volume: VolumePdu) {}
    fn set_pitch(&mut self, _pitch: PitchPdu) {}
    fn close(&mut self) {}
}

/// Kerberos would need a KDC; this loopback client uses NTLM only.
struct NoNetwork;

impl NetworkClient for NoNetwork {
    fn send(&self, _request: &NetworkRequest) -> ironrdp_connector::sspi::Result<Vec<u8>> {
        Err(ironrdp_connector::sspi::Error::new(ironrdp_connector::sspi::ErrorKind::NoAuthenticatingAuthority, "no KDC: NTLM only"))
    }
}

/// Connector configuration. TLS-only (graphical logon) is off: NLA always (spec 30 §5.2).
pub(crate) fn build_config(a: &HelperArgs, password: &str) -> Config {
    Config {
        desktop_size: DesktopSize { width: a.width as u16, height: a.height as u16 },
        monitor_layout: None,
        desktop_scale_factor: 0,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        enable_tls: false,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword { username: a.user.clone(), password: password.to_owned() },
        // Local account: the machine is the domain.
        domain: std::env::var("COMPUTERNAME").ok().filter(|d| !d.is_empty()),
        client_build: CLIENT_BUILD,
        client_name: a.client_name.clone(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: a.layout,
        // What IronRDP 0.10's connector always sent.
        connection_type: ConnectionType::Lan,
        ime_file_name: String::new(),
        bitmap: None,
        dig_product_id: String::new(),
        client_dir: String::from(r"C:\Windows\System32\mstscax.dll"),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        request_data: None,
        autologon: false,
        // false sets INFO_NOAUDIOPLAYBACK; spec 70 §2.6 wants the session's own endpoint.
        enable_audio_playback: true,
        // INFO_AUDIOCAPTURE (mic, spec 70 §5.1): off, as with IronRDP 0.17 (no such field).
        enable_audio_capture: false,
        performance_flags: PerformanceFlags::DISABLE_WALLPAPER
            | PerformanceFlags::DISABLE_FULLWINDOWDRAG
            | PerformanceFlags::DISABLE_MENUANIMATIONS
            | PerformanceFlags::ENABLE_FONT_SMOOTHING,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: None,
        enable_server_pointer: false,
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
    }
}

/// SHA-1 of the server certificate (DER) must equal the pinned thumbprint.
pub(crate) fn check_cert(der: &[u8], expected: &[u8; 20]) -> Result<(), Failure> {
    let got: [u8; 20] = Sha1::digest(der).into();
    if &got == expected {
        Ok(())
    } else {
        Err((HelperFailure::CertMismatch, format!("server certificate {} is not the pinned {}", hex20(&got), hex20(expected))))
    }
}

pub(crate) fn classify(e: &ConnectorError) -> HelperFailure {
    match e.kind() {
        ConnectorErrorKind::Credssp(_) | ConnectorErrorKind::AccessDenied => HelperFailure::ClientAuth,
        ConnectorErrorKind::Custom | ConnectorErrorKind::General if is_io(e) => HelperFailure::Network,
        _ => HelperFailure::Protocol,
    }
}

fn is_io(e: &ConnectorError) -> bool {
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        if s.is::<std::io::Error>() {
            return true;
        }
        src = s.source();
    }
    false
}

fn conn_err(e: ConnectorError) -> Failure {
    (classify(&e), format!("{}", e.report()))
}

fn io_fail(what: &str, e: std::io::Error) -> Failure {
    (HelperFailure::Network, format!("{what}: {e}"))
}

/// Runs one connection until the server ends it. `Ok(reason)` = normal end.
pub fn run(a: &HelperArgs, password: Zeroizing<String>) -> Result<String, Failure> {
    let addr = SocketAddr::new(a.address, a.port);
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| io_fail("connect", e))?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT)).map_err(|e| io_fail("socket", e))?;
    let local = tcp.local_addr().map_err(|e| io_fail("socket", e))?;

    let config = build_config(a, &password);
    let scrubbed = build_config(a, "");
    let mut connector = ClientConnector::new(config, local);
    if a.audio_playback {
        connector.attach_static_channel(Rdpsnd::new(Box::new(DiscardAudio { formats: pcm_formats() })));
    }
    // INFO_AUDIOCAPTURE (mic, spec 70 §5.1) stays off (`build_config`): `--audio-capture`
    // is accepted and has no effect yet.

    let mut framed = Framed::new(tcp);
    let should = ironrdp_blocking::connect_begin(&mut framed, &mut connector).map_err(conn_err)?;
    let (tcp, leftover) = framed.into_inner();

    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true) // replaced by the SHA-1 pin below
        .danger_accept_invalid_hostnames(true)
        .use_sni(false)
        .build()
        .map_err(|e| (HelperFailure::Tls, e.to_string()))?;
    let tls = tls.connect("localhost", tcp).map_err(|e| (HelperFailure::Tls, e.to_string()))?;
    let der = tls
        .peer_certificate()
        .map_err(|e| (HelperFailure::Tls, e.to_string()))?
        .ok_or_else(|| (HelperFailure::Tls, "server sent no certificate".to_owned()))?
        .to_der()
        .map_err(|e| (HelperFailure::Tls, e.to_string()))?;
    check_cert(&der, &a.cert_sha1)?;
    let public_key = {
        use x509_cert::der::Decode as _;
        let cert = x509_cert::Certificate::from_der(&der).map_err(|e| (HelperFailure::Tls, e.to_string()))?;
        cert.tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| (HelperFailure::Tls, "certificate public key is not byte-aligned".to_owned()))?
            .to_vec()
    };

    let upgraded = ironrdp_blocking::mark_as_upgraded(should, &mut connector);
    let mut framed = Framed::new_with_leftover(tls, leftover);
    let outcome = ironrdp_blocking::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetwork,
        ServerName::new("localhost"),
        public_key,
        None,
    )
    .map_err(conn_err)?;
    let mut result = outcome.into_connected().map_err(conn_err)?;
    // CredSSP is done: drop our copy, and keep no password in the reactivation factory.
    drop(password);
    result.activation_factory =
        ConnectionActivationFactory::new(scrubbed, result.activation_factory.io_channel_id(), result.activation_factory.user_channel_id());
    emit(&HelperEvent::Authenticated);
    // IronRDP does not surface the server's logon-info session id; the service discovers it.
    emit(&HelperEvent::Connected { session_id: None });
    let _ = framed.get_inner_mut().0.get_ref().set_read_timeout(None);
    active(&mut framed, result)
}

fn active<S: Read + Write>(framed: &mut Framed<S>, result: ironrdp_connector::ConnectionResult) -> Result<String, Failure> {
    let factory = result.activation_factory;
    let (io_ch, user_ch) = (factory.io_channel_id(), factory.user_channel_id());
    let mut size = result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
    let mut stage: ActiveStage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: false,
        pointer_software_rendering: true,
    }
    .build();
    loop {
        let (action, payload) = match framed.read_pdu() {
            Ok(x) => x,
            // The server closes the socket on logoff / disconnect.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {
                return Ok("connection closed by server".into());
            }
            Err(e) => return Err(io_fail("read", e)),
        };
        let outputs = stage.process(&mut image, action, &payload, framed.last_read_at()).map_err(|e| (HelperFailure::Protocol, e.to_string()))?;
        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(f) if !f.is_empty() => framed.write_all(&f).map_err(|e| io_fail("write", e))?,
                ActiveStageOutput::Terminate(reason) => return Ok(reason.description()),
                ActiveStageOutput::DeactivateAll => {
                    let mut seq = factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let state = seq.connection_activation_state();
                        if matches!(state, ConnectionActivationState::Redirected { .. }) {
                            return Err((HelperFailure::Protocol, "server redirection during reactivation".into()));
                        }
                        if let ConnectionActivationState::Finalized { desktop_size, share_id, .. } = state {
                            stage.set_fastpath_processor(
                                ProcessorBuilder {
                                    io_channel_id: io_ch,
                                    user_channel_id: user_ch,
                                    share_id,
                                    enable_server_pointer: false,
                                    pointer_software_rendering: true,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            if desktop_size != size {
                                size = desktop_size;
                                image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
                            }
                            break;
                        }
                        buf.clear();
                        let written = match seq.next_pdu_hint() {
                            Some(h) => {
                                let pdu = framed.read_by_hint(h).map_err(|e| io_fail("reactivation read", e))?;
                                seq.step(&pdu, framed.last_read_at(), &mut buf)
                            }
                            None => seq.step_no_input(&mut buf),
                        }
                        .map_err(|e| (HelperFailure::Protocol, e.to_string()))?;
                        if let Some(n) = written.size() {
                            framed.write_all(&buf[..n]).map_err(|e| io_fail("reactivation write", e))?;
                        }
                    }
                }
                _ => {}
            }
        }
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
            client_name: "PSS00AA11BB".into(),
            user: "seat1".into(),
            width: 1920,
            height: 1080,
            layout: 0x41F,
            cert_sha1: [0; 20],
            audio_playback: true,
            audio_capture: false,
            dry_run: false,
        }
    }

    #[test]
    fn config_forces_nla_and_carries_seat_values() {
        let c = build_config(&args(), "pw");
        assert!(c.enable_credssp && !c.enable_tls, "NLA only");
        assert_eq!(c.client_name, "PSS00AA11BB");
        assert_eq!(c.keyboard_layout, 0x41F);
        assert_eq!((c.desktop_size.width, c.desktop_size.height), (1920, 1080));
        assert!(c.enable_audio_playback, "no INFO_NOAUDIOPLAYBACK");
        assert!(!c.autologon);
        assert!(matches!(&c.credentials, Credentials::UsernamePassword { username, password } if username == "seat1" && password == "pw"));
        let s = build_config(&args(), "");
        assert!(matches!(&s.credentials, Credentials::UsernamePassword { password, .. } if password.is_empty()));
    }

    #[test]
    fn cert_pinning() {
        let der = b"not really a certificate";
        let good: [u8; 20] = Sha1::digest(der).into();
        assert!(check_cert(der, &good).is_ok());
        let (code, detail) = check_cert(der, &[0xAA; 20]).unwrap_err();
        assert_eq!(code, HelperFailure::CertMismatch);
        assert!(detail.contains(&"AA".repeat(20)));
    }

    #[test]
    fn audio_formats_are_plain_pcm() {
        let f = pcm_formats();
        assert!(!f.is_empty());
        for x in f {
            assert_eq!(x.format, WaveFormat::PCM);
            assert_eq!(x.n_block_align, x.n_channels * x.bits_per_sample / 8);
            assert_eq!(x.n_avg_bytes_per_sec, x.n_samples_per_sec * u32::from(x.n_block_align));
        }
    }

    #[test]
    fn credssp_errors_are_auth_failures() {
        let e = ConnectorError::new("CredSSP", ConnectorErrorKind::AccessDenied);
        assert_eq!(classify(&e), HelperFailure::ClientAuth);
        let e = ConnectorError::new("x", ConnectorErrorKind::General);
        assert_eq!(classify(&e), HelperFailure::Protocol);
    }
}
