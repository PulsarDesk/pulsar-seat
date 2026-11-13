// SPDX-License-Identifier: GPL-3.0-only
//! Update check (spec 90 §11 item 10): GitHub Releases of `PulsarDesk/pulsar-seat`.
//!
//! - **Opt-in.** No network access unless `settings.updates.check` is on; the weekly check
//!   additionally needs `settings.updates.weekly`. Nothing is sent but a plain HTTPS GET
//!   (generic user agent, no identifiers).
//! - **Check**: `releases/latest` (drafts and pre-releases are never offered), version newer
//!   than this build, an installer asset named [`asset_name`] for the native architecture,
//!   and the release's [`SUMS_ASSET`] for its SHA-256.
//! - **Release signature**: the checksum file must carry [`SUMS_SIG_ASSET`], a minisign
//!   (Ed25519, pre-hashed) signature by one of the release keys in [`ACCEPTED_SIGNERS`]
//!   (`installer/release-signers.txt`, compiled in). The key is kept offline by the
//!   maintainer, so a compromised GitHub account or release page alone cannot change the
//!   installer hash the service accepts. No valid signature: no offer (`sums_unsigned`,
//!   `sums_untrusted`, `no_release_key` when this build lists no key). The signed trusted
//!   comment carries `version:X.Y.Z`, which must equal the release tag (`sums_mismatch`),
//!   and a `timestamp` that may not be older than the newest one this PC accepted before
//!   (`sums_rollback`, kept in `updates\state.json`).
//! - **Limits of the release signature** (no freshness service): whoever controls the
//!   GitHub release page can still *withhold* updates (point `latest` at an old release or
//!   delete the new one: the PC then reports "up to date" or stays on its version), and can
//!   offer a PC an **older validly signed release that is still newer than the installed
//!   one** if that PC never accepted a newer signature (e.g. 0.4.0 to a 0.3.0 PC after 0.5.0
//!   exists). It cannot offer a downgrade below the installed version, a release older than
//!   one this PC already accepted, or any installer the release key did not sign. The
//!   update dialog always shows the offered version and links its release page.
//! - **Download**: into `%ProgramData%\Pulsar Seat\updates\` (SYSTEM + Administrators only),
//!   then SHA-256 against the signed checksum file **and** an Authenticode signature that
//!   chains to a trusted root (WinVerifyTrust). When [`AUTHENTICODE_PINS`] is not empty the
//!   leaf certificate must also match a pin (SHA-256 of its SubjectPublicKeyInfo, plus
//!   subject and issuer names; a name match alone is never enough). An unsigned installer
//!   is never offered for installation.
//! - **Install**: only on an administrator's request with `confirm` (the manager asks
//!   first). A worker verifies the file again (without holding the updater lock, so the
//!   service tick never waits on hashing or revocation checks), starts `msiexec` and
//!   waits for it: if msiexec ends while this service still runs (another install busy,
//!   a launch condition, a bad package), the state becomes `failed` / `install_failed`
//!   and the verified file is kept for a retry. Never automatic, never a silent driver
//!   swap.
//!
//! Network, signature and process launch go through [`UpdateNet`] (WinHTTP / WinVerifyTrust
//! in `win::update`), so the flow is tested with a mock. Untested against a real release:
//! the repository has no published release yet.

use seat_core::config::UpdateSettings;
use seat_proto::{UpdateRelease, UpdateState, UpdateStatus};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Release feed (latest non-draft, non-pre-release).
pub const FEED_URL: &str = "https://api.github.com/repos/PulsarDesk/pulsar-seat/releases/latest";
/// Checksum asset of every release (`sha256sum` format: `<hex>  <name>`).
pub const SUMS_ASSET: &str = "SHA256SUMS.txt";
/// minisign signature of [`SUMS_ASSET`] (`installer/scripts/sign-release.ps1`).
pub const SUMS_SIG_ASSET: &str = "SHA256SUMS.txt.minisig";
/// Release signing keys (public, minisign format), one per line; see the file's header.
/// Compiled in: changing the accepted keys needs a new build.
pub const RELEASE_SIGNERS_FILE: &str = include_str!("../../../installer/release-signers.txt");
/// A pinned signing identity: the leaf code-signing certificate of an installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignerPin {
    /// Leaf subject, simple display name (CN), compared case-insensitively.
    pub subject: &'static str,
    /// Issuing CA, simple display name, compared case-insensitively.
    pub issuer: &'static str,
    /// SHA-256 of the leaf's DER-encoded SubjectPublicKeyInfo, lowercase hex. This is the
    /// part that makes the pin: it survives a re-issue with the same key, not a new key.
    pub spki_sha256: &'static str,
}

/// Optional extra pin on the installer's Authenticode leaf certificate. Empty: any
/// signature that WinVerifyTrust accepts (trusted root, revocation checked) is enough,
/// because the installer hash is already bound to an offline release key
/// ([`ACCEPTED_SIGNERS`]). Fill it once the code-signing certificate is known if both are
/// wanted (docs/release/checklist.md). Rotating: ship a release that lists both pins first.
pub const AUTHENTICODE_PINS: &[SignerPin] = &[];

/// The accepted release keys: the text of `installer/release-signers.txt` (public keys
/// only), compiled in. Parsed by [`accepted_signers`]. **Empty until the maintainer adds
/// the real key**: then no release is ever offered (`no_release_key`).
pub const ACCEPTED_SIGNERS: &str = RELEASE_SIGNERS_FILE;

/// Release keys accepted on `SHA256SUMS.txt.minisig`, parsed from [`ACCEPTED_SIGNERS`]. A
/// malformed file fails the unit test `release_signers_file_parses` (and so CI) and
/// yields no key at run time.
pub fn accepted_signers() -> Vec<seat_release::PublicKey> {
    match seat_release::parse_signers(ACCEPTED_SIGNERS) {
        Ok(k) => k,
        Err(e) => {
            crate::log::warn(&format!("installer/release-signers.txt: {e}; no release key accepted"));
            Vec::new()
        }
    }
}

// A release build (`PULSAR_SEAT_RELEASE` non-empty: CI tag builds, build-installer.ps1
// -Release) must not ship without a release key: its updater could never accept anything.
const RELEASE_BUILD: bool = match option_env!("PULSAR_SEAT_RELEASE") {
    Some(v) => !v.is_empty(),
    None => false,
};
const _: () = assert!(
    !RELEASE_BUILD || seat_release::has_signer_lines(ACCEPTED_SIGNERS),
    "PULSAR_SEAT_RELEASE build without a release key in installer/release-signers.txt (docs/release/checklist.md)"
);

/// Leaf signer of a verified Authenticode signature.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signer {
    pub subject: String,
    pub issuer: String,
    /// SHA-256 of the DER SubjectPublicKeyInfo, lowercase hex.
    pub spki_sha256: String,
}

