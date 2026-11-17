// SPDX-License-Identifier: GPL-3.0-only
//
// Writes installer/tauri-bundle.json without its "$comment" keys, for
// `tauri build --config <out>`, plus the generated WiX fragments of the driver packages
// (`PulsarSeatDriverPackages`, empty unless signed packages were staged) and of the remote
// seat kind's third-party payload (`PulsarSeatRemotePayload`, empty without `--remote`).
//
// Usage: bun installer/scripts/bundle-config.ts [out=target/tauri-bundle.json]
//          [--drivers <staged dir>]   one sub-folder per signed package (inf + cat + binaries)
//          [--version <x.y.z>]        product version (defaults to tauri.conf.json's)
//          [--bin <payload dir>]      write a resolved copy of wix/PulsarSeat.wxs next to <out>
//          [--no-arm64ec]             leave the ARM64EC helper out (test builds only)
//          [--language <culture>]     MSI culture: en-US (default) or tr-TR; the fragment's
//                                     strings come from wix/locales/<culture>.wxl
//          [--remote <dir>]           the remote kind's pinned third-party payload
//                                     (fetch-deps.ts output: drivers.json, drivers/,
//                                     stream-server.json, stream-server/), installed to
//                                     <install>\remote\ by the generated PulsarSeatRemote.wxs
//
// The generated fragments always define their component groups, so the groups always
// resolve. The shim group (`PulsarSeatShim`, both packages; arm64 adds shim\arm64) is static in
// wix/PulsarSeat.wxs and referenced from tauri-bundle.json.
//
// The drivers folder is filled by build-installer.ps1 only with -SignedDrivers, and only
// with packages whose catalog carries a valid Microsoft hardware signature. The install
// step (seat-compat) checks the signature again before anything reaches the driver store.
// The remote payload is hash-pinned by installer/remote/*.json and checked by
// check-payload.ts (rules for `remote/`); seat-setup installs from it only on request.

import { createHash } from 'node:crypto';
import { existsSync, readdirSync, statSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

export function stripComments(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(stripComments);
  if (v && typeof v === 'object') {
    const o: Record<string, unknown> = {};
    for (const [k, x] of Object.entries(v)) if (k !== '$comment') o[k] = stripComments(x);
    return o;
  }
  return v;
}

export interface DriverPackage {
  /** Folder name = package name (letters, digits, `-`, `_`). */
  name: string;
  /** Absolute paths of the package files (inf, cat, sys / dll). */
  files: string[];
}

const SAFE = /^[A-Za-z0-9_.-]+$/;

/** Packages under `dir`: every sub-folder that holds an INF. Refuses odd names. */
export function listDriverPackages(dir: string | undefined): DriverPackage[] {
  if (!dir || !existsSync(dir)) return [];
  const out: DriverPackage[] = [];
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    if (!statSync(p).isDirectory()) continue;
    if (!SAFE.test(name)) throw new Error(`driver package folder name not allowed: ${name}`);
    const files = readdirSync(p)
      .sort()
      .filter((f) => statSync(join(p, f)).isFile());
    if (!files.some((f) => f.toLowerCase().endsWith('.inf'))) continue;
    if (!files.some((f) => f.toLowerCase().endsWith('.cat'))) throw new Error(`driver package ${name} has no catalog (.cat)`);
    for (const f of files) if (!SAFE.test(f)) throw new Error(`driver file name not allowed: ${name}/${f}`);
    out.push({ name, files: files.map((f) => resolve(p, f)) });
  }
  return out;
}

