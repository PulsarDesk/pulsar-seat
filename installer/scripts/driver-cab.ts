// SPDX-License-Identifier: GPL-3.0-only
//
// Builds the Partner Center (hardware dashboard) submission CABs of our own drivers from
// the output of installer/scripts/build-drivers.ps1: one CAB per driver with the INF, the
// binaries and their PDBs, for ARM64 and x64, each package in its own sub-folder
// (`<driver>\<Platform>\...`; no file at the CAB root), the layout of Microsoft Learn
// "Attestation signing a kernel driver for public release" (makecab + .ddf).
//
//   bun installer/scripts/driver-cab.ts [--drivers target/drivers] [--out target/driver-submission]
//       [--config Release] [--platforms ARM64,x64] [--driver seat-wfp ...]
//       [--cargo-target-dir target/wf] [--allow-unallocated-altitude] [--dry-run]
//
// Checks before packing (each failure stops the build of that CAB):
//   - exactly one INF per package; `DriverVer` and `CatalogFile` set; every file named in
//     `[SourceDisksFiles*]` is in the package
//   - every .sys / .dll has the PE machine of its platform and a PDB next to it
//   - seat-storage-filter: the altitude is not the `UNALLOCATED` placeholder (Filter
//     Manager cannot attach without a Microsoft altitude; docs/release/
//     minifilter-altitude-request.md) unless --allow-unallocated-altitude
//   - unsigned catalogs (.cat) are left out: Microsoft creates and signs the catalog
// After packing: the CAB's own file table (read here, `listCab`) must hold exactly the
// planned paths.
// Writes `<out>/<driver>.cab` (`<driver>-<Platform>.cab` when only one platform is given,
// as in CI's per-platform drivers jobs), its `.ddf`, `<out>/manifest.json` (SHA-256 of every
// packed file and of each CAB). The CAB is UNSIGNED: the maintainer signs it with the EV
// certificate of the dashboard account before upload (installer/scripts/
// sign-authenticode.ps1; docs/release/driver-submission.md). Nothing is signed, installed
// or loaded here.

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { MACHINE, peMachine, readHead } from "./check-payload";

export type Platform = "ARM64" | "x64";
export const PLATFORMS: Platform[] = ["ARM64", "x64"];
export const DRIVERS = ["seat-input-filter", "seat-idd", "seat-storage-filter", "seat-wfp", "split-pad"] as const;
/** Rust (cargo) drivers: their PDB is in the cargo target dir, not next to the package. */
export const CARGO_DRIVERS = new Set(["split-pad"]);
const TRIPLE: Record<Platform, string> = { ARM64: "aarch64-pc-windows-msvc", x64: "x86_64-pc-windows-msvc" };
const ARCH: Record<Platform, "arm64" | "x64"> = { ARM64: "arm64", x64: "x64" };

/** INF text: UTF-16LE with BOM (our INFs), UTF-8 with or without BOM otherwise. */
export function decodeInf(b: Uint8Array): string {
  if (b.length >= 2 && b[0] === 0xff && b[1] === 0xfe) return new TextDecoder("utf-16le").decode(b.subarray(2));
  if (b.length >= 3 && b[0] === 0xef && b[1] === 0xbb && b[2] === 0xbf) return new TextDecoder().decode(b.subarray(3));
  return new TextDecoder().decode(b);
}

export interface InfFacts {
  driverVer: string | null;
  catalogFile: string | null;
  /** Lower-case file names of every `[SourceDisksFiles]` / `[SourceDisksFiles.<arch>]` entry. */
  sourceFiles: string[];
  /** `Instance1.Altitude` value of `[Strings]` (minifilters), without quotes. */
  altitude: string | null;
}