impl std::fmt::Display for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} (issuer {:?}, key {})", self.subject, self.issuer, self.spki_sha256)
    }
}
/// Hosts an update request may reach (the feed, the asset link and its redirects).
pub const ALLOWED_HOSTS: [&str; 4] =
    ["api.github.com", "github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
/// Generic user agent (GitHub's API refuses requests without one).
pub const USER_AGENT: &str = "pulsar-seat-update-check";
pub const MAX_FEED_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_SUMS_BYTES: u64 = 64 * 1024;
pub const MAX_SIG_BYTES: u64 = seat_release::MAX_SIG_BYTES as u64;
pub const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
/// Weekly check interval.
pub const WEEK_S: u64 = 7 * 24 * 3600;
/// How long the install worker waits for msiexec before giving up on it.
pub const INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const INSTALL_POLL: std::time::Duration = std::time::Duration::from_millis(500);
/// msiexec exit codes that mean "installed" (ok, reboot required, reboot initiated).
const MSI_SUCCESS: [i32; 3] = [0, 3010, 1641];
/// Folder under the data dir.
pub const UPDATES_DIR: &str = "updates";
const STATE_FILE: &str = "state.json";

/// Installer asset name the release workflow uploads (`.github/workflows/ci.yml`).
pub fn asset_name(version: &str, arch: &str) -> String {
    format!("PulsarSeat_{version}_{arch}.msi")
}

/// A failed step: stable `reason` id for the UI plus an English detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
    pub reason: &'static str,
    pub detail: String,
}

impl Fail {
    pub fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self { reason, detail: detail.into() }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.detail)
    }
}

/// Windows side of the updater.
pub trait UpdateNet: Send + Sync {
    /// HTTPS GET into memory (at most `max` bytes). Every hop is checked with [`url_allowed`].
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, Fail>;
    /// HTTPS GET into `dest` (created / truncated), at most `max` bytes; `progress` gets the
    /// running total. Returns the byte count.
    fn download(&self, url: &str, dest: &Path, max: u64, progress: &dyn Fn(u64)) -> Result<u64, Fail>;
    /// Authenticode: Ok(leaf signer) when the signature verifies and chains to a trusted
    /// root; Err(`unsigned`) otherwise.
    fn signer(&self, file: &Path) -> Result<Signer, Fail>;
    /// SHA-256 of a file, lowercase hex.
    fn sha256(&self, file: &Path) -> Result<String, Fail>;
    /// Start the installer detached (`msiexec /i <msi> /qn /norestart /l*v <log>`) and
    /// hand back its process so the caller sees it end.
    fn launch_installer(&self, msi: &Path, log: &Path) -> Result<Box<dyn InstallerRun>, Fail>;
    /// `arm64` / `x64` from the native machine query.
    fn native_arch(&self) -> Option<&'static str>;
    /// Create the private updates folder (SYSTEM + Administrators only).
    fn prepare_dir(&self, dir: &Path) -> Result<(), Fail>;
}

/// A running installer process.
pub trait InstallerRun: Send {
    /// `Some(exit code)` once it ended, `None` while it runs.
    fn try_wait(&mut self) -> Result<Option<i32>, Fail>;
}

/// `1.2.3` or `v1.2.3` →(1, 2, 3). Anything else (pre-release suffix, 4 parts) → None.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.split('.');
    let mut n = [0u64; 3];
    for slot in &mut n {
        let p = it.next()?;
        if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    if it.next().is_some() {
        return None;
    }
    Some((n[0], n[1], n[2]))
}

/// `https://<allowed host>/<path>` with no port, no user info.
pub fn url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains('@') || host.contains(':') || host.is_empty() {
        return false;
    }
    ALLOWED_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host))
}

/// Asset names are used as file names: letters, digits, `.`, `_`, `-` only.
fn safe_file_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && !n.starts_with('.')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[derive(Debug, Deserialize)]
struct FeedAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct Feed {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<FeedAsset>,
}

/// What the feed says, before the checksum file is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedResult {
    UpToDate { latest: String },
    Newer { release: UpdateRelease, installer_url: String, sums_url: Option<String>, sig_url: Option<String> },
}

/// Parse `releases/latest` and pick the installer for `arch`.
pub fn evaluate_feed(json: &[u8], current: &str, arch: &str) -> Result<FeedResult, Fail> {
    let f: Feed = serde_json::from_slice(json).map_err(|e| Fail::new("feed", format!("release feed: {e}")))?;
    let cur = parse_version(current).ok_or_else(|| Fail::new("feed", format!("own version {current} unparsable")))?;
    let Some(v) = parse_version(&f.tag_name) else {
        return Err(Fail::new("feed", format!("release tag {:?} is not vX.Y.Z", f.tag_name)));
    };
    let version = format!("{}.{}.{}", v.0, v.1, v.2);
    if f.draft || f.prerelease || v <= cur {
        return Ok(FeedResult::UpToDate { latest: version });
    }
    let want = asset_name(&version, arch);
    let a = f
        .assets
        .iter()
        .find(|a| a.name == want)
        .ok_or_else(|| Fail::new("no_asset", format!("release {version} has no {want}")))?;
    if !url_allowed(&a.browser_download_url) {
        return Err(Fail::new("feed", format!("asset link not on an allowed host: {}", a.browser_download_url)));
    }
    if a.size > MAX_INSTALLER_BYTES {
        return Err(Fail::new("too_large", format!("{} is {} bytes", a.name, a.size)));
    }
    let sums_url = f.assets.iter().find(|a| a.name == SUMS_ASSET).map(|a| a.browser_download_url.clone());
    let sig_url = f.assets.iter().find(|a| a.name == SUMS_SIG_ASSET).map(|a| a.browser_download_url.clone());
    for u in sums_url.iter().chain(sig_url.iter()) {
        if !url_allowed(u) {
            return Err(Fail::new("feed", format!("checksum link not on an allowed host: {u}")));
        }
    }
    let page_url = if url_allowed(&f.html_url) { f.html_url.clone() } else { String::new() };
    Ok(FeedResult::Newer {
        release: UpdateRelease {
            version,
            tag: f.tag_name.clone(),
            page_url,
            published_at: f.published_at.unwrap_or_default(),
            asset_name: a.name.clone(),
            size: a.size,
            sha256: None,
        },
        installer_url: a.browser_download_url.clone(),
        sums_url,
        sig_url,
    })
}

/// SHA-256 of `name` in a `sha256sum` file (`<64 hex>  <name>` or `<hex> *<name>`).
pub fn parse_sums(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some((hex, rest)) = line.split_once(char::is_whitespace) else { continue };
        let file = rest.trim_start().trim_start_matches('*');
        if file == name && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// A verified checksum signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumsSignature {
    /// Release key that signed (hex key id).
    pub key_id: String,
    /// Signed `timestamp:` of the trusted comment (unix seconds).
    pub timestamp: u64,
}