function xmlAttr(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/** WiX v3 identifier from a free string (max 72 chars). */
function wixId(prefix: string, s: string, i: number): string {
  return `${prefix}${i}_${s.replace(/[^A-Za-z0-9_]/g, '_')}`.slice(0, 70);
}

/**
 * WiX v3 fragment: `ComponentGroup PulsarSeatDriverPackages` with one directory per package
 * under `PulsarSeatDriversDir` (installer/wix/PulsarSeat.wxs). Empty group when none.
 */
export function driversFragment(pkgs: DriverPackage[]): string {
  const dirs: string[] = [];
  const refs: string[] = [];
  pkgs.forEach((p, i) => {
    const dirId = wixId('PsDrvDir', p.name, i);
    const comps = p.files.map((f, j) => {
      const id = wixId(`PsDrv${i}F`, f.split(/[\\/]/).pop()!, j);
      refs.push(`      <ComponentRef Id="${id}" />`);
      return [
        `        <Component Id="${id}" Guid="*" Win64="yes">`,
        `          <File Id="${id}" Source="${xmlAttr(f)}" KeyPath="yes" />`,
        `        </Component>`
      ].join('\n');
    });
    dirs.push([`      <Directory Id="${dirId}" Name="${xmlAttr(p.name)}">`, ...comps, `      </Directory>`].join('\n'));
  });
  return [
    '<?xml version="1.0" encoding="utf-8"?>',
    '<!-- SPDX-License-Identifier: GPL-3.0-only -->',
    '<!-- Generated by installer/scripts/bundle-config.ts: signed driver packages staged for',
    '     this build (empty unless build-installer.ps1 ran with -SignedDrivers). -->',
    '<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">',
    '  <Fragment>',
    // PulsarSeat.wxs schedules a reboot (msiexec exit 3010) when this build stages drivers.
    ...(pkgs.length ? ['    <Property Id="PSHASDRIVERS" Value="1" />'] : []),
    '    <DirectoryRef Id="PulsarSeatDriversDir">',
    ...dirs,
    '    </DirectoryRef>',
    '    <ComponentGroup Id="PulsarSeatDriverPackages">',
    // An empty group is valid WiX; the drivers folder itself comes from PulsarSeat.wxs.
    ...refs,
    '    </ComponentGroup>',
    '  </Fragment>',
    '</Wix>',
    ''
  ].join('\n');
}

/** Stable WiX id for a payload-relative path: prefix + 24 hex digits of its SHA-1. */
function hashId(prefix: string, rel: string): string {
  return `${prefix}${createHash('sha1').update(rel.toLowerCase()).digest('hex').slice(0, 24)}`;
}

/** One folder of the remote payload: file names and sub-folders, sorted. */
export interface Tree {
  files: string[];
  dirs: Map<string, Tree>;
}

/** The tree below `dir` (sorted, so the same payload gives the same fragment). */
export function readTree(dir: string): Tree {
  const t: Tree = { files: [], dirs: new Map() };
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) t.dirs.set(name, readTree(p));
    else t.files.push(name);
  }
  return t;
}

/**
 * WiX v3 fragment: `ComponentGroup PulsarSeatRemotePayload` with one component per file of
 * `tree` (found below `root` on disk), installed to `<INSTALLDIR>remote\`. Without a payload
 * (`root` null) the group is empty and no folder is created.
 */
