// SPDX-License-Identifier: GPL-3.0-only
//
// Build-time (CI) step of the one MSI (merge step 10, from Pulsar Split): download the
// third-party artefacts of the remote seat kind pinned in installer/remote/drivers.json and
// installer/remote/stream-server.json, verify their SHA-256, lay them out for the MSI and
// run check-payload on the result.
//
//   bun installer/scripts/fetch-deps.ts --arch x64|arm64 [--out target/installer-deps] [--only drivers|stream]
//     [--stream-local target/wf/sunshine-out/<arch>]   use the Pulsar build of the stream server
//                                                     (stream-server/build-sunshine.ps1) for the exe
//
// Output (per arch; build-installer.ps1 copies <out>/<arch>/remote into the payload, and
// bundle-config.ts turns it into the generated WiX fragment PulsarSeatRemote.wxs):
//   <out>/<arch>/remote/drivers.json              resolved manifest (only this arch, INF globs resolved)
//   <out>/<arch>/remote/drivers/<id>/...          extracted INF package or the official installer exe
//   <out>/<arch>/remote/stream-server/...         stream server without its browser web UI
//   <out>/<arch>/remote/stream-server.json        runtime ServerDist {flavour, version, file, sha256, source_url, licence}
//   <out>/cache/<sha256>-<file>                   verified downloads (reused when the hash matches)
// The service reads them from `<install>\remote\` (seat-setup `layout::REMOTE_PAYLOAD_DIR`).
//
// Fails (exit 1) on: hash mismatch, missing/ambiguous INF, INF without a section for the
// arch, INF without the manifest's root hardware id, web UI left in the payload, any
// check-payload error. Network: HTTPS GETs to the pinned URLs only. Nothing is executed
// except the system tar.exe (bsdtar) to unpack zip archives.

import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { checkDirs } from "./check-payload.ts";

/** Folder of the remote payload, in the deps output and under the install root. */
export const REMOTE_DIR = "remote";

export type Arch = "x64" | "arm64";

export interface DriverPackage {
  id: string;
  version: string;
  arch: string[];
  url: string;
  sha256: string;
  licence: string;
  licence_url: string;
  install:
    | { kind: "official_installer"; args: string[]; requires_consent?: boolean }
    | { kind: "inf_package"; inf: string; root_hardware_id?: string; class_guid?: string }
    | { kind: "detect_only" };
  detect: { kind: string; value: string };
  min_build?: number;
  default_selected?: boolean;
  archive?: string;
  notes?: string;
}

export interface StreamManifest {
  flavour: "upstream" | "pulsar_fork";
  version: string;
  licence: string;
  source_url: string;
  exe: string;
  install_dir: string;
  web_ui_dirs: string[];
  /**
   * Paths (relative to the exe's folder; a folder ends in `/`) left out of the payload:
   * upstream's own service / firewall / driver installers, which the product never runs.
   */
  drop_paths?: string[];
  builds: Record<string, { url: string; sha256: string }>;
}

export interface ServerDist {
  flavour: string;
  version: string;
  file: string;
  sha256: string;
  source_url: string;
  licence: string;
}

// ------------------------------------------------------------------ pure helpers (tested)

export function sha256Hex(data: Uint8Array): string {
  return createHash("sha256").update(data).digest("hex");
}

export function isHex64(s: string): boolean {
  return /^[0-9a-f]{64}$/.test(s);
}

/** `*` wildcard on a file name, case-insensitive. */
export function globMatch(pattern: string, name: string): boolean {
  const re = new RegExp(
    "^" + pattern.split("*").map((p) => p.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*") + "$",
    "i",
  );
  return re.test(name);
}

/** Problems with an INF for `arch` (empty = fine). Text checks only; nothing is parsed further. */
export function infProblems(text: string, arch: Arch, rootHardwareId?: string): string[] {
  const problems: string[] = [];
  const decoration = arch === "x64" ? /\bNTamd64\b/i : /\bNTarm64\b/i;
  if (!decoration.test(text)) problems.push(`INF has no ${arch === "x64" ? "NTamd64" : "NTarm64"} models section`);
  if (rootHardwareId && !text.toLowerCase().includes(rootHardwareId.toLowerCase())) {
    problems.push(`INF does not contain the root hardware id ${rootHardwareId}`);
  }
  return problems;
}

/** INF files are often UTF-16LE with a BOM. */
export function decodeInf(bytes: Uint8Array): string {
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) return new TextDecoder("utf-16le").decode(bytes.subarray(2));
  return new TextDecoder("utf-8").decode(bytes);
}

