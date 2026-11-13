// SPDX-License-Identifier: GPL-3.0-only
//! DXVK per game, opt-in (spec 50 §10.2): DXVK's documented deployment, i.e. its
//! `d3d8.dll` / `d3d9.dll` copied next to the game executable, with backup and undo. No
//! redirect of system DLL loads, no DXVK binaries in our repo or installer: the release is
//! downloaded at install / first use, its SHA-256 is checked against the installer manifest,
//! and every copied file is re-checked against the hashes recorded at download.
//!
//! DXVK is zlib-licensed; [`DXVK_NOTICE`] is written next to the extracted release and shown
//! in the manager's About screen. DXVK needs a Vulkan-capable GPU driver.

use crate::error::ShimError;
use crate::journal::{Journal, RecordKind, RestoreLog, absent, backup, file_sha256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// DXVK's licence text (from its `LICENSE` file, upstream `doitsujin/dxvk`).
pub const DXVK_NOTICE: &str = "\
DXVK (https://github.com/doitsujin/dxvk)

                  Copyright (c) 2017 Philip Rebohle
                  Copyright (c) 2019 Joshua Ashton
                  Copyright (c) 2019 Robin Kertels
                  Copyright (c) 2023 Jeffrey Ellison

                          zlib/libpng license

This software is provided 'as-is', without any express or implied
warranty. In no event will the authors be held liable for any damages
arising from the use of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it
freely, subject to the following restrictions:

\u{2013} The origin of this software must not be misrepresented; you must not
  claim that you wrote the original software. If you use this software
  in a product, an acknowledgment in the product documentation would be
  appreciated but is not required.

\u{2013} Altered source versions must be plainly marked as such, and must not
  be misrepresented as being the original software.

\u{2013} This notice may not be removed or altered from any source distribution.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bitness {
    X86,
    X64,
}

impl Bitness {
    /// Folder name inside a DXVK release.
    pub fn release_dir(self) -> &'static str {
        match self {
            Self::X86 => "x32",
            Self::X64 => "x64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DxvkDll {
    D3d8,
    D3d9,
}

impl DxvkDll {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::D3d8 => "d3d8.dll",
            Self::D3d9 => "d3d9.dll",
        }
    }
}

/// From the installer manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DxvkRelease {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

