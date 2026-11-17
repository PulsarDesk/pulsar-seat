// SPDX-License-Identifier: GPL-3.0-only
//
// Public-repository readiness scan (docs/release/public-repo.md): everything git would
// publish (tracked files plus untracked files that are not ignored) is checked for things
// that must never reach a public repository, and the files a GPL-3 project must carry are
// required.
//
//   bun installer/scripts/repo-scan.ts [--root <repo>] [--json]
//
// Errors (exit 1):
//   local_path      a home-folder path of a real user (C:\Users\<name>\, /home/<name>/,
//                   /Users/<name>/); placeholders such as <user>, %USERPROFILE%, $env:... pass
//   private_key     a PEM private key block
//   minisign_secret a minisign secret key line (RWQAAEIy / RWRTY0Iy + long base64 body)
//   token           GitHub / AWS / Slack / generic bearer token shapes
//   key_file        a key or credential file by name (.pfx .p12 .pvk .snk .key .jks
//                   .keystore, vm-creds.xml, .env)
//   binary          a built or third-party binary (.exe .dll .sys .msi .cab .iso .zip
//                   .7z .pdb .lib .obj) or any file over 5 MiB
//   missing_file    a required file is missing (README.md, LICENSE, CONTRIBUTING.md,
//                   SECURITY.md, CLEANROOM.md, installer/licenses/SOURCE-OFFER.txt,
//                   installer/licenses/NOTICES.md)
//   missing_spdx    one of our source files (.rs .ts .ps1 .c .cpp .h .svelte) without an
//                   `SPDX-License-Identifier` in its first lines (vendored folders excepted)
//   local_doc       a file under docs/local/ (local-only facts, gitignored)
// Warnings:
//   private_ip      an RFC 1918 192.168.x.y / 10.x.y.z / 172.16-31.x.y address in a doc
//                   or config file (code and tests use sample addresses)
//   email           an e-mail address other than the known public ones
//
// Intentional hits go into installer/repo-scan-allow.txt (`<path>  <rule>  # why`).

import { existsSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

export type Severity = "error" | "warning";
export interface Finding {
  path: string;
  line: number;
  rule: string;
  severity: Severity;
  detail: string;
}

export const REQUIRED_FILES = [
  "README.md",
  "LICENSE",
  "CONTRIBUTING.md",
  "SECURITY.md",
  "CLEANROOM.md",
  "installer/licenses/SOURCE-OFFER.txt",
  "installer/licenses/NOTICES.md",
];

const BINARY_EXT = /\.(exe|dll|sys|msi|cab|iso|zip|7z|pdb|lib|obj|pfx|p12)$/i;
const KEY_FILE = /(^|\/)([^/]+\.(pfx|p12|pvk|snk|key|jks|keystore)|vm-creds\.xml|\.env(\.[^/]*)?)$/i;
const SPDX_EXT = /\.(rs|ts|ps1|c|cpp|h|svelte)$/i;
/** Folders with third-party sources that keep their own headers. */
export const VENDORED = [/(^|\/)vendor(ed)?\//i, /(^|\/)third[-_]party\//i, /(^|\/)node_modules\//, /(^|\/)detours\//i];
const MAX_SIZE = 5 * 1024 * 1024;

/**
 * Placeholder user names in documentation and test paths: one letter (`C:\Users\x`), the
 * product's own seat / kiosk accounts, `<user>`, `%USERNAME%`, `$env:...` and friends.
 */
const PLACEHOLDER_USER = /^([A-Za-z]|PulsarSeat\w*|PulsarKiosk\w*|<[^>]*>|%[^%]*%|\$[\w:{}()]+|\{[^}]*\}|public|default|default user|all users|user|username|you|name|me|runneradmin|\.\.\.|\*)$/i;
const PUBLIC_EMAIL = [
  /^noreply@anthropic\.com$/i,
  /@example\.(com|org|net)$/i,
  /^fsfcomm@microsoft\.com$/i,
  /@users\.noreply\.github\.com$/i,
  /^git@github\.com$/i,
];

/** Prose and config files (where local facts tend to leak); code and tests use sample addresses. */
const DOC_FILE = /\.(md|txt|json|toml|ya?ml|ps1|xml|ini|cfg|conf)$/i;

const LINE_RULES: {
  rule: string;
  severity: Severity;
  re: RegExp;
  detail: string;
  check?: (m: RegExpExecArray) => boolean;
  docsOnly?: boolean;
}[] = [
  {
    rule: "local_path",
    severity: "error",
    re: /\b[A-Za-z]:[\\/]{1,2}Users[\\/]{1,2}([^\\/\s"'`<>|*?]+|<[^>]+>)/g,
    detail: "home-folder path of a real user (local facts belong in docs/local/)",
    check: (m) => !PLACEHOLDER_USER.test(m[1]),
  },
  {
    rule: "local_path",
    severity: "error",
    re: /(?<![\w.~:-])\/(?:home|Users)\/([^/\s"'`<>]+)\//g,
    detail: "home-folder path of a real user (local facts belong in docs/local/)",
    check: (m) => !PLACEHOLDER_USER.test(m[1]),
  },
  { rule: "private_key", severity: "error", re: /-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----/g, detail: "PEM private key" },
  { rule: "minisign_secret", severity: "error", re: /RW(?:QAAEIy|RTY0Iy)[A-Za-z0-9+/]{100,}/g, detail: "minisign secret key: rotate it" },
  { rule: "token", severity: "error", re: /\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9-]{10,})\b/g, detail: "access token" },
  {
    rule: "private_ip",
    severity: "warning",
    re: /(?<![\d.])(?:192\.168|10\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01]))\.\d{1,3}\.\d{1,3}(?![\d.])/g,
    detail: "private network address (local facts belong in docs/local/)",
    docsOnly: true,
  },
  {
    rule: "email",
    severity: "warning",
    re: /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g,
    detail: "e-mail address",
    check: (m) => !PUBLIC_EMAIL.some((r) => r.test(m[0])) && !/\.(png|svg|jpg|webp|ico)$/i.test(m[0]),
  },
];

export interface Allow {
  path: string;
  rule: string;
}

/** `<path>  <rule>  # comment` lines; `*` as path matches every file. */
export function parseAllow(text: string): Allow[] {
  const out: Allow[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    const [path, rule] = line.split(/\s+/);
    if (path && rule) out.push({ path, rule });
  }
  return out;
}

function allowed(allow: Allow[], f: Finding): boolean {
  return allow.some((a) => a.rule === f.rule && (a.path === "*" || a.path === f.path || (a.path.endsWith("/") && f.path.startsWith(a.path))));
}

/** Findings of one text file (path relative, forward slashes). */
export function scanText(path: string, text: string): Finding[] {
  const out: Finding[] = [];
  const lines = text.split(/\r?\n/);
  lines.forEach((l, i) => {
    for (const r of LINE_RULES) {
      if (r.docsOnly && !DOC_FILE.test(path)) continue;
      r.re.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = r.re.exec(l))) {
        if (!r.check || r.check(m)) out.push({ path, line: i + 1, rule: r.rule, severity: r.severity, detail: `${r.detail}: ${m[0].slice(0, 60)}` });
      }
    }
  });
  if (SPDX_EXT.test(path) && !VENDORED.some((v) => v.test(path)) && !lines.slice(0, 5).some((l) => l.includes("SPDX-License-Identifier"))) {
    out.push({ path, line: 1, rule: "missing_spdx", severity: "error", detail: "no SPDX-License-Identifier in the first 5 lines" });
  }
  return out;
}