/** Is `rel` (forward slashes) inside one of the web UI folders (matched at any depth)? */
export function isWebUiPath(rel: string, webDirs: string[]): boolean {
  const p = "/" + rel.replace(/\\/g, "/").toLowerCase();
  return webDirs.some((d) => p.includes("/" + d.toLowerCase().replace(/^\/|\/$/g, "") + "/"));
}

/** Is `rel` one of the `drop_paths` (that exact file, or below a folder ending in `/`)? */
export function isDroppedPath(rel: string, drop: string[] = []): boolean {
  const p = rel.replace(/\\/g, "/").toLowerCase();
  return drop.some((d) => {
    const x = d.replace(/\\/g, "/").toLowerCase();
    return x.endsWith("/") ? p.startsWith(x) : p === x;
  });
}

export function packagesFor(pkgs: DriverPackage[], arch: Arch): DriverPackage[] {
  return pkgs.filter((p) => p.arch.includes(arch));
}

export function validateDrivers(pkgs: DriverPackage[]): string[] {
  const errs: string[] = [];
  for (const p of pkgs) {
    if (!p.url.startsWith("https://")) errs.push(`${p.id}: url must be https`);
    if (p.install.kind !== "detect_only" && !isHex64(p.sha256)) errs.push(`${p.id}: sha256 not pinned`);
  }
  return errs;
}

/**
 * The stream-server binary must come from the repo and tag the GPL source offer names:
 * source_url `https://github.com/<owner>/<repo>/tree/<version>` and every build URL
 * `https://github.com/<owner>/<repo>/releases/download/<version>/<file>` (same owner/repo,
 * compared case-insensitively, same tag), sha256 pinned.
 */
export function validateStream(m: StreamManifest): string[] {
  const errs: string[] = [];
  const src = /^https:\/\/github\.com\/([^/]+)\/([^/]+)\/tree\/([^/]+)$/.exec(m.source_url);
  if (!src) return [`source_url must be https://github.com/<owner>/<repo>/tree/<tag>: ${m.source_url}`];
  const repo = `${src[1]}/${src[2]}`.toLowerCase();
  if (src[3] !== m.version) errs.push(`source_url tag ${src[3]} != version ${m.version}`);
  for (const [arch, b] of Object.entries(m.builds)) {
    const u = /^https:\/\/github\.com\/([^/]+)\/([^/]+)\/releases\/download\/([^/]+)\/[^/]+$/.exec(b.url);
    if (!u) {
      errs.push(`${arch}: build url is not a GitHub release asset: ${b.url}`);
      continue;
    }
    if (`${u[1]}/${u[2]}`.toLowerCase() !== repo) errs.push(`${arch}: build repo ${u[1]}/${u[2]} != source repo ${src[1]}/${src[2]}`);
    if (u[3] !== m.version) errs.push(`${arch}: build tag ${u[3]} != version ${m.version}`);
    if (!isHex64(b.sha256)) errs.push(`${arch}: sha256 not pinned`);
  }
  return errs;
}

/**
 * `--stream-local <dir>`: the output of stream-server/build-sunshine.ps1 (the Pulsar build
 * of the stream server). Its `stream-server.json` must describe a `pulsar_fork` build of the
 * pinned upstream version whose exe hash matches the file next to it. The upstream payload
 * (assets, shaders) stays; only the exe is replaced. Returns the problems found.
 */
export function validateLocalBuild(m: StreamManifest, local: ServerDist, exeSha256: string): string[] {
  const errs: string[] = [];
  if (local.flavour !== "pulsar_fork") errs.push(`local build flavour must be pulsar_fork: ${local.flavour}`);
  if (!local.version.startsWith(`${m.version}-pulsar.`)) errs.push(`local build ${local.version} is not a Pulsar build of ${m.version}`);
  if (!isHex64(local.sha256) || local.sha256 !== exeSha256) errs.push("local build exe does not match its manifest sha256");
  if (local.file.toLowerCase() !== m.exe.toLowerCase()) errs.push(`local build file must be ${m.exe}: ${local.file}`);
  if (local.licence !== "GPL-3.0-only") errs.push(`local build licence must be GPL-3.0-only: ${local.licence}`);
  if (!local.source_url.includes(`/tree/${m.version}`)) errs.push(`local build source_url must name ${m.version}`);
  return errs;
}