impl DxvkRelease {
    /// Latest upstream release when this was written (2026-11-12). The hash is GitHub's
    /// asset digest, re-checked here by downloading the file once. The installer manifest
    /// may name a newer one; that wins.
    pub fn default_release() -> Self {
        Self {
            version: "3.1.1".into(),
            url: "https://github.com/doitsujin/dxvk/releases/download/v3.1.1/dxvk-3.1.1.tar.gz".into(),
            sha256: "40565b4a724aadc4433fa4e010b4b23916d9b1f1baeee64e17186db94f54e608".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCopy {
    /// Relative to the extracted release directory (`x64\d3d9.dll`).
    pub from: PathBuf,
    /// Absolute target next to the game.
    pub to: PathBuf,
}

/// DXVK's `d3d8.dll` is built on its `d3d9.dll`, so D3D8 always brings D3D9 along.
pub fn dxvk_plan(game_exe: &Path, bitness: Bitness, which: &[DxvkDll]) -> Vec<FileCopy> {
    let Some(dir) = game_exe.parent() else { return Vec::new() };
    let mut dlls: Vec<DxvkDll> = which.to_vec();
    if dlls.contains(&DxvkDll::D3d8) {
        dlls.push(DxvkDll::D3d9);
    }
    dlls.sort();
    dlls.dedup();
    dlls.into_iter()
        .map(|d| FileCopy {
            from: Path::new(bitness.release_dir()).join(d.file_name()),
            to: dir.join(d.file_name()),
        })
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), ShimError> {
    let want = expected_hex.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    if want.len() != 64 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ShimError::InvalidValue("expected sha256 is not 64 hex digits".into()));
    }
    let got = sha256_hex(bytes);
    if got == want { Ok(()) } else { Err(ShimError::DxvkHashMismatch(format!("got {got}"))) }
}

/// Picks `dxvk-<version>/{x32,x64}/{d3d8,d3d9}.dll` from the release tarball. Every other
/// entry is ignored (no path from the archive is ever used as a file-system path).
pub fn extract_release(tar_gz: &[u8], version: &str) -> Result<Vec<(PathBuf, Vec<u8>)>, ShimError> {
    let wanted: Vec<(String, PathBuf)> = [Bitness::X86, Bitness::X64]
        .iter()
        .flat_map(|b| {
            [DxvkDll::D3d8, DxvkDll::D3d9].map(|d| {
                (
                    format!("dxvk-{version}/{}/{}", b.release_dir(), d.file_name()),
                    Path::new(b.release_dir()).join(d.file_name()),
                )
            })
        })
        .collect();
    let mut out: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    for entry in ar.entries().map_err(|e| ShimError::Decode(format!("dxvk tar: {e}")))? {
        let mut entry = entry.map_err(|e| ShimError::Decode(format!("dxvk tar: {e}")))?;
        let name = entry.path().map_err(|e| ShimError::Decode(e.to_string()))?.to_string_lossy().replace('\\', "/");
        let name = name.trim_start_matches("./").to_string();
        if let Some((_, rel)) = wanted.iter().find(|(w, _)| *w == name) {
            if entry.size() > 64 * 1024 * 1024 {
                return Err(ShimError::Decode(format!("{name} too large")));
            }
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            out.push((rel.clone(), buf));
        }
    }
    for (w, rel) in &wanted {
        if !out.iter().any(|(r, _)| r == rel) {
            return Err(ShimError::Decode(format!("dxvk release lacks {w}")));
        }
    }
    Ok(out)
}

/// `release.json` written next to the extracted files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub source_sha256: String,
    /// (relative path with `\`, sha256)
    pub files: Vec<(String, String)>,
}

pub const RELEASE_MANIFEST: &str = "release.json";
pub const RELEASE_LICENSE: &str = "LICENSE.txt";
/// Marker next to a game: what we copied and what we backed up.
pub const GAME_MARKER: &str = "pulsarsplit-dxvk.json";
pub const BACKUP_SUFFIX: &str = ".pulsarsplit-orig";

/// File access for DXVK (real impl: [`StdFs`]; fake in [`crate::fake`]).
pub trait DxvkFs: Send + Sync {
    fn read(&self, p: &Path) -> Result<Vec<u8>, ShimError>;
    fn read_head(&self, p: &Path, max: usize) -> Result<Vec<u8>, ShimError> {
        let mut v = self.read(p)?;
        v.truncate(max);
        Ok(v)
    }
    /// Atomic: write a temporary sibling, then rename over the target.
    fn write(&self, p: &Path, bytes: &[u8]) -> Result<(), ShimError>;
    fn exists(&self, p: &Path) -> bool;
    fn rename(&self, from: &Path, to: &Path) -> Result<(), ShimError>;
    fn remove(&self, p: &Path) -> Result<(), ShimError>;
    fn create_dir_all(&self, p: &Path) -> Result<(), ShimError>;
}

/// HTTPS GET (real impl: `win::WinHttpFetcher`). Downloads are made by the service only.
pub trait Fetcher: Send + Sync {
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, ShimError>;
}

pub struct StdFs;