/** The INF facts the submission needs (comments stripped, section names case-insensitive). */
export function parseInf(text: string): InfFacts {
  let section = "";
  const facts: InfFacts = { driverVer: null, catalogFile: null, sourceFiles: [], altitude: null };
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/;.*$/, "").trim();
    if (!line) continue;
    const sec = /^\[([^\]]+)\]$/.exec(line);
    if (sec) {
      section = sec[1].trim().toLowerCase();
      continue;
    }
    const kv = /^([^=]+?)\s*=\s*(.*)$/.exec(line);
    if (section === "version" && kv) {
      const k = kv[1].toLowerCase();
      if (k === "driverver") facts.driverVer = kv[2].trim();
      if (k === "catalogfile" || k.startsWith("catalogfile.")) facts.catalogFile ??= kv[2].trim();
    } else if (section === "sourcedisksfiles" || section.startsWith("sourcedisksfiles.")) {
      const name = (kv ? kv[1] : line).trim().replace(/^"|"$/g, "").toLowerCase();
      if (name && !facts.sourceFiles.includes(name)) facts.sourceFiles.push(name);
    } else if (section === "strings" && kv && kv[1].trim().toLowerCase() === "instance1.altitude") {
      facts.altitude = kv[2].trim().replace(/^"|"$/g, "");
    }
  }
  return facts;
}

export interface PackageFile {
  /** Absolute source path. */
  src: string;
  /** Path inside the CAB (`<driver>\<Platform>\<name>`). */
  dest: string;
}

export interface PackagePlan {
  driver: string;
  platform: Platform;
  files: PackageFile[];
  problems: string[];
}

export interface Listing {
  /** File names in the package dir (`<base>/<driver>/<driver>/`). */
  packageFiles: string[];
  /** File names in the folder that holds the PDBs. */
  pdbFiles: string[];
  pdbDir: string;
  packageDir: string;
}

/**
 * Plan one driver package for one platform from directory listings and the INF text.
 * `machineOf` reads the PE machine of a package binary (injected for tests).
 */
export function planPackage(
  driver: string,
  platform: Platform,
  l: Listing,
  infText: string | null,
  machineOf: (path: string) => number | null,
  allowUnallocatedAltitude = false,
): PackagePlan {
  const problems: string[] = [];
  const files: PackageFile[] = [];
  const dest = (n: string) => `${driver}\\${platform}\\${n}`;
  const infs = l.packageFiles.filter((f) => f.toLowerCase().endsWith(".inf"));
  if (infs.length !== 1) problems.push(`${driver} ${platform}: ${infs.length} INF files in ${l.packageDir}, want 1`);
  const bins = l.packageFiles.filter((f) => /\.(sys|dll)$/i.test(f));
  if (bins.length === 0) problems.push(`${driver} ${platform}: no .sys / .dll in ${l.packageDir}`);
  for (const f of l.packageFiles) {
    if (/\.cat$/i.test(f)) continue; // Microsoft makes and signs the catalog
    files.push({ src: join(l.packageDir, f), dest: dest(f) });
  }
  for (const b of bins) {
    const m = machineOf(join(l.packageDir, b));
    if (m !== MACHINE[ARCH[platform]]) {
      problems.push(`${driver} ${platform}: ${b} machine ${m === null ? "unreadable" : "0x" + m.toString(16)}, want 0x${MACHINE[ARCH[platform]].toString(16)}`);
    }
    const pdb = b.replace(/\.(sys|dll)$/i, ".pdb");
    const found = l.pdbFiles.find((p) => p.toLowerCase() === pdb.toLowerCase());
    if (!found) problems.push(`${driver} ${platform}: no ${pdb} in ${l.pdbDir} (Microsoft asks for the PDBs)`);
    else files.push({ src: join(l.pdbDir, found), dest: dest(found) });
  }
  if (infText !== null) {
    const inf = parseInf(infText);
    if (!inf.driverVer) problems.push(`${driver} ${platform}: INF has no DriverVer`);
    if (!inf.catalogFile) problems.push(`${driver} ${platform}: INF has no CatalogFile (the signed catalog needs a name)`);
    const have = new Set(l.packageFiles.map((f) => f.toLowerCase()));
    for (const s of inf.sourceFiles) {
      if (!have.has(s)) problems.push(`${driver} ${platform}: INF names ${s} in [SourceDisksFiles], not in the package`);
    }
    if (inf.altitude !== null && !/^\d+(\.\d+)?$/.test(inf.altitude) && !allowUnallocatedAltitude) {
      problems.push(
        `${driver} ${platform}: minifilter altitude is "${inf.altitude}" (placeholder); request one first (docs/release/minifilter-altitude-request.md) or pass --allow-unallocated-altitude`,
      );
    }
  }
  return { driver, platform, files, problems };
}