/// Verify the checksum file against its minisign signature with the release keys, for the
/// release `version` offered by the feed, refusing a signature older than `newest_seen`
/// (the newest timestamp this PC accepted before). Reasons: `no_release_key` (this build
/// lists none), `sums_unsigned` (missing or malformed signature), `sums_untrusted`
/// (unknown key or bad signature), `sums_mismatch` (signed for another version or without
/// one), `sums_rollback` (older than a signature already accepted).
pub fn verify_sums(
    keys: &[seat_release::PublicKey],
    sums: &[u8],
    sig: &[u8],
    version: &str,
    newest_seen: Option<u64>,
) -> Result<SumsSignature, Fail> {
    if keys.is_empty() {
        return Err(Fail::new("no_release_key", "no release signing key in this build (installer/release-signers.txt)"));
    }
    let text = std::str::from_utf8(sig).map_err(|_| Fail::new("sums_unsigned", "checksum signature is not text"))?;
    let v = match seat_release::verify(keys, sums, text) {
        Ok(v) => v,
        Err(e @ seat_release::Error::Format(_)) => {
            return Err(Fail::new("sums_unsigned", format!("checksum signature: {e}")));
        }
        Err(e) => return Err(Fail::new("sums_untrusted", format!("checksum signature: {e}"))),
    };
    // The trusted comment is covered by the signature: these fields are the key holder's.
    match seat_release::trusted_comment_field(&v.trusted_comment, "version") {
        Some(sv) if sv == version => {}
        Some(sv) => {
            return Err(Fail::new("sums_mismatch", format!("checksum file signed for version {sv}, offered as {version}")));
        }
        None => {
            return Err(Fail::new("sums_mismatch", format!("checksum signature names no version ({:?})", v.trusted_comment)));
        }
    }
    let timestamp = seat_release::trusted_comment_field(&v.trusted_comment, "timestamp")
        .and_then(|t| t.parse::<u64>().ok())
        .ok_or_else(|| Fail::new("sums_mismatch", format!("checksum signature has no timestamp ({:?})", v.trusted_comment)))?;
    if let Some(seen) = newest_seen
        && timestamp < seen
    {
        return Err(Fail::new(
            "sums_rollback",
            format!("checksum signature of {version} is older ({timestamp}) than one already accepted ({seen})"),
        ));
    }
    Ok(SumsSignature { key_id: v.key_id, timestamp })
}

/// The signer matches a pin: same key (SPKI SHA-256) **and** same subject and issuer names.
pub fn signer_accepted(s: &Signer, pins: &[SignerPin]) -> bool {
    pins.iter().any(|p| {
        p.spki_sha256.len() == 64
            && p.spki_sha256.eq_ignore_ascii_case(s.spki_sha256.trim())
            && p.subject.eq_ignore_ascii_case(s.subject.trim())
            && p.issuer.eq_ignore_ascii_case(s.issuer.trim())
    })
}

/// Kept across restarts: when the last check finished (weekly timer).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    checked_unix_s: Option<u64>,
    /// Newest signed `timestamp` of an accepted checksum signature (rollback guard).
    #[serde(default)]
    sums_signed_unix_s: Option<u64>,
}

#[derive(Default)]
struct Inner {
    loaded: bool,
    state: UpdateState,
    latest: Option<UpdateRelease>,
    installer_url: Option<String>,
    checked_unix_s: Option<u64>,
    sums_signed_unix_s: Option<u64>,
    downloaded: u64,
    signer: Option<String>,
    file: Option<PathBuf>,
    reason: Option<String>,
    error: Option<String>,
}