impl DxvkFs for StdFs {
    fn read(&self, p: &Path) -> Result<Vec<u8>, ShimError> {
        Ok(std::fs::read(p)?)
    }
    fn read_head(&self, p: &Path, max: usize) -> Result<Vec<u8>, ShimError> {
        let mut f = std::fs::File::open(p)?;
        let mut v = vec![0u8; max];
        let mut n = 0;
        while n < max {
            let k = f.read(&mut v[n..])?;
            if k == 0 {
                break;
            }
            n += k;
        }
        v.truncate(n);
        Ok(v)
    }
    fn write(&self, p: &Path, bytes: &[u8]) -> Result<(), ShimError> {
        use std::io::Write;
        let tmp = p.with_extension("pulsarsplit-tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, p)?;
        Ok(())
    }
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), ShimError> {
        Ok(std::fs::rename(from, to)?)
    }
    fn remove(&self, p: &Path) -> Result<(), ShimError> {
        Ok(std::fs::remove_file(p)?)
    }
    fn create_dir_all(&self, p: &Path) -> Result<(), ShimError> {
        Ok(std::fs::create_dir_all(p)?)
    }
}

/// `<root>\<version>`; root = `%ProgramData%\PulsarSplit\dxvk`.
pub fn release_dir(root: &Path, version: &str) -> PathBuf {
    root.join(version)
}

fn valid_version(v: &str) -> bool {
    !v.is_empty() && v.len() <= 32 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Checks every file of an extracted release against its `release.json`.
pub fn verify_release_dir(fs: &dyn DxvkFs, dir: &Path) -> Result<ReleaseManifest, ShimError> {
    let mpath = dir.join(RELEASE_MANIFEST);
    if !fs.exists(&mpath) {
        return Err(ShimError::DxvkNotDownloaded(dir.display().to_string()));
    }
    let m: ReleaseManifest =
        serde_json::from_slice(&fs.read(&mpath)?).map_err(|e| ShimError::Decode(format!("release.json: {e}")))?;
    for (rel, sha) in &m.files {
        let p = dir.join(rel);
        if !fs.exists(&p) {
            return Err(ShimError::DxvkNotDownloaded(rel.clone()));
        }
        verify_sha256(&fs.read(&p)?, sha).map_err(|_| ShimError::DxvkHashMismatch(rel.clone()))?;
    }
    Ok(m)
}

/// Trusted record of the release that deploys use (review fix 2026-11-12). Written by the
/// elevated download into HKLM (`Shim\Dxvk`, writable by SYSTEM / Administrators only), so
/// a `release.json` planted or edited in the data folder is never trusted on its own: the
/// manifest's bytes must hash to [`ReleasePin::manifest_sha256`] and name the pinned
/// version and source archive hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePin {
    pub version: String,
    /// SHA-256 of the downloaded archive (the installer manifest's / code constant's value).
    pub source_sha256: String,
    /// SHA-256 of `release.json` as written at download.
    pub manifest_sha256: String,
}

/// `HKLM\SOFTWARE\PulsarSplit\Shim\Dxvk`
pub fn pin_key() -> String {
    format!(r"{}\Dxvk", crate::REG_SHIM)
}

/// Pin of a release directory that was just downloaded and verified.
pub fn pin_release(fs: &dyn DxvkFs, dir: &Path) -> Result<ReleasePin, ShimError> {
    let m = verify_release_dir(fs, dir)?;
    let bytes = fs.read(&dir.join(RELEASE_MANIFEST))?;
    Ok(ReleasePin { version: m.version, source_sha256: m.source_sha256.to_ascii_lowercase(), manifest_sha256: sha256_hex(&bytes) })
}

pub fn store_pin(sys: &dyn crate::install::ShimSystem, pin: &ReleasePin) -> Result<(), ShimError> {
    use crate::regval::RegValue;
    let k = pin_key();
    sys.reg_write(&k, "Version", &RegValue::Sz(pin.version.clone()))?;
    sys.reg_write(&k, "SourceSha256", &RegValue::Sz(pin.source_sha256.clone()))?;
    sys.reg_write(&k, "ManifestSha256", &RegValue::Sz(pin.manifest_sha256.clone()))
}