/** makecab directive file for one CAB (files grouped by destination folder). */
export function makeDdf(cabName: string, outDir: string, files: PackageFile[]): string {
  const lines = [
    "; SPDX-License-Identifier: GPL-3.0-only",
    "; Generated by installer/scripts/driver-cab.ts - do not edit.",
    ".OPTION EXPLICIT",
    ".Set CabinetFileCountThreshold=0",
    ".Set FolderFileCountThreshold=0",
    ".Set FolderSizeThreshold=0",
    ".Set MaxCabinetSize=0",
    ".Set MaxDiskFileCount=0",
    ".Set MaxDiskSize=0",
    ".Set CompressionType=MSZIP",
    ".Set Cabinet=on",
    ".Set Compress=on",
    `.Set CabinetNameTemplate=${cabName}`,
    `.Set DiskDirectoryTemplate="${outDir}"`,
    `.Set InfFileName="${join(outDir, cabName + ".inf.txt")}"`,
    `.Set RptFileName="${join(outDir, cabName + ".rpt.txt")}"`,
  ];
  const byDir = new Map<string, PackageFile[]>();
  for (const f of files) {
    const dir = f.dest.slice(0, f.dest.lastIndexOf("\\"));
    if (!byDir.has(dir)) byDir.set(dir, []);
    byDir.get(dir)!.push(f);
  }
  for (const [dir, fs] of byDir) {
    lines.push(`.Set DestinationDir=${dir}`);
    for (const f of fs) lines.push(`"${f.src}"`);
  }
  return lines.join("\r\n") + "\r\n";
}

/**
 * File paths stored in a cabinet (MS-CAB: CFHEADER, then `cFiles` CFFILE records at
 * `coffFiles`, each with a NUL-terminated name that keeps its folder). `expand -D` prints
 * only the base names, so the check reads the format itself. Throws on a malformed file.
 */
export function listCab(b: Uint8Array): string[] {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  if (b.length < 36 || b[0] !== 0x4d || b[1] !== 0x53 || b[2] !== 0x43 || b[3] !== 0x46) throw new Error("not a cabinet (no MSCF)");
  const coffFiles = dv.getUint32(16, true);
  const cFiles = dv.getUint16(28, true);
  const names: string[] = [];
  let p = coffFiles;
  for (let i = 0; i < cFiles; i++) {
    if (p + 16 > b.length) throw new Error("cabinet truncated in CFFILE");
    const attribs = dv.getUint16(p + 14, true);
    let e = p + 16;
    while (e < b.length && b[e] !== 0) e++;
    if (e >= b.length) throw new Error("cabinet truncated in a file name");
    const raw = b.subarray(p + 16, e);
    // _A_NAME_IS_UTF (0x80): UTF-8 name; otherwise the OEM code page (ASCII for ours).
    names.push(attribs & 0x80 ? new TextDecoder().decode(raw) : String.fromCharCode(...raw));
    p = e + 1;
  }
  return names;
}

/** `<driver>` for a CAB with both platforms, `<driver>-<Platform>` for one (CI builds each platform in its own job). */
export function cabStem(driver: string, platforms: Platform[]): string {
  const all = PLATFORMS.every((p) => platforms.includes(p));
  return all ? driver : `${driver}-${platforms.join("-")}`;
}

function sha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function arg(argv: string[], name: string): string | undefined {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
}

function list(dir: string): string[] {
  return existsSync(dir) ? readdirSync(dir).filter((f) => statSync(join(dir, f)).isFile()) : [];
}