/// Update state of the running service.
pub struct Updater {
    current: String,
    /// Release keys for the checksum signature ([`accepted_signers`]).
    keys: Vec<seat_release::PublicKey>,
    /// Optional Authenticode pins ([`AUTHENTICODE_PINS`]).
    pins: &'static [SignerPin],
    inner: Mutex<Inner>,
    net: Mutex<Option<Arc<dyn UpdateNet>>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self::new(crate::service::SERVICE_VERSION)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Updater {
    pub fn new(current: &str) -> Self {
        Self::with_keys(current, accepted_signers(), AUTHENTICODE_PINS)
    }

    /// Updater with other release keys / Authenticode pins (tests).
    pub fn with_keys(current: &str, keys: Vec<seat_release::PublicKey>, pins: &'static [SignerPin]) -> Self {
        Self { current: current.to_string(), keys, pins, inner: Mutex::new(Inner::default()), net: Mutex::new(None) }
    }

    /// Authenticode leaf check. The chain itself was verified by [`UpdateNet::signer`]; the
    /// pin is checked only when [`AUTHENTICODE_PINS`] lists one.
    fn check_signer(&self, s: &Signer) -> Result<(), Fail> {
        if !self.pins.is_empty() && !signer_accepted(s, self.pins) {
            return Err(Fail::new("untrusted_signer", format!("signed by {s}, not a pinned signing identity")));
        }
        Ok(())
    }

    pub fn set_net(&self, net: Arc<dyn UpdateNet>) {
        *self.net.lock().unwrap_or_else(|e| e.into_inner()) = Some(net);
    }

    fn net(&self) -> Result<Arc<dyn UpdateNet>, Fail> {
        self.net
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| Fail::new("unsupported", "no update transport in this build"))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load(&self, data_dir: &Path, i: &mut Inner) {
        if i.loaded {
            return;
        }
        i.loaded = true;
        let p = data_dir.join(UPDATES_DIR).join(STATE_FILE);
        if let Ok(b) = std::fs::read(p)
            && let Ok(s) = serde_json::from_slice::<Persisted>(&b)
        {
            i.checked_unix_s = s.checked_unix_s;
            i.sums_signed_unix_s = s.sums_signed_unix_s;
        }
    }

    fn save(&self, data_dir: &Path, i: &Inner) {
        let p = data_dir.join(UPDATES_DIR).join(STATE_FILE);
        let body = serde_json::to_vec(&Persisted {
            checked_unix_s: i.checked_unix_s,
            sums_signed_unix_s: i.sums_signed_unix_s,
        }).unwrap_or_default();
        if let Err(e) = std::fs::write(&p, body) {
            crate::log::warn(&format!("update state {}: {e}", p.display()));
        }
    }

    pub fn status(&self, s: &UpdateSettings, data_dir: &Path) -> UpdateStatus {
        let arch = self.net().ok().and_then(|n| n.native_arch()).unwrap_or("").to_string();
        let mut i = self.lock();
        self.load(data_dir, &mut i);
        UpdateStatus {
            enabled: s.check,
            weekly: s.weekly,
            current_version: self.current.clone(),
            arch,
            state: i.state,
            latest: i.latest.clone(),
            checked_unix_s: i.checked_unix_s,
            downloaded: i.downloaded,
            signer: i.signer.clone(),
            reason: i.reason.clone(),
            error: i.error.clone(),
        }
    }

    fn busy(i: &Inner) -> bool {
        matches!(i.state, UpdateState::Checking | UpdateState::Downloading | UpdateState::Installing)
    }

    fn fail(&self, i: &mut Inner, f: &Fail) {
        crate::log::warn(&format!("update: {f}"));
        i.state = UpdateState::Failed;
        i.reason = Some(f.reason.to_string());
        i.error = Some(f.detail.clone());
    }

    /// Mark a check as started (opt-in and not busy). The caller then runs [`Self::run_check`].
    pub fn begin_check(&self, s: &UpdateSettings) -> Result<(), Fail> {
        if !s.check {
            return Err(Fail::new("disabled", "update check is off (settings.updates.check)"));
        }
        self.net()?;
        let mut i = self.lock();
        if Self::busy(&i) {
            return Err(Fail::new("busy", "an update step is running"));
        }
        i.state = UpdateState::Checking;
        i.reason = None;
        i.error = None;
        Ok(())
    }

    /// Weekly check due? Marks it started when it is (the caller runs [`Self::run_check`]).
    pub fn begin_weekly(&self, s: &UpdateSettings, data_dir: &Path, now_s: u64) -> bool {
        if !(s.check && s.weekly) || self.net().is_err() {
            return false;
        }
        let mut i = self.lock();
        self.load(data_dir, &mut i);
        if Self::busy(&i) || matches!(i.state, UpdateState::Ready) {
            return false;
        }
        if i.checked_unix_s.is_some_and(|t| now_s.saturating_sub(t) < WEEK_S) {
            return false;
        }
        // Count the attempt so an offline PC does not retry on every tick.
        i.checked_unix_s = Some(now_s);
        i.state = UpdateState::Checking;
        true
    }

    /// Blocking: query the feed and the checksum file.
    pub fn run_check(&self, data_dir: &Path) {
        let newest_seen = {
            let mut i = self.lock();
            self.load(data_dir, &mut i);
            i.sums_signed_unix_s
        };
        let r = self.check_inner(newest_seen);
        let mut i = self.lock();
        i.checked_unix_s = Some(now_unix());
        let r = r.map(|(feed, sha, signed)| {
            if let Some(t) = signed {
                i.sums_signed_unix_s = Some(i.sums_signed_unix_s.map_or(t, |s| s.max(t)));
            }
            (feed, sha)
        });
        match r {
            Ok((FeedResult::UpToDate { latest }, _)) => {
                crate::log::info(&format!("update: up to date ({} , latest {latest})", self.current));
                i.state = UpdateState::UpToDate;
                i.latest = None;
                i.installer_url = None;
                i.file = None;
                i.signer = None;
            }
            Ok((FeedResult::Newer { release, installer_url, .. }, sha)) => {
                crate::log::info(&format!("update: {} available", release.version));
                let same = i.latest.as_ref().is_some_and(|l| l.version == release.version)
                    && i.state == UpdateState::Ready;
                i.latest = Some(UpdateRelease { sha256: sha, ..release });
                i.installer_url = Some(installer_url);
                if !same {
                    i.state = UpdateState::Available;
                    i.file = None;
                    i.signer = None;
                    i.downloaded = 0;
                } else {
                    i.state = UpdateState::Ready;
                }
            }
            Err(f) => self.fail(&mut i, &f),
        }
        if self.net().and_then(|n| n.prepare_dir(&data_dir.join(UPDATES_DIR))).is_ok() {
            self.save(data_dir, &i);
        }
    }

    /// Feed, the installer's SHA-256 and the signed timestamp of the checksum file.
    fn check_inner(&self, newest_seen: Option<u64>) -> Result<(FeedResult, Option<String>, Option<u64>), Fail> {
        let net = self.net()?;
        let arch = net.native_arch().ok_or_else(|| Fail::new("unsupported", "native machine is neither ARM64 nor x64"))?;
        let body = net.get(FEED_URL, MAX_FEED_BYTES)?;
        let r = evaluate_feed(&body, &self.current, arch)?;
        let (sha, signed) = match &r {
            FeedResult::Newer { release, sums_url: Some(u), sig_url, .. } => {
                // The checksum file is used only with a valid release-key signature.
                let sig_url = sig_url.as_ref().ok_or_else(|| {
                    Fail::new("sums_unsigned", format!("release {} has no {SUMS_SIG_ASSET}", release.version))
                })?;
                let sums = net.get(u, MAX_SUMS_BYTES)?;
                let sig = net.get(sig_url, MAX_SIG_BYTES)?;
                let v = verify_sums(&self.keys, &sums, &sig, &release.version, newest_seen)?;
                crate::log::info(&format!(
                    "update: {SUMS_ASSET} of {} signed by release key {} (timestamp {})",
                    release.version, v.key_id, v.timestamp
                ));
                (parse_sums(&String::from_utf8_lossy(&sums), &release.asset_name), Some(v.timestamp))
            }
            _ => (None, None),
        };
        Ok((r, sha, signed))
    }

    /// Mark a download as started (needs state `available` / `failed` with a release, a
    /// checksum, the opt-in).
    pub fn begin_download(&self, s: &UpdateSettings) -> Result<(), Fail> {
        if !s.check {
            return Err(Fail::new("disabled", "update check is off (settings.updates.check)"));
        }
        self.net()?;
        let mut i = self.lock();
        if Self::busy(&i) {
            return Err(Fail::new("busy", "an update step is running"));
        }
        let Some(l) = &i.latest else { return Err(Fail::new("not_ready", "no newer release known; check first")) };
        if l.sha256.is_none() {
            let f = Fail::new("no_checksum", format!("release {} lists no SHA-256 for {}", l.version, l.asset_name));
            self.fail(&mut i, &f);
            return Err(f);
        }
        if i.installer_url.is_none() || !safe_file_name(&l.asset_name) || !l.asset_name.ends_with(".msi") {
            return Err(Fail::new("feed", "installer asset unusable"));
        }
        i.state = UpdateState::Downloading;
        i.downloaded = 0;
        i.reason = None;
        i.error = None;
        Ok(())
    }

    /// Blocking: download, hash, signature.
    pub fn run_download(&self, data_dir: &Path) {
        let r = self.download_inner(data_dir);
        let mut i = self.lock();
        match r {
            Ok((file, signer)) => {
                crate::log::info(&format!("update: {} verified, signed by {signer}", file.display()));
                i.state = UpdateState::Ready;
                i.file = Some(file);
                i.signer = Some(signer);
            }
            Err(f) => {
                i.file = None;
                i.signer = None;
                self.fail(&mut i, &f);
            }
        }
    }

    fn download_inner(&self, data_dir: &Path) -> Result<(PathBuf, String), Fail> {
        let net = self.net()?;
        let (rel, url) = {
            let i = self.lock();
            (i.latest.clone(), i.installer_url.clone())
        };
        let (Some(rel), Some(url)) = (rel, url) else { return Err(Fail::new("not_ready", "no release")) };
        let want = rel.sha256.clone().ok_or_else(|| Fail::new("no_checksum", "no checksum"))?;
        let dir = data_dir.join(UPDATES_DIR);
        net.prepare_dir(&dir)?;
        // Old installers go first (one verified file at a time).
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("msi") || x.eq_ignore_ascii_case("part")) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        let part = dir.join(format!("{}.part", rel.asset_name));
        let file = dir.join(&rel.asset_name);
        let progress = |n: u64| self.lock().downloaded = n;
        let res = (|| {
            net.download(&url, &part, MAX_INSTALLER_BYTES, &progress)?;
            let got = net.sha256(&part)?;
            if got != want {
                return Err(Fail::new("hash_mismatch", format!("SHA-256 {got}, checksum file says {want}")));
            }
            let signer = net.signer(&part)?;
            self.check_signer(&signer)?;
            std::fs::rename(&part, &file).map_err(|e| Fail::new("launch", format!("rename: {e}")))?;
            Ok(signer.subject)
        })();
        match res {
            Ok(signer) => Ok((file, signer)),
            Err(f) => {
                let _ = std::fs::remove_file(&part);
                Err(f)
            }
        }
    }

