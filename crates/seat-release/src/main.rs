// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-release`: the maintainer's release-signing tool (never shipped).
//!
//! ```text
//! pulsar-seat-release keygen [--out-dir <dir>] [--allow-unprotected]
//! pulsar-seat-release sign --file <SHA256SUMS.txt> [--key <minisign.key>] [--signers <release-signers.txt>] [--version <X.Y.Z>]
//! pulsar-seat-release verify --file <file> --signers <release-signers.txt> [--sig <file.minisig>]
//! pulsar-seat-release check-signers <release-signers.txt> [--require]
//! ```
//!
//! The secret key is written only outside any source tree (default
//! `%USERPROFILE%\.pulsar-seat\release-key\`), is never printed, and is created with a
//! protected DACL (current user + SYSTEM; `keyfile`). A volume without ACLs (FAT / exFAT)
//! is refused unless `--allow-unprotected` is given. Wrapped by
//! `installer/scripts/release-keygen.ps1` and `installer/scripts/sign-release.ps1`.

mod keyfile;

use seat_release::{Error, SecretKey, parse_signers, sign, verify};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const KEY_FILE: &str = "minisign.key";
const PUB_FILE: &str = "minisign.pub";

fn default_key_dir() -> Result<PathBuf, Error> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| Error::Other("USERPROFILE is not set; pass --out-dir".into()))?;
    Ok(PathBuf::from(home).join(".pulsar-seat").join("release-key"))
}

/// Refuse a key directory inside a source tree (any ancestor with `.git`, `Cargo.toml`,
/// `package.json` or `AGENTS.md`): the secret key must never land in a repository.
fn refuse_in_repo(dir: &Path) -> Result<(), Error> {
    let abs = if dir.is_absolute() { dir.to_path_buf() } else { std::env::current_dir().map_err(io)?.join(dir) };
    for a in abs.ancestors() {
        for marker in [".git", "Cargo.toml", "package.json", "AGENTS.md"] {
            if a.join(marker).exists() {
                return Err(Error::Other(format!(
                    "refusing to write a secret key inside a source tree ({} has {marker}); use a folder outside every repository",
                    a.display()
                )));
            }
        }
    }
    Ok(())
}

fn io(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn read(p: &str) -> Result<String, Error> {
    std::fs::read_to_string(p).map_err(|e| Error::Other(format!("{p}: {e}")))
}

fn keygen(args: &[String]) -> Result<(), Error> {
    let dir = match arg(args, "--out-dir") {
        Some(d) => PathBuf::from(d),
        None => default_key_dir()?,
    };
    refuse_in_repo(&dir)?;
    std::fs::create_dir_all(&dir).map_err(io)?;
    let key_path = dir.join(KEY_FILE);
    let pub_path = dir.join(PUB_FILE);
    if key_path.exists() || pub_path.exists() {
        return Err(Error::Other(format!("{} already holds a key; never overwrite a release key", dir.display())));
    }
    let allow_unprotected = args.iter().any(|a| a == "--allow-unprotected");
    let sk = SecretKey::generate()?;
    // Protected DACL from the start (no inherited entry of the folder ever applies).
    let (mut f, prot) = keyfile::create_secret_file(&key_path, allow_unprotected).map_err(Error::Other)?;
    let written = f.write_all(sk.to_file_text().as_bytes()).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&key_path);
        return Err(io(e));
    }
    let pk = sk.public();
    std::fs::write(&pub_path, pk.to_file_text()).map_err(io)?;
    match prot {
        keyfile::Protection::Restricted { .. } => println!(
            "secret key: {} (not printed; readable by your account and SYSTEM only; keep a backup offline)",
            key_path.display()
        ),
        keyfile::Protection::Unprotected => eprintln!(
            "WARNING: secret key {} is on a volume without ACLs (--allow-unprotected): every local user can read it",
            key_path.display()
        ),
    }
    println!("public key: {}", pub_path.display());
    println!("key id:     {}", pk.key_id_hex());
    println!("Add this line to installer/release-signers.txt:");
    println!("{}", pk.to_base64());
    Ok(())
}