/// `None` when no release was downloaded through us yet.
pub fn load_pin(sys: &dyn crate::install::ShimSystem) -> Result<Option<ReleasePin>, ShimError> {
    let k = pin_key();
    let get = |n: &str| -> Result<Option<String>, ShimError> {
        Ok(sys.reg_read(&k, n)?.and_then(|v| v.as_str().map(str::to_string)))
    };
    match (get("Version")?, get("SourceSha256")?, get("ManifestSha256")?) {
        (Some(version), Some(source_sha256), Some(manifest_sha256)) if valid_version(&version) => {
            Ok(Some(ReleasePin { version, source_sha256, manifest_sha256 }))
        }
        _ => Ok(None),
    }
}

/// Verifies `dir` against the pin: manifest bytes, version, source hash, then every file.
pub fn verify_pinned(fs: &dyn DxvkFs, dir: &Path, pin: &ReleasePin) -> Result<ReleaseManifest, ShimError> {
    let mpath = dir.join(RELEASE_MANIFEST);
    if !fs.exists(&mpath) {
        return Err(ShimError::DxvkNotDownloaded(dir.display().to_string()));
    }
    verify_sha256(&fs.read(&mpath)?, &pin.manifest_sha256).map_err(|_| ShimError::DxvkHashMismatch(RELEASE_MANIFEST.into()))?;
    let m = verify_release_dir(fs, dir)?;
    let src = pin.source_sha256.trim_start_matches("sha256:");
    if m.version != pin.version || !m.source_sha256.eq_ignore_ascii_case(src) {
        return Err(ShimError::DxvkHashMismatch(RELEASE_MANIFEST.into()));
    }
    Ok(m)
}

/// Downloads, verifies and extracts a release once; later calls only re-verify.
pub fn download_release(
    fetcher: &dyn Fetcher,
    fs: &dyn DxvkFs,
    rel: &DxvkRelease,
    root: &Path,
) -> Result<PathBuf, ShimError> {
    if !valid_version(&rel.version) {
        return Err(ShimError::InvalidValue("dxvk version".into()));
    }
    if !rel.url.starts_with("https://") {
        return Err(ShimError::InvalidValue("dxvk url must be https".into()));
    }
    let dir = release_dir(root, &rel.version);
    if let Ok(m) = verify_release_dir(fs, &dir) {
        if m.source_sha256.eq_ignore_ascii_case(rel.sha256.trim_start_matches("sha256:")) {
            return Ok(dir);
        }
    }
    let tgz = fetcher.get(&rel.url, 128 * 1024 * 1024)?;
    verify_sha256(&tgz, &rel.sha256)?;
    let files = extract_release(&tgz, &rel.version)?;
    let mut manifest = ReleaseManifest { version: rel.version.clone(), source_sha256: sha256_hex(&tgz), files: vec![] };
    for (relp, bytes) in &files {
        let p = dir.join(relp);
        if let Some(parent) = p.parent() {
            fs.create_dir_all(parent)?;
        }
        fs.write(&p, bytes)?;
        manifest.files.push((relp.to_string_lossy().replace('/', "\\"), sha256_hex(bytes)));
    }
    fs.write(&dir.join(RELEASE_LICENSE), DXVK_NOTICE.as_bytes())?;
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| ShimError::Decode(e.to_string()))?;
    fs.write(&dir.join(RELEASE_MANIFEST), &json)?;
    Ok(dir)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerFile {
    pub name: String,
    pub sha256: String,
    /// File name of the saved original, if there was one.
    pub backup: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameMarker {
    pub version: String,
    pub files: Vec<MarkerFile>,
}

pub trait DxvkDeployer: Send + Sync {
    /// Journals every change and backs up existing files.
    fn deploy(&self, game_exe: &Path, release_dir: &Path, which: &[DxvkDll]) -> Result<(), ShimError>;
    fn undo(&self, game_exe: &Path) -> Result<(), ShimError>;
}

/// Refuses system folders: DXVK is only placed next to a game.
pub fn check_game_path(game_exe: &Path) -> Result<(), ShimError> {
    let l = game_exe.to_string_lossy().to_ascii_lowercase().replace('/', "\\");
    let sys = ["\\windows\\system32", "\\windows\\syswow64", "\\windows\\winsxs"];
    if !l.ends_with(".exe") || sys.iter().any(|s| l.contains(s)) || game_exe.parent().is_none() {
        return Err(ShimError::InvalidValue("dxvk: not a game executable outside the Windows folder".into()));
    }
    Ok(())
}

pub struct FsDxvkDeployer<'a> {
    pub fs: &'a dyn DxvkFs,
    /// The trusted release record ([`load_pin`]); the release directory must match it.
    pub pin: &'a ReleasePin,
    pub log: &'a dyn RestoreLog,
    pub clock: &'a (dyn Fn() -> String + Sync),
}