async function main(argv: string[]) {
  const repo = resolve(import.meta.dir, "..", "..");
  const driversRoot = resolve(repo, arg(argv, "--drivers") ?? "target/drivers");
  const out = resolve(repo, arg(argv, "--out") ?? "target/driver-submission");
  const config = arg(argv, "--config") ?? "Release";
  const platforms = (arg(argv, "--platforms") ?? PLATFORMS.join(",")).split(",").filter(Boolean) as Platform[];
  const cargoTarget = resolve(repo, arg(argv, "--cargo-target-dir") ?? process.env.CARGO_TARGET_DIR ?? "target/wf");
  const allowAlt = argv.includes("--allow-unallocated-altitude");
  const dryRun = argv.includes("--dry-run");
  const picked = argv.flatMap((a, i) => (a === "--driver" ? [argv[i + 1]] : []));
  const drivers = picked.length ? picked : [...DRIVERS];
  for (const p of platforms) if (!PLATFORMS.includes(p)) throw new Error(`unknown platform ${p}`);
  for (const d of drivers) if (!(DRIVERS as readonly string[]).includes(d)) throw new Error(`unknown driver ${d}`);
  mkdirSync(out, { recursive: true });

  const manifest: Record<string, unknown> = {
    generated: new Date().toISOString(),
    config,
    platforms,
    signed: false,
    altitude_placeholder_allowed: allowAlt,
    drivers: {},
  };
  let failed = false;
  for (const driver of drivers) {
    const plans: PackagePlan[] = [];
    for (const platform of platforms) {
      const base = join(driversRoot, platform, config, driver);
      const packageDir = join(base, driver);
      const pdbDir = CARGO_DRIVERS.has(driver) ? join(cargoTarget, TRIPLE[platform], config === "Release" ? "release" : "debug") : base;
      const packageFiles = list(packageDir);
      const infName = packageFiles.find((f) => f.toLowerCase().endsWith(".inf"));
      const infText = infName ? decodeInf(readFileSync(join(packageDir, infName))) : null;
      const pdbFiles = list(pdbDir).filter((f) => f.toLowerCase().endsWith(".pdb"));
      plans.push(
        planPackage(driver, platform, { packageFiles, pdbFiles, pdbDir, packageDir }, infText, (p) => peMachine(readHead(p)), allowAlt),
      );
    }
    const problems = plans.flatMap((p) => p.problems);
    if (problems.length) {
      failed = true;
      for (const p of problems) console.error(`  ${p}`);
      console.error(`${driver}: not packed`);
      continue;
    }
    const files = plans.flatMap((p) => p.files);
    const stem = cabStem(driver, platforms);
    const cab = `${stem}.cab`;
    const ddfPath = join(out, `${stem}.ddf`);
    writeFileSync(ddfPath, makeDdf(cab, out, files));
    if (dryRun) {
      console.log(`${driver}: ${files.length} files planned (dry run, ${ddfPath})`);
      continue;
    }
    const mk = Bun.spawnSync(["makecab.exe", "/F", ddfPath], { stdout: "pipe", stderr: "pipe" });
    if (mk.exitCode !== 0) {
      failed = true;
      console.error(`${driver}: makecab exit ${mk.exitCode}\n${mk.stdout.toString()}${mk.stderr.toString()}`);
      continue;
    }
    const cabPath = join(out, cab);
    const inside = new Set(listCab(readFileSync(cabPath)).map((s) => s.toLowerCase()));
    const missing = files.filter((f) => !inside.has(f.dest.toLowerCase()));
    if (missing.length || inside.size !== files.length) {
      failed = true;
      console.error(`${driver}: CAB content does not match the plan (${inside.size} inside, ${files.length} planned, missing ${missing.map((m) => m.dest).join(", ")})`);
      continue;
    }
    (manifest.drivers as Record<string, unknown>)[driver] = {
      cab,
      cab_sha256: sha256(cabPath),
      files: files.map((f) => ({ path: f.dest, sha256: sha256(f.src), size: statSync(f.src).size })),
    };
    console.log(`${driver}: ${cabPath} (${files.length} files: ${files.map((f) => basename(f.dest)).join(", ")})`);
  }
  writeFileSync(join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  if (failed) {
    console.error("driver-cab: FAILED");
    process.exit(1);
  }
  console.log(`unsigned submission CABs in ${out}; EV-sign them before upload (docs/release/driver-submission.md)`);
}

if (import.meta.main) await main(process.argv.slice(2));