export function remoteFragment(root: string | null, tree: Tree | null): string {
  const refs: string[] = [];
  const body: string[] = [];
  const base = (root ?? '').replace(/[\\/]+$/, '');
  if (root !== null && /[<>"&]/.test(root)) throw new Error(`remote payload path not allowed in WiX: ${root}`);
  const walk = (t: Tree, rel: string, indent: string) => {
    for (const f of t.files) {
      const r = `${rel}/${f}`;
      const id = hashId('PsRemF', r);
      refs.push(`      <ComponentRef Id="${id}" />`);
      const src = `${base}${r}`.replace(/\//g, '\\');
      body.push(
        `${indent}<Component Id="${id}" Guid="*" Win64="yes">`,
        `${indent}  <File Id="${id}" Name="${xmlAttr(f)}" Source="${xmlAttr(src)}" KeyPath="yes" />`,
        `${indent}</Component>`
      );
    }
    for (const [name, sub] of t.dirs) {
      const r = `${rel}/${name}`;
      body.push(`${indent}<Directory Id="${hashId('PsRemD', r)}" Name="${xmlAttr(name)}">`);
      walk(sub, r, `${indent}  `);
      body.push(`${indent}</Directory>`);
    }
  };
  if (root !== null && tree !== null) walk(tree, '', '        ');
  return [
    '<?xml version="1.0" encoding="utf-8"?>',
    '<!-- SPDX-License-Identifier: GPL-3.0-only -->',
    "<!-- Generated by installer/scripts/bundle-config.ts: the remote seat kind's pinned",
    '     third-party payload (installer/scripts/fetch-deps.ts), installed to <install>\\remote. -->',
    '<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">',
    '  <Fragment>',
    ...(body.length
      ? [
          '    <DirectoryRef Id="INSTALLDIR">',
          '      <Directory Id="PulsarSeatRemoteDir" Name="remote">',
          ...body,
          '      </Directory>',
          '    </DirectoryRef>'
        ]
      : []),
    '    <ComponentGroup Id="PulsarSeatRemotePayload">',
    ...refs,
    '    </ComponentGroup>',
    '  </Fragment>',
    '</Wix>',
    ''
  ].join('\n');
}

/**
 * Copy of installer/wix/PulsarSeat.wxs for one build: `$(env.PULSAR_SEAT_BIN)` replaced by
 * the payload folder (Tauri runs candle without the caller's environment), and the
 * ARM64EC helper switched off for `--no-arm64ec` test builds.
 */
export function resolveFragment(wxs: string, bin: string, noArm64ec = false): string {
  if (/[<>"&]/.test(bin)) throw new Error(`payload path not allowed in WiX: ${bin}`);
  const marker = '<?define PsArm64ec = "yes" ?>';
  if (!wxs.includes(marker)) throw new Error('PulsarSeat.wxs: ARM64EC marker missing');
  let out = wxs.split('$(env.PULSAR_SEAT_BIN)').join(bin.replace(/[\\/]+$/, ''));
  if (noArm64ec) out = out.replace(marker, '<?define PsArm64ec = "no" ?>');
  return out;
}

/** MSI cultures with a locale file in wix/locales/ (English is the default). */
export const MSI_LANGUAGES = ['en-US', 'tr-TR'] as const;
export type MsiLanguage = (typeof MSI_LANGUAGES)[number];

/** Point the WiX build at one culture and its locale file (one MSI per build). */
export function setLanguage(j: Record<string, any>, language: string): void {
  if (!(MSI_LANGUAGES as readonly string[]).includes(language)) throw new Error(`unsupported MSI language: ${language}`);
  j.bundle.windows.wix.language = { [language]: { localePath: `../../../installer/wix/locales/${language}.wxl` } };
}

/**
 * The bundle config: the snippet + generated fragments (drivers, and the remote payload when
 * `remoteFragmentPath` is given) + optional version override.
 */
export function buildConfig(
  snippet: unknown,
  fragmentPath: string,
  version?: string,
  remoteFragmentPath?: string,
  language?: string
): Record<string, unknown> {
  const j = stripComments(snippet) as Record<string, any>;
  const wix = j.bundle.windows.wix;
  wix.fragmentPaths = [...(wix.fragmentPaths ?? []), fragmentPath, ...(remoteFragmentPath ? [remoteFragmentPath] : [])];
  wix.componentGroupRefs = [
    ...(wix.componentGroupRefs ?? []),
    'PulsarSeatDriverPackages',
    ...(remoteFragmentPath ? ['PulsarSeatRemotePayload'] : [])
  ];
  if (version) {
    if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`version must be x.y.z: ${version}`);
    j.version = version;
  }
  if (language) setLanguage(j, language);
  return j;
}

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

if (import.meta.main) {
  const positional = process.argv.slice(2).filter((a, i, all) => !a.startsWith('--') && !all[i - 1]?.startsWith('--'));
  const out = resolve(positional[0] ?? 'target/tauri-bundle.json');
  const src = new URL('../tauri-bundle.json', import.meta.url);
  const pkgs = listDriverPackages(arg('--drivers'));
  const frag = join(dirname(out), 'PulsarSeatDrivers.wxs');
  mkdirSync(dirname(out), { recursive: true });
  await Bun.write(frag, driversFragment(pkgs));
  const remoteArg = arg('--remote');
  const remoteDir = remoteArg ? resolve(remoteArg) : null;
  if (remoteDir && !existsSync(remoteDir)) throw new Error(`--remote folder missing: ${remoteDir}`);
  const remoteFrag = join(dirname(out), 'PulsarSeatRemote.wxs');
  await Bun.write(remoteFrag, remoteFragment(remoteDir, remoteDir ? readTree(remoteDir) : null));
  const j = buildConfig(
    JSON.parse(await Bun.file(src).text()),
    frag.replace(/\\/g, '/'),
    arg('--version'),
    remoteFrag.replace(/\\/g, '/'),
    arg('--language')
  ) as any;
  const bin = arg('--bin');
  if (bin) {
    const resolved = join(dirname(out), 'PulsarSeat.wxs');
    const text = await Bun.file(new URL('../wix/PulsarSeat.wxs', import.meta.url)).text();
    await Bun.write(resolved, resolveFragment(text, resolve(bin), process.argv.includes('--no-arm64ec')));
    const wix = j.bundle.windows.wix;
    wix.fragmentPaths = wix.fragmentPaths.map((p: string) => (p.endsWith('/wix/PulsarSeat.wxs') ? resolved.replace(/\\/g, '/') : p));
  }
  await Bun.write(out, `${JSON.stringify(j, null, 2)}\n`);
  console.log(`wrote ${out}, ${frag} (${pkgs.length} driver package(s)), ${remoteFrag}${remoteDir ? '' : ' (empty)'}`);
}