fn sign_cmd(args: &[String]) -> Result<(), Error> {
    let file = arg(args, "--file").ok_or_else(|| Error::Other("--file <SHA256SUMS.txt> is required".into()))?;
    let key = match arg(args, "--key") {
        Some(k) => PathBuf::from(k),
        None => default_key_dir()?.join(KEY_FILE),
    };
    let text = zeroize::Zeroizing::new(
        std::fs::read_to_string(&key).map_err(|e| Error::Other(format!("{}: {e}", key.display())))?,
    );
    let sk = SecretKey::from_file_text(&text)?;
    drop(text);
    let data = std::fs::read(&file).map_err(|e| Error::Other(format!("{file}: {e}")))?;
    let name = Path::new(&file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // The release version is part of the signed trusted comment: the service refuses a
    // checksum file whose `version:` differs from the release tag it was offered with.
    let sums_version = seat_release::sums_version(&String::from_utf8_lossy(&data)).map_err(Error::Other)?;
    if let Some(v) = arg(args, "--version")
        && v.trim_start_matches('v') != sums_version
    {
        return Err(Error::Other(format!("--version {v}, but the MSIs in {file} are {sums_version}")));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let sig = sign(
        &sk,
        &data,
        &seat_release::release_trusted_comment(now, &name, &sums_version),
        &format!("signature from pulsar-seat release key {}", sk.key_id_hex()),
    )?;
    // Self-check before anything is written: the signature verifies with our own key and,
    // when a signer list is given, with that list (a key the service does not know would
    // make every client refuse the release).
    verify(&[sk.public()], &data, &sig)?;
    if let Some(s) = arg(args, "--signers") {
        let keys = parse_signers(&read(&s)?)?;
        verify(&keys, &data, &sig)
            .map_err(|e| Error::Other(format!("{s} does not accept this key ({e}); add its public key first")))?;
    }
    let out = format!("{file}.minisig");
    std::fs::write(&out, &sig).map_err(io)?;
    println!("wrote {out} (key {})", sk.key_id_hex());
    Ok(())
}

fn verify_cmd(args: &[String]) -> Result<(), Error> {
    let file = arg(args, "--file").ok_or_else(|| Error::Other("--file is required".into()))?;
    let signers = arg(args, "--signers").ok_or_else(|| Error::Other("--signers is required".into()))?;
    let sig = arg(args, "--sig").unwrap_or_else(|| format!("{file}.minisig"));
    let keys = parse_signers(&read(&signers)?)?;
    let data = std::fs::read(&file).map_err(|e| Error::Other(format!("{file}: {e}")))?;
    let v = verify(&keys, &data, &read(&sig)?)?;
    println!("ok: {file} signed by key {} ({})", v.key_id, v.trusted_comment);
    Ok(())
}

fn check_signers(args: &[String]) -> Result<(), Error> {
    let file = args.first().filter(|a| !a.starts_with("--")).ok_or_else(|| Error::Other("signer file path required".into()))?;
    let keys = parse_signers(&read(file)?)?;
    if keys.is_empty() && args.iter().any(|a| a == "--require") {
        return Err(Error::Other(format!(
            "{file} lists no release key: a release build could never accept an update (docs/release/checklist.md)"
        )));
    }
    for k in &keys {
        println!("key {}", k.key_id_hex());
    }
    println!("{} release key(s) in {file}", keys.len());
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("", &[][..]),
    };
    let r = match cmd {
        "keygen" => keygen(rest),
        "sign" => sign_cmd(rest),
        "verify" => verify_cmd(rest),
        "check-signers" => check_signers(rest),
        _ => {
            eprintln!(
                "usage: pulsar-seat-release keygen [--out-dir <dir>] [--allow-unprotected]\n       pulsar-seat-release sign --file <SHA256SUMS.txt> [--key <minisign.key>] [--signers <release-signers.txt>] [--version <X.Y.Z>]\n       pulsar-seat-release verify --file <file> --signers <release-signers.txt> [--sig <file.minisig>]\n       pulsar-seat-release check-signers <release-signers.txt> [--require]"
            );
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_dir_inside_a_repo_refused() {
        let repo = std::env::temp_dir().join(format!("pulsar-seat-release-test-{}", std::process::id()));
        let inner = repo.join("a").join("b");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "").unwrap();
        assert!(refuse_in_repo(&inner).is_err());
        std::fs::remove_file(repo.join("Cargo.toml")).unwrap();
        // Only refuses because of a marker; the temp dir itself has none (normally).
        let _ = std::fs::remove_dir_all(&repo);
    }
}