    /// Mark an install as started: opt-in, confirmation, a verified installer of exactly
    /// `version` (state `ready`, or `failed` after msiexec itself failed or timed out: the
    /// file is kept for a retry). The caller then runs [`Self::run_install`] on a worker.
    pub fn begin_install(&self, s: &UpdateSettings, version: &str, confirm: bool) -> Result<(), Fail> {
        if !s.check {
            return Err(Fail::new("disabled", "update check is off (settings.updates.check)"));
        }
        if !confirm {
            return Err(Fail::new("not_confirmed", "the administrator did not confirm"));
        }
        self.net()?;
        let mut i = self.lock();
        if Self::busy(&i) {
            return Err(Fail::new("busy", "an update step is running"));
        }
        let retry = i.state == UpdateState::Failed && i.reason.as_deref().is_some_and(|r| r.starts_with("install_"));
        if i.state != UpdateState::Ready && !retry {
            return Err(Fail::new("not_ready", "no verified installer"));
        }
        let (Some(_), Some(rel)) = (&i.file, &i.latest) else {
            return Err(Fail::new("not_ready", "no verified installer"));
        };
        if rel.version != version {
            return Err(Fail::new("not_ready", format!("verified installer is {}, not {version}", rel.version)));
        }
        i.state = UpdateState::Installing;
        i.reason = None;
        i.error = None;
        Ok(())
    }

    /// Blocking (worker): verify the file again, start msiexec, wait for it to end.
    /// `logs_dir` gets `update-msi.log`.
    pub fn run_install(&self, logs_dir: &Path) {
        self.run_install_with(logs_dir, INSTALL_POLL, INSTALL_TIMEOUT);
    }

    fn run_install_with(&self, logs_dir: &Path, poll: std::time::Duration, timeout: std::time::Duration) {
        let log = logs_dir.join("update-msi.log");
        let (file, rel) = {
            let i = self.lock();
            (i.file.clone(), i.latest.clone())
        };
        let (Some(file), Some(rel)) = (file, rel) else {
            let mut i = self.lock();
            self.fail(&mut i, &Fail::new("not_ready", "no verified installer"));
            return;
        };
        // Verify again, without the lock (hashing up to 512 MiB and a revocation check
        // must not stall the service tick): the file sits in a SYSTEM/Administrators-only
        // folder, but this is the last moment before it runs with full rights.
        let launched = self.net().and_then(|net| {
            let got = net.sha256(&file)?;
            if Some(&got) != rel.sha256.as_ref() {
                return Err(Fail::new("hash_mismatch", "installer changed since it was verified"));
            }
            self.check_signer(&net.signer(&file)?)?;
            net.launch_installer(&file, &log)
        });
        let mut run = match launched {
            Ok(r) => r,
            Err(f) => {
                let mut i = self.lock();
                self.fail(&mut i, &f);
                if f.reason != "launch" {
                    i.file = None;
                    let _ = std::fs::remove_file(&file);
                }
                return;
            }
        };
        crate::log::info(&format!("update: installer {} started", file.display()));
        // A real upgrade stops this service (and so this worker) before msiexec ends.
        // Anything seen here means msiexec ended without replacing us.
        let started = std::time::Instant::now();
        loop {
            let ended = run.try_wait();
            if let Ok(None) = ended {
                if started.elapsed() < timeout {
                    std::thread::sleep(poll);
                    continue;
                }
            }
            let mut i = self.lock();
            match ended {
                Ok(Some(code)) if MSI_SUCCESS.contains(&code) => {
                    crate::log::warn(&format!("update: msiexec ended with {code} while the service still runs"));
                    i.state = UpdateState::Ready;
                }
                Ok(Some(code)) => {
                    let f = Fail::new("install_failed", format!("msiexec exit code {code}; log {}", log.display()));
                    self.fail(&mut i, &f);
                }
                Ok(None) => {
                    let f = Fail::new(
                        "install_timeout",
                        format!("msiexec still running after {} s; log {}", timeout.as_secs(), log.display()),
                    );
                    self.fail(&mut i, &f);
                }
                Err(f) => {
                    let f = Fail::new("install_failed", format!("{}; log {}", f.detail, log.display()));
                    self.fail(&mut i, &f);
                }
            }
            return;
        }
    }
}

fn proto_err(f: Fail) -> seat_proto::ProtoError {
    use seat_proto::ErrorCode as E;
    let code = match f.reason {
        "disabled" => E::Disabled,
        "busy" | "not_ready" => E::NotReady,
        "unsupported" => E::Unsupported,
        "not_confirmed" => E::InvalidValue,
        _ => E::Os,
    };
    seat_proto::ProtoError::new(code, f.detail).with_reason(f.reason)
}

impl crate::service::Service {
    fn update_settings(&self) -> UpdateSettings {
        self.lock().config.settings.updates.clone()
    }

    pub(crate) fn update_status(&self) -> UpdateStatus {
        self.updater.status(&self.update_settings(), &self.data_dir)
    }

    /// `check_for_update`: answers at once (`checking`), the feed is read on a worker.
    pub(crate) fn update_check(&self) -> Result<UpdateStatus, seat_proto::ProtoError> {
        self.updater.begin_check(&self.update_settings()).map_err(proto_err)?;
        self.spawn(|s| s.updater.run_check(&s.data_dir));
        Ok(self.update_status())
    }

    /// `download_update`: answers at once (`downloading`).
    pub(crate) fn update_download(&self) -> Result<UpdateStatus, seat_proto::ProtoError> {
        self.updater.begin_download(&self.update_settings()).map_err(proto_err)?;
        self.spawn(|s| s.updater.run_download(&s.data_dir));
        Ok(self.update_status())
    }

    /// `install_update`: answers at once (`installing`); a worker verifies the file again,
    /// runs msiexec (the service stops during a real upgrade) and records a failure.
    pub(crate) fn update_install(&self, version: &str, confirm: bool) -> Result<UpdateStatus, seat_proto::ProtoError> {
        self.updater.begin_install(&self.update_settings(), version, confirm).map_err(proto_err)?;
        self.spawn(|s| s.updater.run_install(&s.data_dir.join(seat_core::consts::LOGS_DIR)));
        Ok(self.update_status())
    }

