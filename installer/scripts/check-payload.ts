// SPDX-License-Identifier: GPL-3.0-only
//
// Fails the build when the installer payload contains something we must never ship
// (spec 90 §2.2, §2.5, §7; contracts §8.2):
//   - Microsoft binaries: C/C++ runtime (vcruntime*, msvcp*, ucrtbase, concrt*, vccorlib*,
//     vcomp*, mfc*, api-ms-win-*), WebView2Loader.dll, copies of Windows system files
//     (rdpidd.dll, rfxvmt.dll, termsrv.dll, AcRes.dll), D3D compiler redistributables
//   - certificates / private keys: *.pfx *.p12 *.cer *.crt *.der *.pem *.key *.pvk *.spc
//     *.p7b *.sst (no root / TrustedPublisher certificate, no fixed TLS key pair)
//   - browser web-UI assets (*.html, *.htm, folders named web / webui / www)
//   - known vulnerable or kernel-patching drivers (RTCore64.sys, DuoDriver.sys, ...)
// Kernel drivers (*.sys) of third-party packages produce a warning for review.
//
// Usage: bun installer/scripts/check-payload.ts <dir> [<dir> ...]
// Exit 0 = clean, 1 = violations (printed), 2 = usage error.

import { readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

export type Severity = "error" | "warning";
export interface Violation {
  path: string;
  rule: string;
  severity: Severity;
}

const MS_DLL_PATTERNS: RegExp[] = [
  /^vcruntime\d*(_\d+)?\.dll$/i,
  /^msvcp\d+(_\w+)?\.dll$/i,
  /^ucrtbased?\.dll$/i,
  /^concrt\d+\.dll$/i,
  /^vccorlib\d+\.dll$/i,
  /^vcomp\d+\.dll$/i,
  /^mfc\d+\w*\.dll$/i,
  /^api-ms-win-.*\.dll$/i,
  /^webview2loader\.dll$/i,
  /^rdpidd\.dll$/i,
  /^rfxvmt\.dll$/i,
  /^termsrv\.dll$/i,
  /^acres\.dll$/i,
  /^d3dcompiler_\d+\.dll$/i,
  /^msvcr\d+\.dll$/i,
];

const CERT_EXT = new Set([".pfx", ".p12", ".cer", ".crt", ".der", ".pem", ".key", ".pvk", ".spc", ".p7b", ".sst"]);
const WEB_EXT = new Set([".html", ".htm"]);
const WEB_DIRS = new Set(["web", "webui", "www"]);
const BAD_DRIVERS = new Set(["rtcore64.sys", "duodriver.sys", "winring0.sys", "winring0x64.sys", "gdrv.sys", "dbutil_2_3.sys"]);

function ext(name: string): string {
  const i = name.lastIndexOf(".");
  return i < 0 ? "" : name.slice(i).toLowerCase();
}

/** Pure rule check over relative paths (forward or back slashes). */
export function findViolations(paths: string[]): Violation[] {
  const out: Violation[] = [];
  for (const raw of paths) {
    const path = raw.replace(/\\/g, "/");
    const parts = path.split("/").filter((p) => p.length > 0);
    const name = parts[parts.length - 1] ?? "";
    const lower = name.toLowerCase();
    if (MS_DLL_PATTERNS.some((re) => re.test(name))) {
      out.push({ path, rule: "microsoft_binary", severity: "error" });
    }
    if (CERT_EXT.has(ext(name))) {
      out.push({ path, rule: "certificate_or_key", severity: "error" });
    }
    if (WEB_EXT.has(ext(name)) || parts.slice(0, -1).some((d) => WEB_DIRS.has(d.toLowerCase()))) {
      out.push({ path, rule: "web_ui_asset", severity: "error" });
    }
    if (BAD_DRIVERS.has(lower)) {
      out.push({ path, rule: "vulnerable_driver", severity: "error" });
    } else if (ext(name) === ".sys") {
      out.push({ path, rule: "kernel_driver_review", severity: "warning" });
    }
  }
  return out;
}

/** All files below `root`, as paths relative to `root` with forward slashes. */
export function listFiles(root: string): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else out.push(relative(root, p).split(sep).join("/"));
    }
  };
  walk(root);
  return out;
}

export function checkDirs(dirs: string[]): Violation[] {
  const all: Violation[] = [];
  for (const d of dirs) {
    if (!statSync(d).isDirectory()) throw new Error(`not a directory: ${d}`);
    for (const v of findViolations(listFiles(d))) all.push({ ...v, path: `${d}/${v.path}` });
  }
  return all;
}

if (import.meta.main) {
  const dirs = process.argv.slice(2);
  if (dirs.length === 0) {
    console.error("usage: bun installer/scripts/check-payload.ts <dir> [<dir> ...]");
    process.exit(2);
  }
  const found = checkDirs(dirs);
  for (const v of found) console.log(`${v.severity.toUpperCase()} ${v.rule}: ${v.path}`);
  const errors = found.filter((v) => v.severity === "error").length;
  console.log(errors === 0 ? "payload clean" : `${errors} forbidden file(s) in the payload`);
  process.exit(errors === 0 ? 0 : 1);
}
