// SPDX-License-Identifier: GPL-3.0-only
//! Repository scan for double-encoded text (UTF-8 read as Windows-1252 and saved again as
//! UTF-8). A bulk edit once shipped broken Turkish runtime strings (each dotless i as two
//! Latin-1 letters); this test fails on any such marker in our own sources, docs and
//! installer files.

use std::path::{Path, PathBuf};

const ROOTS: [&str; 7] = ["crates", "apps/manager/src", "apps/manager/src-tauri/src", "docs", "installer", "shim", "spec"];
const EXTS: [&str; 12] = ["rs", "ts", "svelte", "md", "ps1", "psm1", "wxs", "wxl", "json", "toml", "txt", "css"];
const SKIP_DIRS: [&str; 5] = ["target", "node_modules", "third_party", ".svelte-kit", "build"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&p, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| EXTS.contains(&x)) {
            out.push(p);
        }
    }
}

#[test]
fn no_double_encoded_text_in_sources() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for r in ROOTS {
        walk(&repo.join(r), &mut files);
    }
    assert!(files.len() > 100, "scan found too few files ({}), wrong root?", files.len());
    let mut hits = Vec::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for (n, line) in text.lines().enumerate() {
            if let Some(m) = seat_core::i18n::mojibake(line) {
                hits.push(format!("{}:{}: {m:?}", f.strip_prefix(&repo).unwrap_or(f).display(), n + 1));
            }
            // A backslash escape a script expanded by accident (`\a` became BEL in a doc
            // once): control characters other than tab / CR / ESC never belong in text.
            if let Some(c) = line.chars().find(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\u{1b}')) {
                hits.push(format!("{}:{}: control character U+{:04X}", f.strip_prefix(&repo).unwrap_or(f).display(), n + 1, c as u32));
            }
        }
    }
    assert!(hits.is_empty(), "double-encoded text:\n{}", hits.join("\n"));
}
