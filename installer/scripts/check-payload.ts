// SPDX-License-Identifier: GPL-3.0-only
//
// Release gate for an installer payload folder (adapted from pulsar-split,
// installer/scripts/check-payload.ts, same author). Fails the build when:
//   - a PE file (.exe .dll .sys) is not native for the target architecture
//     (rules.md: ARM64 0xAA64, x64 0x8664; checked from the PE header, never the name);
//   - Microsoft runtime / system binaries are bundled (vcruntime, msvcp, ucrtbase,
//     api-ms-win-*, WebView2Loader, termsrv, ...) — binaries are built with a static CRT;
//   - certificates or keys are bundled (no certificate is ever installed, spec 90 §11 item 3);
//   - a kernel driver ships outside a driver package (INF + CAT next to it), or any
//     driver ships while `--allow-drivers` is not given (none is signed yet,
//     drivers/README.md);
//   - known vulnerable drivers: a short file-name list (fast pre-check only, renaming
//     defeats it) and, with `--blocklist <SiPolicy.xml>`, Microsoft's recommended driver
//     block rules: the Authenticode SHA-1 / SHA-256 of every .sys against the `Hash`
//     attributes of the `Deny` file rules, and the file name against `FileName` rules.
//
// The app-facing helper DLL folder is the one exception to "target arch only": it is
// loaded into apps of every architecture, so `apihelper/<x86|x64|arm64|arm64ec>/*.dll`
// must carry the machine its folder names (ARM64EC: x64 / ARM64EC / ARM64X header).
//
// `--release` (release gate): DLLs that Windows loads into its own SYSTEM processes must be
// Authenticode-signed: `pulsar-seat-credprov.dll` (LogonUI). Structural check here (a
// certificate table is present); sign-release.ps1 also asks WinVerifyTrust (Valid).
//
// `--notices` (release gate, on an extracted MSI): the licence texts the product must carry
// are present and not empty: `licenses/LICENSE.txt`, `NOTICES.md`, `SOURCE-OFFER.txt` and
// the generated `THIRD-PARTY-NOTICES-RUST.md` / `THIRD-PARTY-NOTICES-JS.md` (at least
// 1 KiB each, the size CI checks after generating them). A release MSI without the MIT /
// Apache / BSD texts of its bundled crates and packages must not be published.
//
// Remote seat kind (merge step 10, rules from Pulsar Split's installer/split/scripts/
// check-payload.ts, same author, merged here):
//   - browser web-UI assets (`*.html`, `*.htm`, folders named web / webui / www) are refused
//     (the stream server's web UI never ships);
//   - more Microsoft files: rfxvmt.dll, AcRes.dll, d3dcompiler_NN.dll;
//   - the pinned third-party driver packages of `remote/drivers/<id>/` (fetch-deps.ts,
//     SHA-256 pinned in installer/remote/drivers.json) may ship without `--allow-drivers`,
//     still only as INF + CAT packages; their kernel drivers (*.sys) warn for review; an
//     upstream installer `.exe` directly in `remote/drivers/<id>/` may be x86 (Inno Setup
//     stubs are 32-bit on every arch); every other file there is native;
//   - the per-process shim: `shim/x64/` must be x64, `shim/x86/` x86 (both packages) and
//     `shim/arm64/` ARM64 (ARM64 package only; parity-arm64, Detours backend).
//
// ARM64 package: `pulsar-seat-credprov.dll` must be ARM64X (ARM64 machine + CHPE metadata),
// so x64 processes under emulation that enumerate credential providers can load it
// (`not_arm64x`; an error with `--release`, a warning otherwise: `-NoArm64ec` test builds
// carry a plain ARM64 DLL).
//
// `--machine-only` checks just the PE machine (loose driver binaries in CI); the package
// shape rules stay with the release gate.
//
// Usage: bun installer/scripts/check-payload.ts --arch arm64|x64 [--allow-drivers]
//          [--machine-only] [--release] [--notices] [--blocklist <xml>] <dir>...
// Exit 0 = clean, 1 = violations (printed), 2 = usage error.

