// SPDX-License-Identifier: GPL-3.0-only
//! Real [`HttpsLoopback`]: `ureq` 2 + `rustls` (ring) to `https://127.0.0.1:<base+1>` only.
//!
//! - The server certificate must be byte-identical to the one the seat server created in
//!   its `credentials\cacert.pem`; no CA, name or date check (the certificate is
//!   self-signed with an upstream default subject). Anything else fails the handshake, so
//!   the Basic-auth header is never sent to a listener that is not our seat server.
//! - No proxy (ureq 2 reads proxy variables only when asked to), no redirects, short
//!   timeouts, response bodies capped at 1 MiB.
//! - Only `/api/...` paths. Bodies and the auth header are never logged.

use super::upstream::{HttpsLoopback, WebUiCredentials};
use crate::StreamError;
use base64::Engine as _;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

const MAX_BODY: u64 = 1024 * 1024;

/// DER of the first `CERTIFICATE` block in a PEM file.
pub fn pem_certificate_der(pem: &str) -> Result<Vec<u8>, StreamError> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem.find(BEGIN).ok_or_else(|| StreamError::WebUi("no certificate in PEM".into()))? + BEGIN.len();
    let len = pem[start..].find(END).ok_or_else(|| StreamError::WebUi("unterminated PEM".into()))?;
    let b64: String = pem[start..start + len].chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| StreamError::WebUi(format!("bad PEM: {e}")))
}

#[derive(Debug)]
struct PinnedCert {
    der: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.der.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// rustls client config that accepts exactly `cert_der`.
pub fn pinned_config(cert_der: Vec<u8>) -> Result<Arc<rustls::ClientConfig>, StreamError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedCert { der: cert_der, provider: provider.clone() });
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| StreamError::WebUi(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

pub struct PinnedHttps {
    agent: ureq::Agent,
    port: u16,
    auth: Zeroizing<String>,
}

impl PinnedHttps {
    /// `web_port` = seat base + 1; `cert_pem` = contents of the seat's `cacert.pem`.
    pub fn new(web_port: u16, cert_pem: &str, creds: &WebUiCredentials) -> Result<Self, StreamError> {
        let der = pem_certificate_der(cert_pem)?;
        let agent = ureq::AgentBuilder::new()
            .tls_config(pinned_config(der)?)
            .timeout_connect(Duration::from_secs(2))
            .timeout(Duration::from_secs(15))
            .redirects(0)
            // Kept from Pulsar Split (same reason as `upstream::WEB_UI_USER`); only the
            // seat's own stream server sees it.
            .user_agent("pulsar-split-service")
            .build();
        let mut raw = Zeroizing::new(String::with_capacity(creds.user.len() + creds.password.len() + 1));
        raw.push_str(&creds.user);
        raw.push(':');
        raw.push_str(&creds.password);
        let auth = Zeroizing::new(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())));
        Ok(Self { agent, port: web_port, auth })
    }

    fn read_body(r: ureq::Response) -> Vec<u8> {
        let mut buf = Vec::new();
        let _ = r.into_reader().take(MAX_BODY).read_to_end(&mut buf);
        buf
    }
}

impl HttpsLoopback for PinnedHttps {
    fn request(&mut self, method: &str, path: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>), StreamError> {
        if !path.starts_with("/api/") || path.contains("..") || path.contains(['?', '#', '@']) {
            return Err(StreamError::invalid("path", "only /api/ paths"));
        }
        let url = format!("https://127.0.0.1:{}{path}", self.port);
        let req = self.agent.request(method, &url).set("Authorization", &self.auth).set("Accept", "application/json");
        let res = match body {
            Some(b) => req.set("Content-Type", "application/json").send_bytes(b),
            None => req.call(),
        };
        match res {
            Ok(r) => Ok((r.status(), Self::read_body(r))),
            Err(ureq::Error::Status(code, r)) => Ok((code, Self::read_body(r))),
            Err(ureq::Error::Transport(t)) => Err(StreamError::WebUi(format!("{method} {path}: {}", t.kind()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_parse() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\nAAEC\nAw==\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_certificate_der(pem).unwrap(), [0, 1, 2, 3]);
        assert!(pem_certificate_der("-----BEGIN CERTIFICATE-----\nAAEC").is_err());
        assert!(pem_certificate_der("nothing").is_err());
    }

    #[test]
    fn verifier_accepts_only_pinned_der() {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let v = PinnedCert { der: vec![1, 2, 3], provider };
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let now = UnixTime::now();
        assert!(v.verify_server_cert(&CertificateDer::from(vec![1, 2, 3]), &[], &name, &[], now).is_ok());
        assert!(v.verify_server_cert(&CertificateDer::from(vec![1, 2, 4]), &[], &name, &[], now).is_err());
        assert!(!v.supported_verify_schemes().is_empty());
        pinned_config(vec![1]).unwrap();
    }

    #[test]
    fn only_api_paths_and_no_listener_means_error() {
        let creds = WebUiCredentials { user: "u".into(), password: Zeroizing::new("p".into()), salt: [0; 16] };
        let pem = "-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----";
        // Port 1 on loopback: nothing listens; must fail fast as a transport error.
        let mut h = PinnedHttps::new(1, pem, &creds).unwrap();
        assert_eq!(h.request("GET", "/index.html", None).unwrap_err().code(), "invalid_value");
        assert_eq!(h.request("GET", "/api/../x", None).unwrap_err().code(), "invalid_value");
        assert_eq!(h.request("GET", "/api/clients/list", None).unwrap_err().code(), "web_ui");
    }
}