/** Findings from the file name / size alone. */
export function scanName(path: string, size: number): Finding[] {
  const out: Finding[] = [];
  if (KEY_FILE.test(path)) out.push({ path, line: 0, rule: "key_file", severity: "error", detail: "key or credential file" });
  else if (BINARY_EXT.test(path)) out.push({ path, line: 0, rule: "binary", severity: "error", detail: "binary file" });
  if (size > MAX_SIZE) out.push({ path, line: 0, rule: "binary", severity: "error", detail: `${(size / 1048576).toFixed(1)} MiB (> 5 MiB)` });
  if (/^docs\/local\//.test(path)) out.push({ path, line: 0, rule: "local_doc", severity: "error", detail: "docs/local/ is local-only" });
  return out;
}

export function missingFiles(paths: Set<string>): Finding[] {
  return REQUIRED_FILES.filter((f) => !paths.has(f)).map((f) => ({ path: f, line: 0, rule: "missing_file", severity: "error" as const, detail: "required for a public GPL-3 repository" }));
}

function isText(b: Uint8Array): boolean {
  const n = Math.min(b.length, 8000);
  for (let i = 0; i < n; i++) if (b[i] === 0) return false;
  return true;
}

/** Decode a text file: UTF-16LE with BOM (INF files) or UTF-8. */
function decode(b: Uint8Array): string | null {
  if (b.length >= 2 && b[0] === 0xff && b[1] === 0xfe) return new TextDecoder("utf-16le").decode(b.subarray(2));
  return isText(b) ? new TextDecoder().decode(b) : null;
}

export function scanRepo(root: string, files: string[], allow: Allow[]): Finding[] {
  const all: Finding[] = [];
  for (const f of files) {
    const abs = join(root, f);
    if (!existsSync(abs)) continue; // deleted in the working tree
    const size = statSync(abs).size;
    all.push(...scanName(f, size));
    if (BINARY_EXT.test(f) || size > MAX_SIZE) continue;
    const text = decode(readFileSync(abs));
    if (text !== null) all.push(...scanText(f, text));
  }
  all.push(...missingFiles(new Set(files)));
  return all.filter((x) => !allowed(allow, x));
}

/** Files git would publish: tracked + untracked-not-ignored. */
export function publishable(root: string): string[] {
  const r = Bun.spawnSync(["git", "-C", root, "ls-files", "-z", "--cached", "--others", "--exclude-standard"], { stdout: "pipe", stderr: "pipe" });
  if (r.exitCode !== 0) throw new Error(`git ls-files failed: ${r.stderr.toString()}`);
  return [...new Set(r.stdout.toString().split("\0").filter(Boolean))].sort();
}

if (import.meta.main) {
  const argv = process.argv.slice(2);
  const i = argv.indexOf("--root");
  const root = resolve(i >= 0 ? argv[i + 1] : join(import.meta.dir, "..", ".."));
  const allowFile = join(root, "installer", "repo-scan-allow.txt");
  const allow = existsSync(allowFile) ? parseAllow(readFileSync(allowFile, "utf8")) : [];
  const files = publishable(root);
  const findings = scanRepo(root, files, allow);
  if (argv.includes("--json")) console.log(JSON.stringify(findings, null, 2));
  else for (const f of findings) console.log(`${f.severity === "error" ? "ERROR" : "warn "} ${f.rule.padEnd(15)} ${f.path}${f.line ? ":" + f.line : ""}  ${f.detail}`);
  const errors = findings.filter((f) => f.severity === "error").length;
  console.log(`${files.length} files scanned: ${errors} error(s), ${findings.length - errors} warning(s)`);
  process.exit(errors ? 1 : 0);
}