/**
 * `file` of the runtime manifest: the exe's bare file name. The service resolves it inside
 * `remote\<install_dir>\` and refuses any path (`seat_stream::dist::validate_parts`); gaming
 * PC round 4 (2026-11-25): `stream-server\sunshine.exe` made every remote seat fail with
 * `bad_manifest` ("sunshine stream backend not installed").
 */
export function exeFileName(exeRelPath: string): string {
  return exeRelPath.split(/[\\/]/).pop() ?? exeRelPath;
}

/** Runtime manifest of the payload with the local Pulsar build's exe. */
export function localServerDist(local: ServerDist, exeRelPath: string): ServerDist {
  return { ...local, file: exeFileName(exeRelPath) };
}

export function serverDist(m: StreamManifest, exeRelPath: string, exeSha256: string): ServerDist {
  return {
    flavour: m.flavour,
    version: m.version,
    file: exeFileName(exeRelPath),
    sha256: exeSha256,
    source_url: m.source_url,
    licence: m.licence,
  };
}

// ------------------------------------------------------------------ I/O

function walk(root: string): string[] {
  const out: string[] = [];
  const rec = (d: string) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) rec(p);
      else out.push(relative(root, p).split(sep).join("/"));
    }
  };
  rec(root);
  return out;
}

async function download(url: string, sha256: string, cacheDir: string): Promise<string> {
  mkdirSync(cacheDir, { recursive: true });
  const file = join(cacheDir, `${sha256}-${basename(new URL(url).pathname)}`);
  if (existsSync(file) && sha256Hex(readFileSync(file)) === sha256) return file;
  console.log(`download ${url}`);
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`);
  if (!res.url.startsWith("https://")) throw new Error(`redirected off https: ${res.url}`);
  const body = new Uint8Array(await res.arrayBuffer());
  const actual = sha256Hex(body);
  if (actual !== sha256) throw new Error(`sha256 mismatch for ${url}: expected ${sha256}, got ${actual}`);
  writeFileSync(file + ".partial", body);
  renameSync(file + ".partial", file);
  return file;
}

async function unzip(zip: string, dest: string): Promise<void> {
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  const tar = join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
  const p = Bun.spawn([tar, "-xf", zip, "-C", dest], { stdout: "inherit", stderr: "inherit" });
  if ((await p.exited) !== 0) throw new Error(`tar failed for ${zip}`);
}

async function fetchDrivers(root: string, arch: Arch, out: string, cache: string): Promise<void> {
  const all: DriverPackage[] = JSON.parse(readFileSync(join(root, "installer", "remote", "drivers.json"), "utf8"));
  const errs = validateDrivers(all);
  if (errs.length) throw new Error(errs.join("\n"));
  const resolved: DriverPackage[] = [];
  for (const p of packagesFor(all, arch)) {
    if (p.install.kind === "detect_only") {
      resolved.push({ ...p, arch: [arch] });
      continue;
    }
    const file = await download(p.url, p.sha256, cache);
    const dest = join(out, "drivers", p.id);
    if (p.install.kind === "official_installer") {
      rmSync(dest, { recursive: true, force: true });
      mkdirSync(dest, { recursive: true });
      cpSync(file, join(dest, basename(new URL(p.url).pathname)));
      resolved.push({ ...p, arch: [arch] });
      continue;
    }
    if (p.archive !== "zip") throw new Error(`${p.id}: inf_package needs archive "zip"`);
    await unzip(file, dest);
    const pattern = basename(p.install.inf.replace(/\\/g, "/"));
    const infs = walk(dest).filter((f) => globMatch(pattern, basename(f)));
    if (infs.length !== 1) throw new Error(`${p.id}: expected exactly one INF matching ${pattern}, found ${infs.length}`);
    const problems = infProblems(decodeInf(readFileSync(join(dest, infs[0]))), arch, p.install.root_hardware_id);
    if (problems.length) throw new Error(`${p.id}: ${problems.join("; ")}`);
    resolved.push({ ...p, arch: [arch], install: { ...p.install, inf: infs[0].replace(/\//g, "\\") } });
  }
  writeFileSync(join(out, "drivers.json"), JSON.stringify(resolved, null, 2) + "\n");
}

async function fetchStream(root: string, arch: Arch, out: string, cache: string, localDir?: string): Promise<void> {
  const m: StreamManifest = JSON.parse(readFileSync(join(root, "installer", "remote", "stream-server.json"), "utf8"));
  const errs = validateStream(m);
  if (errs.length) throw new Error(errs.join("\n"));
  const b = m.builds[arch];
  if (!b) throw new Error(`no stream-server build for ${arch}`);
  const zip = await download(b.url, b.sha256, cache);
  const tmp = join(out, ".stream-extract");
  await unzip(zip, tmp);
  const files = walk(tmp);
  const exe = files.find((f) => basename(f).toLowerCase() === m.exe.toLowerCase());
  if (!exe) throw new Error(`${m.exe} not found in ${b.url}`);
  const base = dirname(exe) === "." ? "" : dirname(exe);
  const dest = join(out, m.install_dir);
  rmSync(dest, { recursive: true, force: true });
  let dropped = 0;
  for (const f of files) {
    if (base && !f.startsWith(base + "/")) continue;
    const rel = base ? f.slice(base.length + 1) : f;
    if (isWebUiPath(rel, m.web_ui_dirs) || /\.html?$/i.test(rel) || isDroppedPath(rel, m.drop_paths)) {
      dropped++;
      continue;
    }
    mkdirSync(dirname(join(dest, rel)), { recursive: true });
    cpSync(join(tmp, f), join(dest, rel));
  }
  rmSync(tmp, { recursive: true, force: true });
  console.log(`stream server: dropped ${dropped} web UI / installer file(s)`);
  const exeRel = `${m.install_dir}/${basename(exe)}`;
  let dist = serverDist(m, exeRel, sha256Hex(readFileSync(join(out, exeRel))));
  if (localDir) {
    // The Pulsar build (stream-server/build-sunshine.ps1) replaces the upstream exe.
    const local: ServerDist = JSON.parse(readFileSync(join(localDir, "stream-server.json"), "utf8").replace(/^﻿/, ""));
    const bytes = readFileSync(join(localDir, local.file));
    const errs = validateLocalBuild(m, local, sha256Hex(bytes));
    if (errs.length) throw new Error(errs.join("\n"));
    writeFileSync(join(out, exeRel), bytes);
    dist = localServerDist(local, exeRel);
    console.log(`stream server: Pulsar build ${local.version} from ${localDir}`);
  }
  writeFileSync(join(out, "stream-server.json"), JSON.stringify(dist, null, 2) + "\n");
}

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

async function main(): Promise<void> {
  const root = resolve(import.meta.dir, "..", "..");
  const arch = (arg("--arch") ?? (process.arch === "arm64" ? "arm64" : "x64")) as Arch;
  if (arch !== "x64" && arch !== "arm64") throw new Error(`unsupported --arch ${arch}`);
  const base = resolve(root, arg("--out") ?? join("target", "installer-deps"));
  const out = join(base, arch, REMOTE_DIR);
  const cache = join(base, "cache");
  const only = arg("--only");
  mkdirSync(out, { recursive: true });
  if (!only || only === "drivers") await fetchDrivers(root, arch, out, cache);
  const local = arg("--stream-local");
  if (!only || only === "stream") await fetchStream(root, arch, out, cache, local ? resolve(root, local) : undefined);
  // The same rules as the whole MSI payload, on the folder as it will sit in it.
  const found = checkDirs([join(base, arch)], arch, { allowDrivers: false });
  for (const v of found) console.error(`${v.severity.toUpperCase()} ${v.rule}: ${v.path}${v.detail ? ` (${v.detail})` : ""}`);
  if (found.some((v) => v.severity === "error")) throw new Error("payload check failed");
  console.log(`ok: ${out} (${statSync(out).isDirectory() ? "ready for build-installer.ps1" : ""})`);
}

if (import.meta.main) {
  main().catch((e) => {
    console.error(String(e instanceof Error ? e.message : e));
    process.exit(1);
  });
}