    /// Tick: start the weekly check when it is due (opt-in + weekly on).
    pub(crate) fn update_tick(&self) {
        if self.updater.begin_weekly(&self.update_settings(), &self.data_dir, now_unix()) {
            self.spawn(|s| s.updater.run_check(&s.data_dir));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("v1.2.3-rc1"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.+2.3"), None);
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
    }

    #[test]
    fn urls() {
        assert!(url_allowed("https://github.com/PulsarDesk/pulsar-seat/releases/download/v1.0.0/x.msi"));
        assert!(url_allowed("https://objects.githubusercontent.com/abc?sig=1"));
        assert!(!url_allowed("http://github.com/x"));
        assert!(!url_allowed("https://github.com:444/x"));
        assert!(!url_allowed("https://user@github.com/x"));
        assert!(!url_allowed("https://github.com.evil.example/x"));
        assert!(!url_allowed("https://evil.example/github.com"));
    }

    #[test]
    fn sums() {
        let h = "a".repeat(64);
        let t = format!("{}  other.msi\n{h} *PulsarSeat_1.0.0_arm64.msi\n", "b".repeat(64));
        assert_eq!(parse_sums(&t, "PulsarSeat_1.0.0_arm64.msi"), Some(h));
        assert_eq!(parse_sums(&t, "PulsarSeat_1.0.0_x64.msi"), None);
        assert_eq!(parse_sums("zz  a.msi", "a.msi"), None);
    }

    fn feed(tag: &str, pre: bool, assets: &[&str]) -> Vec<u8> {
        let a: Vec<_> = assets
            .iter()
            .map(|n| {
                serde_json::json!({"name": n, "size": 10,
                "browser_download_url": format!("https://github.com/PulsarDesk/pulsar-seat/releases/download/{tag}/{n}")})
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({"tag_name": tag, "html_url": "https://github.com/PulsarDesk/pulsar-seat/releases/tag/x",
            "draft": false, "prerelease": pre, "assets": a}))
        .unwrap()
    }

    #[test]
    fn feed_selection() {
        let f = feed("v0.2.0", false, &["PulsarSeat_0.2.0_arm64.msi", "PulsarSeat_0.2.0_x64.msi", SUMS_ASSET]);
        match evaluate_feed(&f, "0.1.0", "x64").unwrap() {
            FeedResult::Newer { release, sums_url, sig_url, .. } => {
                assert_eq!(release.asset_name, "PulsarSeat_0.2.0_x64.msi");
                assert!(sums_url.is_some());
                assert!(sig_url.is_none(), "this feed has no {SUMS_SIG_ASSET}");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(evaluate_feed(&f, "0.2.0", "x64").unwrap(), FeedResult::UpToDate { .. }));
        assert!(matches!(evaluate_feed(&f, "0.3.0", "x64").unwrap(), FeedResult::UpToDate { .. }));
        let pre = feed("v0.2.0", true, &["PulsarSeat_0.2.0_x64.msi"]);
        assert!(matches!(evaluate_feed(&pre, "0.1.0", "x64").unwrap(), FeedResult::UpToDate { .. }));
        let no = feed("v0.2.0", false, &["PulsarSeat_0.2.0_x64.msi"]);
        assert_eq!(evaluate_feed(&no, "0.1.0", "arm64").unwrap_err().reason, "no_asset");
        let mut bad: serde_json::Value = serde_json::from_slice(&f).unwrap();
        bad["assets"][1]["browser_download_url"] = "https://evil.example/x.msi".into();
        assert_eq!(evaluate_feed(&serde_json::to_vec(&bad).unwrap(), "0.1.0", "x64").unwrap_err().reason, "feed");
    }

    const KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const PINS: &[SignerPin] = &[SignerPin { subject: "PulsarDesk", issuer: "Test Code Signing CA", spki_sha256: KEY }];

    fn ours() -> Signer {
        Signer { subject: "PulsarDesk".into(), issuer: "Test Code Signing CA".into(), spki_sha256: KEY.into() }
    }

    #[test]
    fn signer_pin_needs_key_subject_and_issuer() {
        assert!(signer_accepted(&ours(), PINS));
        assert!(signer_accepted(&Signer { subject: "pulsardesk ".into(), ..ours() }, PINS));
        // Same name, another key: a different organisation with the same CN.
        assert!(!signer_accepted(&Signer { spki_sha256: "2".repeat(64), ..ours() }, PINS));
        assert!(!signer_accepted(&Signer { issuer: "Other CA".into(), ..ours() }, PINS));
        assert!(!signer_accepted(&Signer { subject: "Someone".into(), ..ours() }, PINS));
        assert!(!signer_accepted(&ours(), &[]));
        let short = [SignerPin { spki_sha256: "", ..PINS[0] }];
        assert!(!signer_accepted(&Signer { spki_sha256: String::new(), ..ours() }, &short));
    }

    struct MockRun(Option<i32>);
    impl InstallerRun for MockRun {
        fn try_wait(&mut self) -> Result<Option<i32>, Fail> {
            Ok(self.0)
        }
    }

    #[derive(Default)]
    struct MockNet {
        pages: BTreeMap<String, Vec<u8>>,
        file: Vec<u8>,
        signer: Option<Signer>,
        launched: Mutex<Vec<PathBuf>>,
        gets: Mutex<u32>,
        /// msiexec exit code the mock reports (None = still running).
        exit: Mutex<Option<i32>>,
    }

    impl UpdateNet for MockNet {
        fn get(&self, url: &str, _max: u64) -> Result<Vec<u8>, Fail> {
            *self.gets.lock().unwrap() += 1;
            self.pages.get(url).cloned().ok_or_else(|| Fail::new("network", "404"))
        }
        fn download(&self, _url: &str, dest: &Path, _max: u64, progress: &dyn Fn(u64)) -> Result<u64, Fail> {
            std::fs::write(dest, &self.file).unwrap();
            progress(self.file.len() as u64);
            Ok(self.file.len() as u64)
        }
        fn signer(&self, _f: &Path) -> Result<Signer, Fail> {
            self.signer.clone().ok_or_else(|| Fail::new("unsigned", "no signature"))
        }
        fn sha256(&self, f: &Path) -> Result<String, Fail> {
            Ok(seat_compat::drivers::sha256_hex(&std::fs::read(f).unwrap()))
        }
        fn launch_installer(&self, msi: &Path, _log: &Path) -> Result<Box<dyn InstallerRun>, Fail> {
            self.launched.lock().unwrap().push(msi.to_path_buf());
            Ok(Box::new(MockRun(*self.exit.lock().unwrap())))
        }
        fn native_arch(&self) -> Option<&'static str> {
            Some("arm64")
        }
        fn prepare_dir(&self, dir: &Path) -> Result<(), Fail> {
            std::fs::create_dir_all(dir).map_err(|e| Fail::new("launch", e.to_string()))
        }
    }

    fn tmp() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "pulsar-seat-update-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Test release key (generated once per test run; never a real key).
    fn release_key() -> &'static seat_release::SecretKey {
        static K: std::sync::OnceLock<seat_release::SecretKey> = std::sync::OnceLock::new();
        K.get_or_init(|| seat_release::SecretKey::generate().unwrap())
    }

    fn asset_url(n: &str) -> String {
        format!("https://github.com/PulsarDesk/pulsar-seat/releases/download/v0.2.0/{n}")
    }

    fn sign_sums(sk: &seat_release::SecretKey, sums: &[u8]) -> Vec<u8> {
        sign_sums_as(sk, sums, "0.2.0", 1_000)
    }

    fn sign_sums_as(sk: &seat_release::SecretKey, sums: &[u8], version: &str, ts: u64) -> Vec<u8> {
        let c = seat_release::release_trusted_comment(ts, SUMS_ASSET, version);
        seat_release::sign(sk, sums, &c, "test").unwrap().into_bytes()
    }

    fn net(signer: Option<Signer>, sums_ok: bool) -> Arc<MockNet> {
        Arc::new(mock_net(signer, sums_ok))
    }

    fn mock_net(signer: Option<Signer>, sums_ok: bool) -> MockNet {
        let file = b"MSI bytes".to_vec();
        let name = "PulsarSeat_0.2.0_arm64.msi";
        let mut n = MockNet { file: file.clone(), signer, ..Default::default() };
        n.pages.insert(FEED_URL.into(), feed("v0.2.0", false, &[name, SUMS_ASSET, SUMS_SIG_ASSET]));
        let h = if sums_ok { seat_compat::drivers::sha256_hex(&file) } else { "0".repeat(64) };
        let sums = format!("{h}  {name}\n").into_bytes();
        n.pages.insert(asset_url(SUMS_SIG_ASSET), sign_sums(release_key(), &sums));
        n.pages.insert(asset_url(SUMS_ASSET), sums);
        n
    }

    const ON: UpdateSettings = UpdateSettings { check: true, weekly: false };
    const NOW: std::time::Duration = std::time::Duration::ZERO;

    fn updater(n: &Arc<MockNet>) -> Updater {
        let u = Updater::with_keys("0.1.0", vec![release_key().public()], PINS);
        u.set_net(n.clone());
        u
    }

    /// Check + download to `ready`.
    fn ready(u: &Updater, d: &Path) {
        u.begin_check(&ON).unwrap();
        u.run_check(d);
        u.begin_download(&ON).unwrap();
        u.run_download(d);
        assert_eq!(u.status(&ON, d).state, UpdateState::Ready, "{:?}", u.status(&ON, d));
    }

    #[test]
    fn opt_in_required() {
        let n = net(Some(ours()), true);
        let u = updater(&n);
        let off = UpdateSettings::default();
        assert_eq!(u.begin_check(&off).unwrap_err().reason, "disabled");
        assert_eq!(u.begin_download(&off).unwrap_err().reason, "disabled");
        assert_eq!(u.begin_install(&off, "0.2.0", true).unwrap_err().reason, "disabled");
        let d = tmp();
        assert!(!u.begin_weekly(&UpdateSettings { check: false, weekly: true }, &d, 1_000_000));
        assert_eq!(*n.gets.lock().unwrap(), 0, "no network access while off");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn full_flow_needs_confirmation_and_reverifies() {
        let d = tmp();
        let n = net(Some(ours()), true);
        let u = updater(&n);
        u.begin_check(&ON).unwrap();
        assert_eq!(u.begin_check(&ON).unwrap_err().reason, "busy");
        u.run_check(&d);
        let st = u.status(&ON, &d);
        assert_eq!(st.state, UpdateState::Available);
        assert_eq!(st.latest.as_ref().unwrap().version, "0.2.0");
        assert!(st.latest.unwrap().sha256.is_some());
        u.begin_download(&ON).unwrap();
        u.run_download(&d);
        let st = u.status(&ON, &d);
        assert_eq!(st.state, UpdateState::Ready, "{st:?}");
        assert_eq!(st.signer.as_deref(), Some("PulsarDesk"));
        assert_eq!(u.begin_install(&ON, "0.2.0", false).unwrap_err().reason, "not_confirmed");
        assert_eq!(u.begin_install(&ON, "0.3.0", true).unwrap_err().reason, "not_ready");
        assert!(n.launched.lock().unwrap().is_empty());
        // Tampered after verification: refused by the worker, file removed.
        let f = d.join(UPDATES_DIR).join("PulsarSeat_0.2.0_arm64.msi");
        std::fs::write(&f, b"evil").unwrap();
        u.begin_install(&ON, "0.2.0", true).unwrap();
        assert_eq!(u.status(&ON, &d).state, UpdateState::Installing);
        assert_eq!(u.begin_install(&ON, "0.2.0", true).unwrap_err().reason, "busy");
        u.run_install_with(&d, NOW, INSTALL_TIMEOUT);
        let st = u.status(&ON, &d);
        assert_eq!((st.state, st.reason.as_deref()), (UpdateState::Failed, Some("hash_mismatch")));
        assert!(!f.exists());
        assert!(n.launched.lock().unwrap().is_empty());
        assert_eq!(u.begin_install(&ON, "0.2.0", true).unwrap_err().reason, "not_ready");
        // Again, untouched this time: the worker launches msiexec.
        u.begin_download(&ON).unwrap();
        u.run_download(&d);
        u.begin_install(&ON, "0.2.0", true).unwrap();
        assert_eq!(n.launched.lock().unwrap().len(), 0, "launch happens on the worker");
        *n.exit.lock().unwrap() = Some(1603);
        u.run_install_with(&d, NOW, INSTALL_TIMEOUT);
        assert_eq!(n.launched.lock().unwrap().len(), 1);
        assert_eq!(u.status(&ON, &d).reason.as_deref(), Some("install_failed"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn msiexec_failure_or_timeout_leaves_installing_and_allows_retry() {
        for (exit, reason) in [(Some(1618), Some("install_failed")), (None, Some("install_timeout")), (Some(0), None)] {
            let d = tmp();
            let n = net(Some(ours()), true);
            *n.exit.lock().unwrap() = exit;
            let u = updater(&n);
            ready(&u, &d);
            u.begin_install(&ON, "0.2.0", true).unwrap();
            u.run_install_with(&d.join("logs"), NOW, NOW);
            let st = u.status(&ON, &d);
            assert_eq!(n.launched.lock().unwrap().len(), 1);
            match reason {
                Some(r) => {
                    assert_eq!((st.state, st.reason.as_deref()), (UpdateState::Failed, Some(r)), "{st:?}");
                    assert!(st.error.as_deref().unwrap().contains("update-msi.log"), "{st:?}");
                }
                None => assert_eq!(st.state, UpdateState::Ready, "{st:?}"),
            }
            // Not stuck: checks run again, and the kept file can be installed again.
            assert!(d.join(UPDATES_DIR).join("PulsarSeat_0.2.0_arm64.msi").exists());
            u.begin_install(&ON, "0.2.0", true).unwrap();
            u.run_install_with(&d, NOW, NOW);
            assert_eq!(n.launched.lock().unwrap().len(), 2);
            u.begin_check(&ON).unwrap();
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn unsigned_or_foreign_signer_or_bad_hash_refused() {
        let same_name_other_key = Signer { spki_sha256: "2".repeat(64), ..ours() };
        for (signer, sums_ok, reason) in [
            (None, true, "unsigned"),
            (Some(Signer { subject: "Someone Else".into(), ..ours() }), true, "untrusted_signer"),
            (Some(same_name_other_key), true, "untrusted_signer"),
            (Some(ours()), false, "hash_mismatch"),
        ] {
            let d = tmp();
            let u = updater(&net(signer, sums_ok));
            u.begin_check(&ON).unwrap();
            u.run_check(&d);
            u.begin_download(&ON).unwrap();
            u.run_download(&d);
            let st = u.status(&ON, &d);
            assert_eq!(st.state, UpdateState::Failed);
            assert_eq!(st.reason.as_deref(), Some(reason));
            let left: Vec<_> = std::fs::read_dir(d.join(UPDATES_DIR))
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x != "json"))
                .collect();
            assert!(left.is_empty(), "nothing unverified stays: {left:?}");
            assert_eq!(u.begin_install(&ON, "0.2.0", true).unwrap_err().reason, "not_ready");
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Check only; returns the state and reason.
    fn check_with(u: &Updater, d: &Path) -> (UpdateState, Option<String>) {
        u.begin_check(&ON).unwrap();
        u.run_check(d);
        let st = u.status(&ON, d);
        (st.state, st.reason)
    }

    #[test]
    fn empty_release_key_list_accepts_nothing() {
        let d = tmp();
        let u = Updater::with_keys("0.1.0", Vec::new(), PINS);
        u.set_net(net(Some(ours()), true));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("no_release_key".into())));
        assert_eq!(u.begin_download(&ON).unwrap_err().reason, "not_ready", "nothing offered");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn checksum_file_needs_a_release_key_signature() {
        let name = "PulsarSeat_0.2.0_arm64.msi";
        let d = tmp();
        // No .minisig asset at all.
        let mut n = MockNet { file: b"MSI bytes".to_vec(), signer: Some(ours()), ..Default::default() };
        n.pages.insert(FEED_URL.into(), feed("v0.2.0", false, &[name, SUMS_ASSET]));
        n.pages.insert(asset_url(SUMS_ASSET), format!("{}  {name}\n", "a".repeat(64)).into_bytes());
        let u = updater(&Arc::new(n));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_unsigned".into())));
        assert!(u.status(&ON, &d).latest.is_none());
        // Signed by a key this build does not accept.
        let other = seat_release::SecretKey::generate().unwrap();
        let mut n = mock_net(Some(ours()), true);
        let sums = n.pages[&asset_url(SUMS_ASSET)].clone();
        n.pages.insert(asset_url(SUMS_SIG_ASSET), sign_sums(&other, &sums));
        let u = updater(&Arc::new(n));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_untrusted".into())));
        // Checksum file changed after signing (e.g. a swapped installer hash on the page).
        let mut n = mock_net(Some(ours()), true);
        n.pages.insert(asset_url(SUMS_ASSET), format!("{}  {name}\n", "b".repeat(64)).into_bytes());
        let u = updater(&Arc::new(n));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_untrusted".into())));
        // Garbage instead of a signature.
        let mut n = mock_net(Some(ours()), true);
        n.pages.insert(asset_url(SUMS_SIG_ASSET), b"not a signature".to_vec());
        let u = updater(&Arc::new(n));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_unsigned".into())));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn checksum_signature_bound_to_version_and_not_older() {
        let d = tmp();
        let with_sig = |sig: Vec<u8>| {
            let mut n = mock_net(Some(ours()), true);
            n.pages.insert(asset_url(SUMS_SIG_ASSET), sig);
            updater(&Arc::new(n))
        };
        let sums = mock_net(Some(ours()), true).pages[&asset_url(SUMS_ASSET)].clone();
        // Signed for another release (a checksum file moved to another tag).
        let u = with_sig(sign_sums_as(release_key(), &sums, "0.3.0", 1_000));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_mismatch".into())));
        // No version in the trusted comment (plain `minisign -S` without -t).
        let plain = seat_release::sign(release_key(), &sums, "timestamp:1000\tfile:SHA256SUMS.txt\thashed", "x").unwrap();
        let u = with_sig(plain.into_bytes());
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_mismatch".into())));
        // Accept timestamp 2000 (persisted in state.json); a later check, by a fresh
        // service, offered a signature from 1500 (an older release re-published) refuses it.
        let u = with_sig(sign_sums_as(release_key(), &sums, "0.2.0", 2_000));
        assert_eq!(check_with(&u, &d).0, UpdateState::Available);
        let u = with_sig(sign_sums_as(release_key(), &sums, "0.2.0", 1_500));
        assert_eq!(check_with(&u, &d), (UpdateState::Failed, Some("sums_rollback".into())));
        // The same or a newer timestamp is still accepted.
        let u = with_sig(sign_sums_as(release_key(), &sums, "0.2.0", 2_000));
        assert_eq!(check_with(&u, &d).0, UpdateState::Available);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn authenticode_pin_list_is_optional() {
        // No pin: any WinVerifyTrust-valid signer is accepted (the hash is bound to the
        // release key); an unsigned installer is still refused.
        let d = tmp();
        let u = Updater::with_keys("0.1.0", vec![release_key().public()], &[]);
        u.set_net(net(Some(Signer { subject: "Any Publisher".into(), ..ours() }), true));
        ready(&u, &d);
        let d2 = tmp();
        let u = Updater::with_keys("0.1.0", vec![release_key().public()], &[]);
        u.set_net(net(None, true));
        u.begin_check(&ON).unwrap();
        u.run_check(&d2);
        u.begin_download(&ON).unwrap();
        u.run_download(&d2);
        assert_eq!(u.status(&ON, &d2).reason.as_deref(), Some("unsigned"));
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(d2);
    }

    #[test]
    fn release_signers_file_parses() {
        // installer/release-signers.txt is compiled in: a malformed line fails here (CI)
        // instead of silently accepting no key at run time.
        let keys = seat_release::parse_signers(ACCEPTED_SIGNERS).expect("installer/release-signers.txt");
        assert_eq!(keys.len(), accepted_signers().len());
        assert_eq!(seat_release::has_signer_lines(ACCEPTED_SIGNERS), !keys.is_empty());
    }

    #[test]
    fn weekly_timer_persists() {
        let d = tmp();
        let wk = UpdateSettings { check: true, weekly: true };
        let u = updater(&net(Some(ours()), true));
        assert!(u.begin_weekly(&wk, &d, 10 * WEEK_S));
        u.run_check(&d);
        let t = u.status(&wk, &d).checked_unix_s.unwrap();
        // A fresh service reads the timestamp back and waits a week.
        let u2 = updater(&net(Some(ours()), true));
        assert!(!u2.begin_weekly(&wk, &d, t + 60));
        assert!(u2.begin_weekly(&wk, &d, t + WEEK_S));
        let _ = std::fs::remove_dir_all(d);
    }
}
