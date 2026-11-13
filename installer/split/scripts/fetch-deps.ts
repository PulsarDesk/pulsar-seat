// SPDX-License-Identifier: GPL-3.0-only
//
// Build-time (CI) step: download the third-party artefacts pinned in
// installer/split/drivers.json and installer/split/stream-server.json, verify their SHA-256, lay them
// out for the Tauri bundle and run check-payload on the result.
//
//   bun installer/split/scripts/fetch-deps.ts --arch x64|arm64 [--out target/installer-deps] [--only drivers|stream]
//
// Output (per arch, plus a copy in <out>/current for installer/split/tauri.release.conf.json):
//   <out>/<arch>/drivers.json              resolved manifest (only this arch, INF globs resolved)
//   <out>/<arch>/drivers/<id>/...          extracted INF package or the official installer exe
//   <out>/<arch>/stream-server/...         stream server without its browser web UI
//   <out>/<arch>/stream-server.json        runtime ServerDist {flavour, version, file, sha256, source_url, licence}
//   <out>/cache/<sha256>-<file>            verified downloads (reused when the hash matches)
//
// Fails (exit 1) on: hash mismatch, missing/ambiguous INF, INF without a section for the
// arch, INF without the manifest's root hardware id, web UI left in the payload, any
// check-payload error. Network: HTTPS GETs to the pinned URLs only. Nothing is executed
// except the system tar.exe (bsdtar) to unpack zip archives.

import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { checkDirs } from "./check-payload.ts";

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

export function serverDist(m: StreamManifest, exeRelPath: string, exeSha256: string): ServerDist {
  return {
    flavour: m.flavour,
    version: m.version,
    file: exeRelPath.replace(/\//g, "\\"),
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
  const all: DriverPackage[] = JSON.parse(readFileSync(join(root, "installer", "split", "drivers.json"), "utf8"));
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

async function fetchStream(root: string, arch: Arch, out: string, cache: string): Promise<void> {
  const m: StreamManifest = JSON.parse(readFileSync(join(root, "installer", "split", "stream-server.json"), "utf8"));
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
    if (isWebUiPath(rel, m.web_ui_dirs) || /\.html?$/i.test(rel)) {
      dropped++;
      continue;
    }
    mkdirSync(dirname(join(dest, rel)), { recursive: true });
    cpSync(join(tmp, f), join(dest, rel));
  }
  rmSync(tmp, { recursive: true, force: true });
  console.log(`stream server: dropped ${dropped} web UI file(s)`);
  const exeRel = `${m.install_dir}/${basename(exe)}`;
  const dist = serverDist(m, exeRel, sha256Hex(readFileSync(join(out, exeRel))));
  writeFileSync(join(out, "stream-server.json"), JSON.stringify(dist, null, 2) + "\n");
}

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

async function main(): Promise<void> {
  const root = resolve(import.meta.dir, "..", "..", "..");
  const arch = (arg("--arch") ?? (process.arch === "arm64" ? "arm64" : "x64")) as Arch;
  if (arch !== "x64" && arch !== "arm64") throw new Error(`unsupported --arch ${arch}`);
  const base = resolve(root, arg("--out") ?? join("target", "installer-deps"));
  const out = join(base, arch);
  const cache = join(base, "cache");
  const only = arg("--only");
  mkdirSync(out, { recursive: true });
  if (!only || only === "drivers") await fetchDrivers(root, arch, out, cache);
  if (!only || only === "stream") await fetchStream(root, arch, out, cache);
  const errors = checkDirs([out]).filter((v) => v.severity === "error");
  for (const v of errors) console.error(`ERROR ${v.rule}: ${v.path}`);
  if (errors.length) throw new Error("payload check failed");
  const current = join(base, "current");
  rmSync(current, { recursive: true, force: true });
  cpSync(out, current, { recursive: true });
  console.log(`ok: ${out} (copied to ${current}); ${statSync(out).isDirectory() ? "ready for tauri build" : ""}`);
}

if (import.meta.main) {
  main().catch((e) => {
    console.error(String(e instanceof Error ? e.message : e));
    process.exit(1);
  });
}