import { createHash } from "node:crypto";
import { closeSync, openSync, readFileSync, readSync, readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

export type Arch = "arm64" | "x64";
export const MACHINE: Record<Arch, number> = { arm64: 0xaa64, x64: 0x8664 };
export const MACHINE_NAMES: Record<number, string> = {
  0xaa64: "ARM64",
  0x8664: "x64",
  0x014c: "x86",
  0xa641: "ARM64EC/X",
};

export type Severity = "error" | "warning";
export interface Violation {
  path: string;
  rule: string;
  severity: Severity;
  detail?: string;
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
  /^termsrv\.dll$/i,
  /^rdpidd\.dll$/i,
  /^rfxvmt\.dll$/i,
  /^acres\.dll$/i,
  /^d3dcompiler_\d+\.dll$/i,
  /^msvcr\d+\.dll$/i,
];
const CERT_EXT = new Set([".pfx", ".p12", ".cer", ".crt", ".der", ".pem", ".key", ".pvk", ".spc", ".p7b", ".sst"]);
const PE_EXT = new Set([".exe", ".dll", ".sys"]);
/**
 * Subject marker of the VM test-signing certificate (tools/vm/testsign.ps1) and of the TEST
 * ONLY minifilter altitude it writes into the staged INF. A file that carries it (in an
 * Authenticode signature, a catalog or an INF, ASCII or UTF-16LE) is a test-signed VM
 * artefact and never ships.
 */
export const TEST_SIGN_MARKER = "Pulsar Seat TEST ONLY";
const TEST_SIGN_EXT = new Set([".exe", ".dll", ".sys", ".cat", ".inf", ".msi"]);
const WEB_EXT = new Set([".html", ".htm"]);
const WEB_DIRS = new Set(["web", "webui", "www"]);
const BAD_DRIVERS = new Set([
  "duodriver.sys",
  "rtcore64.sys",
  "winring0.sys",
  "winring0x64.sys",
  "gdrv.sys",
  "dbutil_2_3.sys",
  "iqvw64e.sys",
]);

function ext(name: string): string {
  const i = name.lastIndexOf(".");
  return i < 0 ? "" : name.slice(i).toLowerCase();
}

/**
 * Machine field of a PE image, or null when the bytes are not a PE file.
 * DOS header `MZ`, e_lfanew at 0x3C, then `PE\0\0` and the 2-byte machine.
 */
export function peMachine(head: Uint8Array): number | null {
  if (head.length < 0x40 || head[0] !== 0x4d || head[1] !== 0x5a) return null;
  const view = new DataView(head.buffer, head.byteOffset, head.byteLength);
  const off = view.getUint32(0x3c, true);
  if (off + 6 > head.length) return null;
  if (view.getUint32(off, true) !== 0x00004550) return null;
  return view.getUint16(off + 4, true);
}

/** First 4 KiB of a file (enough for any normal PE header). */
export function readHead(path: string, size = 4096): Uint8Array {
  const fd = openSync(path, "r");
  try {
    const buf = new Uint8Array(size);
    const n = readSync(fd, buf, 0, size, 0);
    return buf.subarray(0, n);
  } finally {
    closeSync(fd);
  }
}

/** Name rules over relative paths (forward or back slashes). */
export function findNameViolations(paths: string[], allowDrivers: boolean): Violation[] {
  const out: Violation[] = [];
  const set = new Set(paths.map((p) => p.replace(/\\/g, "/").toLowerCase()));
  for (const raw of paths) {
    const path = raw.replace(/\\/g, "/");
    const parts = path.split("/").filter((p) => p.length > 0);
    const name = parts[parts.length - 1] ?? "";
    const lower = name.toLowerCase();
    if (MS_DLL_PATTERNS.some((re) => re.test(name))) out.push({ path, rule: "microsoft_binary", severity: "error" });
    if (CERT_EXT.has(ext(name))) out.push({ path, rule: "certificate_or_key", severity: "error" });
    if (WEB_EXT.has(ext(name)) || parts.slice(0, -1).some((d) => WEB_DIRS.has(d.toLowerCase()))) {
      out.push({ path, rule: "web_ui_asset", severity: "error" });
    }
    // The shim module only ever ships in shim/<x64|x86>/ (a copy next to the manager exe
    // would be loaded by nothing and hide a bundling mistake).
    if (lower === "pulsarsplitshim.dll" && !parts.slice(0, -1).some((d) => d.toLowerCase() === "shim")) {
      out.push({ path, rule: "shim_outside_folder", severity: "error" });
    }
    if (BAD_DRIVERS.has(lower)) {
      out.push({ path, rule: "vulnerable_driver", severity: "error" });
      continue;
    }
    if (ext(name) === ".sys" || ext(name) === ".inf") {
      const pinned = remoteDriverFile(path) !== null;
      if (pinned && ext(name) === ".sys") out.push({ path, rule: "kernel_driver_review", severity: "warning" });
      if (!allowDrivers && !pinned) {
        out.push({ path, rule: "driver_not_allowed", severity: "error", detail: "no signed driver package yet" });
        continue;
      }
      // A driver package folder must hold an INF and a CAT.
      const dir = parts.slice(0, -1).join("/").toLowerCase();
      const inDir = [...set].filter((p) => p.startsWith(dir === "" ? "" : `${dir}/`) && !p.slice(dir.length + 1).includes("/"));
      const hasInf = inDir.some((p) => p.endsWith(".inf"));
      const hasCat = inDir.some((p) => p.endsWith(".cat"));
      if (!hasInf || !hasCat) out.push({ path, rule: "driver_without_package", severity: "error" });
    }
  }
  return out;
}

function indexOfBytes(hay: Uint8Array, needle: Uint8Array): number {
  outer: for (let i = 0; i + needle.length <= hay.length; i++) {
    for (let j = 0; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}

/** `test_signed` when the bytes carry [`TEST_SIGN_MARKER`] (ASCII or UTF-16LE). */
export function checkTestSigned(path: string, bytes: Uint8Array): Violation | null {
  const ascii = new TextEncoder().encode(TEST_SIGN_MARKER);
  const utf16 = new Uint8Array(ascii.length * 2);
  ascii.forEach((c, i) => (utf16[i * 2] = c));
  if (indexOfBytes(bytes, ascii) >= 0 || indexOfBytes(bytes, utf16) >= 0) {
    return { path, rule: "test_signed", severity: "error", detail: "VM test-signing artefact" };
  }
  return null;
}

/** Machines allowed in `apihelper/<folder>/` (the app-facing DLL for every app arch). */
export const APIHELPER_MACHINES: Record<string, number[]> = {
  x86: [0x014c],
  x64: [0x8664],
  arm64: [0xaa64],
  // ARM64EC images carry the x64 machine; ARM64X the ARM64 one; 0xA641 is the EC id.
  arm64ec: [0x8664, 0xa641, 0xaa64],
};

/** Machines allowed in `shim/<folder>/` per package. */
export const SHIM_MACHINES: Record<Arch, Record<string, number[]>> = {
  x64: { x64: [0x8664], x86: [0x014c] },
  arm64: { arm64: [0xaa64], x64: [0x8664], x86: [0x014c] },
};

/** DLLs the ARM64 package must ship as ARM64X (loaded by native and emulated x64 hosts). */
export const MUST_BE_ARM64X = new Set(["pulsar-seat-credprov.dll"]);

/**
 * `remote/drivers/<id>/<rest>` (the pinned third-party driver payload): `{ id, rest }`
 * with `rest` relative to the package folder, or null for any other path.
 */
export function remoteDriverFile(path: string): { id: string; rest: string } | null {
  const parts = path.replace(/\\/g, "/").split("/").filter((p) => p.length > 0);
  for (let i = 0; i + 3 < parts.length; i++) {
    if (parts[i].toLowerCase() === "remote" && parts[i + 1].toLowerCase() === "drivers") {
      return { id: parts[i + 2], rest: parts.slice(i + 3).join("/") };
    }
  }
  return null;
}

/** Allowed machines for a payload-relative path (forward slashes). */
export function allowedMachines(path: string, arch: Arch): number[] | string {
  const parts = path.replace(/\\/g, "/").toLowerCase().split("/");
  const s = parts.indexOf("shim");
  // shim/<arm64|x64|x86>/<file>
  if (s >= 0 && parts.length >= s + 3) {
    const m = SHIM_MACHINES[arch][parts[s + 1]];
    return m ?? `shim folder ${parts[s + 1]} does not belong in the ${arch} package`;
  }
  // An upstream installer stub directly in remote/drivers/<id>/ (32-bit on every arch).
  const rd = remoteDriverFile(path);
  if (rd && !rd.rest.includes("/") && ext(rd.rest) === ".exe") return [MACHINE[arch], 0x014c];
  const i = parts.indexOf("apihelper");
  // apihelper/<folder>/<file>
  if (i >= 0 && parts.length >= i + 3) {
    const m = APIHELPER_MACHINES[parts[i + 1]];
    if (!m) return `unknown apihelper folder ${parts[i + 1]}`;
    return m;
  }
  return [MACHINE[arch]];
}

/** PE machine rule for one file's header bytes. */
export function checkMachine(path: string, head: Uint8Array, arch: Arch, rel = path): Violation | null {
  const m = peMachine(head);
  if (m === null) return { path, rule: "not_a_pe_file", severity: "error" };
  const allowed = allowedMachines(rel, arch);
  if (typeof allowed === "string") return { path, rule: "wrong_machine", severity: "error", detail: allowed };
  if (!allowed.includes(m)) {
    return {
      path,
      rule: "wrong_machine",
      severity: "error",
      detail: `${MACHINE_NAMES[m] ?? `0x${m.toString(16)}`} instead of ${allowed.map((a) => MACHINE_NAMES[a] ?? `0x${a.toString(16)}`).join(" / ")}`,
    };
  }
  return null;
}

/**
 * ARM64EC check of a whole PE file: a PE32+ image whose load configuration has a non-zero
 * CHPE (hybrid) metadata pointer (IMAGE_LOAD_CONFIG_DIRECTORY64 offset 0xC8). An ARM64EC
 * DLL carries the x64 machine in its header, so the header alone cannot tell it from a
 * plain x64 DLL; the hybrid metadata can. Returns null when the bytes are not a PE32+.
 */
export function hasHybridMetadata(b: Uint8Array): boolean | null {
  if (peMachine(b.subarray(0, Math.min(b.length, 4096))) === null) return null;
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const pe = v.getUint32(0x3c, true);
  if (pe + 24 + 2 > b.length) return null;
  const nSections = v.getUint16(pe + 6, true);
  const optSize = v.getUint16(pe + 20, true);
  const opt = pe + 24;
  if (v.getUint16(opt, true) !== 0x20b) return null; // PE32+ only
  const dirs = opt + 112;
  const numDirs = v.getUint32(opt + 108, true);
  if (numDirs <= 10 || dirs + 11 * 8 > b.length) return false;
  const lcRva = v.getUint32(dirs + 10 * 8, true);
  if (lcRva === 0) return false;
  const secs = opt + optSize;
  let lcOff = -1;
  for (let i = 0; i < nSections; i++) {
    const s = secs + i * 40;
    if (s + 40 > b.length) return null;
    const vsize = v.getUint32(s + 8, true);
    const va = v.getUint32(s + 12, true);
    const rawSize = v.getUint32(s + 16, true);
    const raw = v.getUint32(s + 20, true);
    if (lcRva >= va && lcRva < va + Math.max(vsize, rawSize)) {
      lcOff = raw + (lcRva - va);
      break;
    }
  }
  if (lcOff < 0 || lcOff + 4 > b.length) return false;
  const lcSize = v.getUint32(lcOff, true);
  if (lcSize < 0xd0 || lcOff + 0xd0 > b.length) return false;
  return v.getBigUint64(lcOff + 0xc8, true) !== 0n;
}

/** ARM64 package: MUST_BE_ARM64X files carry the ARM64 machine and hybrid metadata. */
export function checkArm64x(path: string, bytes: Uint8Array, release: boolean): Violation | null {
  const m = peMachine(bytes.subarray(0, Math.min(bytes.length, 4096)));
  if (m === 0xaa64 && hasHybridMetadata(bytes) === true) return null;
  return {
    path,
    rule: "not_arm64x",
    severity: release ? "error" : "warning",
    detail: m === 0xaa64 ? "plain ARM64 image (no CHPE metadata): emulated x64 hosts cannot load it" : "not an ARM64 PE image",
  };
}

/** `apihelper/arm64ec/` must hold real ARM64EC (or ARM64X) images, not plain x64 ones. */
export function checkArm64ec(path: string, bytes: Uint8Array): Violation | null {
  const h = hasHybridMetadata(bytes);
  if (h === true) return null;
  return { path, rule: "not_arm64ec", severity: "error", detail: h === null ? "not a PE32+ image" : "no hybrid (CHPE) metadata: a plain x64 build" };
}

// ---- vulnerable driver block rules ---------------------------------------------------

export interface Blocklist {
  /** Uppercase hex Authenticode hashes (SHA-1 and SHA-256) of denied files. */
  hashes: Set<string>;
  /** Lowercase file names of denied files. */
  names: Set<string>;
}

/**
 * Parse Microsoft's recommended driver block rules (SiPolicy XML, the
 * "VulnerableDriverBlockList" download). Only `<Deny .../>` file rules are read:
 * `Hash="..."` and `FileName="..."` attributes.
 */
export function parseBlocklist(xml: string): Blocklist {
  const hashes = new Set<string>();
  const names = new Set<string>();
  for (const m of xml.matchAll(/<Deny\b([^>]*)\/?>/g)) {
    const attrs = m[1];
    const h = /\bHash="([0-9A-Fa-f]+)"/.exec(attrs);
    if (h) hashes.add(h[1].toUpperCase());
    const n = /\bFileName="([^"]+)"/.exec(attrs);
    if (n) names.add(n[1].toLowerCase());
  }
  return { hashes, names };
}

/**
 * Authenticode digests of a PE file: the whole file except the checksum field, the
 * certificate-table directory entry and the certificate data. Equals the spec's
 * section-ordered hash for normally laid out images (sections contiguous, in order).
 */
export function authenticodeHashes(b: Uint8Array): { sha1: string; sha256: string } | null {
  if (peMachine(b.subarray(0, Math.min(b.length, 4096))) === null) return null;
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const pe = v.getUint32(0x3c, true);
  const opt = pe + 24;
  if (opt + 2 > b.length) return null;
  const magic = v.getUint16(opt, true);
  const checksum = opt + 64;
  const dirs = opt + (magic === 0x20b ? 112 : 96);
  const certEntry = dirs + 4 * 8;
  if (certEntry + 8 > b.length) return null;
  const certOff = v.getUint32(certEntry, true);
  const certLen = v.getUint32(certEntry + 4, true);
  const end = certOff > 0 && certLen > 0 && certOff <= b.length ? certOff : b.length;
  const parts = [b.subarray(0, checksum), b.subarray(checksum + 4, certEntry), b.subarray(certEntry + 8, end)];
  const digest = (alg: string) => {
    const h = createHash(alg);
    for (const p of parts) h.update(p);
    return h.digest("hex").toUpperCase();
  };
  return { sha1: digest("sha1"), sha256: digest("sha256") };
}

export function checkBlocklist(path: string, bytes: Uint8Array, bl: Blocklist): Violation | null {
  const name = (path.split(/[\\/]/).pop() ?? "").toLowerCase();
  if (bl.names.has(name)) return { path, rule: "vulnerable_driver", severity: "error", detail: "blocklist file name" };
  const h = authenticodeHashes(bytes);
  if (h && (bl.hashes.has(h.sha1) || bl.hashes.has(h.sha256))) {
    return { path, rule: "vulnerable_driver", severity: "error", detail: "blocklist hash" };
  }
  return null;
}

// ---- signatures of DLLs loaded into Windows' own processes -----------------------------

/**
 * Loaded into Windows' own processes (LogonUI; TermService for the protocol provider): a
 * release ships them signed only.
 */
export const MUST_BE_SIGNED = new Set(["pulsar-seat-credprov.dll", "pulsar-seat-wrds.dll"]);

/**
 * The PE carries an Authenticode certificate table (data directory 4, a file offset):
 * non-zero, inside the file, room for a WIN_CERTIFICATE header. Structure only; validity is
 * WinVerifyTrust's job (sign-release.ps1). null = not a PE.
 */
export function hasCertificateTable(b: Uint8Array): boolean | null {
  if (peMachine(b.subarray(0, Math.min(b.length, 4096))) === null) return null;
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const opt = v.getUint32(0x3c, true) + 24;
  if (opt + 2 > b.length) return null;
  const magic = v.getUint16(opt, true);
  if (magic !== 0x20b && magic !== 0x10b) return null;
  const numDirs = v.getUint32(opt + (magic === 0x20b ? 108 : 92), true);
  const certEntry = opt + (magic === 0x20b ? 112 : 96) + 4 * 8;
  if (numDirs <= 4 || certEntry + 8 > b.length) return false;
  const off = v.getUint32(certEntry, true);
  const len = v.getUint32(certEntry + 4, true);
  return off > 0 && len >= 8 && off + len <= b.length;
}

export function checkSigned(path: string, bytes: Uint8Array): Violation | null {
  if (hasCertificateTable(bytes) === true) return null;
  return {
    path,
    rule: "unsigned_system_dll",
    severity: "error",
    detail: "LogonUI loads it as SYSTEM: Authenticode-sign it before bundling (docs/release/checklist.md §3)",
  };
}

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

export interface CheckOptions {
  allowDrivers?: boolean;
  machineOnly?: boolean;
  blocklist?: Blocklist;
  /** Release gate: MUST_BE_SIGNED files need a certificate table. */
  release?: boolean;
  /** Release gate on an extracted MSI: REQUIRED_NOTICES present and large enough. */
  notices?: boolean;
}

/** Licence files every release MSI carries (file name: minimum size in bytes). */
export const REQUIRED_NOTICES: Record<string, number> = {
  "license.txt": 1024,
  "notices.md": 1,
  "source-offer.txt": 1,
  "third-party-notices-rust.md": 1024,
  "third-party-notices-js.md": 1024,
};

/** Missing or too-small licence texts among `files` (relative paths) with their sizes. */
export function checkNotices(files: { path: string; size: number }[]): Violation[] {
  const out: Violation[] = [];
  for (const [name, min] of Object.entries(REQUIRED_NOTICES)) {
    const hits = files.filter((f) => {
      const parts = f.path.replace(/\\/g, "/").toLowerCase().split("/");
      return parts.at(-1) === name && parts.at(-2) === "licenses";
    });
    if (hits.length === 0) {
      out.push({
        path: `licenses/${name}`,
        rule: "notice-missing",
        severity: "error",
        detail: "licence text missing from the package: generate it (ci.yml 'Third-party notices') before a release build",
      });
      continue;
    }
    for (const h of hits) {
      if (h.size < min) {
        out.push({ path: h.path, rule: "notice-empty", severity: "error", detail: `${h.size} bytes, want at least ${min}` });
      }
    }
  }
  return out;
}

export function checkDirs(dirs: string[], arch: Arch, opts: CheckOptions | boolean = {}): Violation[] {
  const o: CheckOptions = typeof opts === "boolean" ? { allowDrivers: opts } : opts;
  const all: Violation[] = [];
  for (const d of dirs) {
    if (!statSync(d).isDirectory()) throw new Error(`not a directory: ${d}`);
    const files = listFiles(d);
    if (o.notices) {
      for (const v of checkNotices(files.map((f) => ({ path: f, size: statSync(join(d, f)).size })))) {
        all.push({ ...v, path: `${d}/${v.path}` });
      }
    }
    if (!o.machineOnly) {
      for (const v of findNameViolations(files, o.allowDrivers ?? false)) all.push({ ...v, path: `${d}/${v.path}` });
    }
    for (const f of files) {
      if (TEST_SIGN_EXT.has(ext(f))) {
        const t = checkTestSigned(`${d}/${f}`, readFileSync(join(d, f)));
        if (t) all.push(t);
      }
      if (!PE_EXT.has(ext(f))) continue;
      const v = checkMachine(`${d}/${f}`, readHead(join(d, f)), arch, f);
      if (v) all.push(v);
      if (!v && /(^|\/)apihelper\/arm64ec\//i.test(f)) {
        const ec = checkArm64ec(`${d}/${f}`, readFileSync(join(d, f)));
        if (ec) all.push(ec);
      }
      if (o.blocklist && ext(f) === ".sys") {
        const b = checkBlocklist(`${d}/${f}`, readFileSync(join(d, f)), o.blocklist);
        if (b) all.push(b);
      }
      if (!v && !o.machineOnly && arch === "arm64" && MUST_BE_ARM64X.has((f.split("/").pop() ?? "").toLowerCase())) {
        const x = checkArm64x(`${d}/${f}`, readFileSync(join(d, f)), o.release ?? false);
        if (x) all.push(x);
      }
      if (o.release && MUST_BE_SIGNED.has((f.split("/").pop() ?? "").toLowerCase())) {
        const s = checkSigned(`${d}/${f}`, readFileSync(join(d, f)));
        if (s) all.push(s);
      }
    }
  }
  return all;
}

export interface Args {
  arch: Arch;
  allowDrivers: boolean;
  machineOnly: boolean;
  release: boolean;
  notices: boolean;
  blocklist: string | null;
  dirs: string[];
}

export function parseArgs(argv: string[]): Args | null {
  let arch: Arch | null = null;
  let allowDrivers = false;
  let machineOnly = false;
  let release = false;
  let notices = false;
  let blocklist: string | null = null;
  const dirs: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--arch") {
      const v = argv[++i];
      if (v !== "arm64" && v !== "x64") return null;
      arch = v;
    } else if (a === "--allow-drivers") allowDrivers = true;
    else if (a === "--machine-only") machineOnly = true;
    else if (a === "--release") release = true;
    else if (a === "--notices") notices = true;
    else if (a === "--blocklist") {
      const v = argv[++i];
      if (!v) return null;
      blocklist = v;
    } else dirs.push(a);
  }
  return arch && dirs.length > 0 ? { arch, allowDrivers, machineOnly, release, notices, blocklist, dirs } : null;
}

if (import.meta.main) {
  const args = parseArgs(process.argv.slice(2));
  if (!args) {
    console.error(
      "usage: bun installer/scripts/check-payload.ts --arch arm64|x64 [--allow-drivers] [--machine-only] [--release] [--notices] [--blocklist <xml>] <dir>...",
    );
    process.exit(2);
  }
  const blocklist = args.blocklist ? parseBlocklist(readFileSync(args.blocklist, "utf8")) : undefined;
  if (blocklist && blocklist.hashes.size === 0) {
    console.error(`blocklist ${args.blocklist} has no Deny hash rules`);
    process.exit(2);
  }
  const found = checkDirs(args.dirs, args.arch, {
    allowDrivers: args.allowDrivers,
    machineOnly: args.machineOnly,
    release: args.release,
    notices: args.notices,
    blocklist,
  });
  for (const v of found) console.log(`${v.severity.toUpperCase()} ${v.rule}: ${v.path}${v.detail ? ` (${v.detail})` : ""}`);
  const errors = found.filter((v) => v.severity === "error").length;
  console.log(errors === 0 ? `payload clean (${args.arch})` : `${errors} problem(s) in the payload`);
  process.exit(errors === 0 ? 0 : 1);
}