impl FsDxvkDeployer<'_> {
    fn journal(&self) -> Result<Journal<'_>, ShimError> {
        let c: &dyn Fn() -> String = self.clock;
        Journal::new(self.log, "service", c)
    }

    fn marker_path(game_exe: &Path) -> PathBuf {
        game_exe.parent().unwrap_or(Path::new(".")).join(GAME_MARKER)
    }

    fn free_backup_name(&self, target: &Path) -> PathBuf {
        let base = format!("{}{BACKUP_SUFFIX}", target.display());
        let mut p = PathBuf::from(&base);
        let mut n = 1;
        while self.fs.exists(&p) {
            p = PathBuf::from(format!("{base}.{n}"));
            n += 1;
        }
        p
    }
}

impl DxvkDeployer for FsDxvkDeployer<'_> {
    fn deploy(&self, game_exe: &Path, release_dir: &Path, which: &[DxvkDll]) -> Result<(), ShimError> {
        check_game_path(game_exe)?;
        if which.is_empty() {
            return Err(ShimError::InvalidValue("dxvk: no dll chosen".into()));
        }
        let head = self.fs.read_head(game_exe, 4096)?;
        let bitness = crate::launch::pe_machine(&head)
            .and_then(crate::launch::bitness_of_machine)
            .ok_or_else(|| ShimError::Unsupported("dxvk: game is neither x86 nor x64".into()))?;
        let manifest = verify_pinned(self.fs, release_dir, self.pin)?;
        if self.fs.exists(&Self::marker_path(game_exe)) {
            self.undo(game_exe)?;
        }
        let j = self.journal()?;
        let mut marker = GameMarker { version: manifest.version.clone(), files: vec![] };
        for c in dxvk_plan(game_exe, bitness, which) {
            let rel = c.from.to_string_lossy().replace('/', "\\");
            let sha = manifest
                .files
                .iter()
                .find(|(r, _)| r.eq_ignore_ascii_case(&rel))
                .map(|(_, s)| s.clone())
                .ok_or_else(|| ShimError::DxvkNotDownloaded(rel.clone()))?;
            let bytes = self.fs.read(&release_dir.join(&c.from))?;
            verify_sha256(&bytes, &sha).map_err(|_| ShimError::DxvkHashMismatch(rel.clone()))?;
            let name = c.to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let target = c.to.display().to_string();
            let backup_path = self.fs.exists(&c.to).then(|| self.free_backup_name(&c.to));
            let previous = backup_path.as_ref().map_or_else(absent, |b| backup(&b.display().to_string()));
            j.record(RecordKind::File, &target, previous, file_sha256(&sha), || {
                if let Some(b) = &backup_path {
                    self.fs.rename(&c.to, b)?;
                }
                self.fs.write(&c.to, &bytes)
            })?;
            marker.files.push(MarkerFile {
                name,
                sha256: sha,
                backup: backup_path.and_then(|b| b.file_name().map(|n| n.to_string_lossy().into_owned())),
            });
        }
        let json = serde_json::to_vec_pretty(&marker).map_err(|e| ShimError::Decode(e.to_string()))?;
        self.fs.write(&Self::marker_path(game_exe), &json)
    }

    fn undo(&self, game_exe: &Path) -> Result<(), ShimError> {
        let mpath = Self::marker_path(game_exe);
        if !self.fs.exists(&mpath) {
            return Ok(());
        }
        let marker: GameMarker = serde_json::from_slice(&self.fs.read(&mpath)?)
            .map_err(|e| ShimError::Decode(format!("{GAME_MARKER}: {e}")))?;
        let dir = mpath.parent().unwrap_or(Path::new(".")).to_path_buf();
        let j = self.journal()?;
        for f in &marker.files {
            if f.name.contains(['\\', '/']) || f.backup.as_deref().is_some_and(|b| b.contains(['\\', '/'])) {
                return Err(ShimError::Decode(format!("{GAME_MARKER}: bad file name")));
            }
            let target = dir.join(&f.name);
            if self.fs.exists(&target) {
                let ours = self.fs.read(&target).map(|b| sha256_hex(&b) == f.sha256).unwrap_or(false);
                if !ours {
                    log::warn!("dxvk undo: {} was replaced since deploy; left in place", f.name);
                    continue;
                }
                j.record(RecordKind::File, &target.display().to_string(), file_sha256(&f.sha256), absent(), || {
                    self.fs.remove(&target)
                })?;
            }
            if let Some(b) = &f.backup {
                let bp = dir.join(b);
                if self.fs.exists(&bp) && !self.fs.exists(&target) {
                    j.record(RecordKind::File, &target.display().to_string(), absent(), backup(&bp.display().to_string()), || {
                        self.fs.rename(&bp, &target)
                    })?;
                }
            }
        }
        self.fs.remove(&mpath)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeFetcher, MemFs, MemoryLog};

    fn tarball(version: &str, skip: Option<&str>) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for arch in ["x32", "x64"] {
            for dll in ["d3d8.dll", "d3d9.dll", "dxgi.dll"] {
                let name = format!("dxvk-{version}/{arch}/{dll}");
                if Some(name.as_str()) == skip {
                    continue;
                }
                let data = format!("{arch}-{dll}").into_bytes();
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, &name, data.as_slice()).unwrap();
            }
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    fn release(tgz: &[u8]) -> DxvkRelease {
        DxvkRelease { version: "3.1.1".into(), url: "https://example.invalid/dxvk.tar.gz".into(), sha256: sha256_hex(tgz) }
    }

    #[test]
    fn plan_adds_d3d9_for_d3d8() {
        let p = dxvk_plan(Path::new(r"C:\G\game.exe"), Bitness::X86, &[DxvkDll::D3d8]);
        assert_eq!(
            p,
            vec![
                FileCopy { from: PathBuf::from(r"x32\d3d8.dll"), to: PathBuf::from(r"C:\G\d3d8.dll") },
                FileCopy { from: PathBuf::from(r"x32\d3d9.dll"), to: PathBuf::from(r"C:\G\d3d9.dll") },
            ]
        );
        let p = dxvk_plan(Path::new(r"C:\G\game.exe"), Bitness::X64, &[DxvkDll::D3d9, DxvkDll::D3d9]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].from, PathBuf::from(r"x64\d3d9.dll"));
    }

    #[test]
    fn sha256() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(verify_sha256(b"abc", "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD").is_ok());
        assert!(verify_sha256(b"abc", "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").is_ok());
        assert_eq!(
            verify_sha256(b"abd", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").unwrap_err().code(),
            "dxvk_hash_mismatch"
        );
        assert!(verify_sha256(b"abc", "xyz").is_err());
    }

    #[test]
    fn extract_picks_only_wanted() {
        let files = extract_release(&tarball("3.1.1", None), "3.1.1").unwrap();
        assert_eq!(files.len(), 4);
        assert!(files.iter().any(|(p, b)| p == Path::new(r"x64\d3d8.dll") && b == b"x64-d3d8.dll"));
        assert!(extract_release(&tarball("3.1.1", Some("dxvk-3.1.1/x32/d3d9.dll")), "3.1.1").is_err());
        assert!(extract_release(b"not gzip", "3.1.1").is_err());
    }

    #[test]
    fn download_verifies_and_caches() {
        let tgz = tarball("3.1.1", None);
        let fs = MemFs::default();
        let f = FakeFetcher::new(tgz.clone());
        let root = Path::new(r"C:\ProgramData\PulsarSplit\dxvk");
        let dir = download_release(&f, &fs, &release(&tgz), root).unwrap();
        assert_eq!(dir, root.join("3.1.1"));
        assert!(fs.exists(&dir.join(RELEASE_LICENSE)));
        assert_eq!(verify_release_dir(&fs, &dir).unwrap().files.len(), 4);
        // Second call: no download.
        download_release(&f, &fs, &release(&tgz), root).unwrap();
        assert_eq!(f.calls(), 1);
        // Tampered file is detected.
        fs.put(&dir.join(r"x64\d3d9.dll"), b"evil");
        assert_eq!(verify_release_dir(&fs, &dir).unwrap_err().code(), "dxvk_hash_mismatch");
        // Wrong archive hash.
        let mut bad = release(&tgz);
        bad.sha256 = sha256_hex(b"other");
        let fs2 = MemFs::default();
        assert_eq!(download_release(&f, &fs2, &bad, root).unwrap_err().code(), "dxvk_hash_mismatch");
        let mut http = release(&tgz);
        http.url = "http://x".into();
        assert!(download_release(&f, &fs2, &http, root).is_err());
        assert_eq!(verify_release_dir(&fs2, &dir).unwrap_err().code(), "dxvk_not_downloaded");
    }

    fn setup() -> (MemFs, PathBuf, ReleasePin) {
        let tgz = tarball("3.1.1", None);
        let fs = MemFs::default();
        let dir = download_release(&FakeFetcher::new(tgz.clone()), &fs, &release(&tgz), Path::new(r"C:\PD\dxvk")).unwrap();
        fs.put(Path::new(r"C:\Games\Old\game.exe"), &crate::launch::tests_support::fake_pe(0x14c));
        let pin = pin_release(&fs, &dir).unwrap();
        assert_eq!(pin.source_sha256, sha256_hex(&tgz));
        (fs, dir, pin)
    }

    #[test]
    fn pinned_release_refuses_planted_or_edited_manifest() {
        let (fs, dir, pin) = setup();
        verify_pinned(&fs, &dir, &pin).unwrap();
        // A planted release whose own manifest matches its own files is still refused.
        let planted = Path::new(r"C:\PD\dxvk\zzz");
        fs.put(&planted.join(r"x32\d3d9.dll"), b"evil");
        let m = ReleaseManifest { version: "3.1.1".into(), source_sha256: pin.source_sha256.clone(), files: vec![(r"x32\d3d9.dll".into(), sha256_hex(b"evil"))] };
        fs.put(&planted.join(RELEASE_MANIFEST), &serde_json::to_vec(&m).unwrap());
        verify_release_dir(&fs, planted).unwrap();
        assert_eq!(verify_pinned(&fs, planted, &pin).unwrap_err().code(), "dxvk_hash_mismatch");
        // Editing the pinned release's manifest (with matching file hashes) is refused too.
        fs.put(&dir.join(r"x32\d3d9.dll"), b"evil");
        let mut m2 = verify_release_dir(&fs, &dir).unwrap_err();
        assert_eq!(m2.code(), "dxvk_hash_mismatch");
        let m = ReleaseManifest { version: "3.1.1".into(), source_sha256: pin.source_sha256.clone(), files: vec![(r"x32\d3d9.dll".into(), sha256_hex(b"evil"))] };
        fs.put(&dir.join(RELEASE_MANIFEST), &serde_json::to_vec(&m).unwrap());
        m2 = verify_pinned(&fs, &dir, &pin).unwrap_err();
        assert_eq!(m2.code(), "dxvk_hash_mismatch");
    }

    #[test]
    fn pin_round_trips_through_the_registry() {
        let (fs, dir, pin) = setup();
        let sys = crate::fake::FakeShimSystem::default();
        assert_eq!(load_pin(&sys).unwrap(), None);
        store_pin(&sys, &pin).unwrap();
        assert_eq!(load_pin(&sys).unwrap(), Some(pin.clone()));
        let _ = (fs, dir);
    }

    #[test]
    fn deploy_backup_and_undo() {
        let (fs, dir, pin) = setup();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let d = FsDxvkDeployer { fs: &fs, pin: &pin, log: &log, clock: &clock };
        let game = Path::new(r"C:\Games\Old\game.exe");
        fs.put(Path::new(r"C:\Games\Old\d3d9.dll"), b"game's own d3d9");
        d.deploy(game, &dir, &[DxvkDll::D3d8]).unwrap();
        assert_eq!(fs.get(Path::new(r"C:\Games\Old\d3d8.dll")).unwrap(), b"x32-d3d8.dll");
        assert_eq!(fs.get(Path::new(r"C:\Games\Old\d3d9.dll")).unwrap(), b"x32-d3d9.dll");
        assert_eq!(fs.get(Path::new(r"C:\Games\Old\d3d9.dll.pulsarsplit-orig")).unwrap(), b"game's own d3d9");
        assert!(fs.exists(Path::new(r"C:\Games\Old\pulsarsplit-dxvk.json")));
        assert!(log.read_all().unwrap().len() >= 4);
        // Redeploy is idempotent (undo first, same backup restored then re-made).
        d.deploy(game, &dir, &[DxvkDll::D3d9]).unwrap();
        assert!(!fs.exists(Path::new(r"C:\Games\Old\d3d8.dll")));
        d.undo(game).unwrap();
        assert_eq!(fs.get(Path::new(r"C:\Games\Old\d3d9.dll")).unwrap(), b"game's own d3d9");
        assert!(!fs.exists(Path::new(r"C:\Games\Old\d3d9.dll.pulsarsplit-orig")));
        assert!(!fs.exists(Path::new(r"C:\Games\Old\pulsarsplit-dxvk.json")));
        d.undo(game).unwrap(); // no-op
    }

    #[test]
    fn undo_keeps_replaced_files() {
        let (fs, dir, pin) = setup();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let d = FsDxvkDeployer { fs: &fs, pin: &pin, log: &log, clock: &clock };
        let game = Path::new(r"C:\Games\Old\game.exe");
        d.deploy(game, &dir, &[DxvkDll::D3d9]).unwrap();
        fs.put(Path::new(r"C:\Games\Old\d3d9.dll"), b"user update");
        d.undo(game).unwrap();
        assert_eq!(fs.get(Path::new(r"C:\Games\Old\d3d9.dll")).unwrap(), b"user update");
    }

    #[test]
    fn deploy_refusals() {
        let (fs, dir, pin) = setup();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let d = FsDxvkDeployer { fs: &fs, pin: &pin, log: &log, clock: &clock };
        assert!(d.deploy(Path::new(r"C:\Windows\System32\notepad.exe"), &dir, &[DxvkDll::D3d9]).is_err());
        assert!(d.deploy(Path::new(r"C:\Games\Old\game.exe"), &dir, &[]).is_err());
        fs.put(Path::new(r"C:\Games\Arm\a.exe"), &crate::launch::tests_support::fake_pe(0xAA64));
        assert_eq!(d.deploy(Path::new(r"C:\Games\Arm\a.exe"), &dir, &[DxvkDll::D3d9]).unwrap_err().code(), "shim_unsupported");
        assert_eq!(
            d.deploy(Path::new(r"C:\Games\Old\game.exe"), Path::new(r"C:\nowhere"), &[DxvkDll::D3d9]).unwrap_err().code(),
            "dxvk_not_downloaded"
        );
    }

    #[test]
    fn notice_is_zlib() {
        assert!(DXVK_NOTICE.contains("zlib/libpng license"));
        assert!(DXVK_NOTICE.contains("Philip Rebohle"));
    }
}
