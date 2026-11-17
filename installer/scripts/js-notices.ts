// SPDX-License-Identifier: GPL-3.0-only
//
// Writes the licence notices of the JavaScript packages bundled into the manager's
// frontend: the app's `dependencies` plus the runtime parts of its build tooling that end
// up in the bundle (svelte, @sveltejs/kit client runtime), with everything they depend on.
// Listing a little more than the bundle holds is harmless; missing one is not.
// It also lists the third-party assets the frontend bundles as files rather than packages
// (fonts, icon geometry), with the licence texts kept next to them in the app's sources.
//
// Fails (exit 1) when a package in the closure cannot be found or has no licence field, or
// when the licence text of a bundled asset is missing.
//
// Usage: bun installer/scripts/js-notices.ts <app dir> <out.md>

import { existsSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

/** Build-time packages whose runtime is bundled into the frontend output. */
export const BUNDLED_RUNTIME = ["svelte", "@sveltejs/kit"];

/** A third-party asset bundled as files (not an npm package). `licenseFile` is relative to the app dir. */
export interface BundledAsset {
  name: string;
  version: string;
  license: string;
  source: string;
  licenseFile: string;
}

/** Fonts and icons of the manager's design system (apps/manager/src/lib/ui/). */
export const BUNDLED_ASSETS: BundledAsset[] = [
  {
    name: "Space Grotesk (font)",
    version: "@fontsource-variable/space-grotesk 5.2.5, woff2 latin + latin-ext",
    license: "OFL-1.1",
    source: "https://fontsource.org/fonts/space-grotesk",
    licenseFile: "src/lib/ui/fonts/OFL-space-grotesk.txt",
  },
  {
    name: "Hanken Grotesk (font)",
    version: "@fontsource-variable/hanken-grotesk 5.2.5, woff2 latin + latin-ext",
    license: "OFL-1.1",
    source: "https://fontsource.org/fonts/hanken-grotesk",
    licenseFile: "src/lib/ui/fonts/OFL-hanken-grotesk.txt",
  },
  {
    name: "JetBrains Mono (font)",
    version: "@fontsource-variable/jetbrains-mono 5.2.5, woff2 latin + latin-ext",
    license: "OFL-1.1",
    source: "https://fontsource.org/fonts/jetbrains-mono",
    licenseFile: "src/lib/ui/fonts/OFL-jetbrains-mono.txt",
  },
  {
    name: "Lucide (icon geometry)",
    version: "path data of about 75 icons in src/lib/ui/icons.ts",
    license: "ISC (portions from Feather: MIT)",
    source: "https://lucide.dev",
    licenseFile: "src/lib/ui/LUCIDE-LICENSE.txt",
  },
];

export interface AssetNotice extends BundledAsset {
  text: string;
}

/** Licence texts of `assets` read from `appDir`. Throws when a licence file is missing. */
export function collectAssets(appDir: string, assets: BundledAsset[] = BUNDLED_ASSETS): AssetNotice[] {
  return assets.map((a) => {
    const p = join(appDir, a.licenseFile);
    if (!existsSync(p)) throw new Error(`licence text of ${a.name} missing: ${a.licenseFile}`);
    return { ...a, text: readFileSync(p, "utf8") };
  });
}

export interface PackageNotice {
  name: string;
  version: string;
  license: string;
  text: string | null;
}

interface PackageJson {
  name?: string;
  version?: string;
  license?: string | { type?: string };
  dependencies?: Record<string, string>;
}

/** Resolve `name` from `from` the Node way (walk up node_modules folders). */
export function resolvePackage(name: string, from: string): string | null {
  let dir = resolve(from);
  for (;;) {
    const p = join(dir, "node_modules", name);
    // Real path: bun / pnpm style stores keep a package's own deps next to its real folder.
    if (existsSync(join(p, "package.json"))) return realpathSync(p);
    const up = dirname(dir);
    if (up === dir) return null;
    dir = up;
  }
}

function licenseText(dir: string): string | null {
  const f = readdirSync(dir).find((n) => /^(licen[cs]e|copying|notice)(\.|$)/i.test(n));
  return f ? readFileSync(join(dir, f), "utf8") : null;
}

/** Dependency closure of `roots` resolved from `appDir`. Throws on a missing package. */
export function collect(appDir: string, roots: string[]): PackageNotice[] {
  const seen = new Map<string, PackageNotice>();
  const queue: { name: string; from: string }[] = roots.map((name) => ({ name, from: appDir }));
  while (queue.length > 0) {
    const { name, from } = queue.shift() as { name: string; from: string };
    const dir = resolvePackage(name, from);
    if (!dir) throw new Error(`package ${name} not found from ${from}`);
    const pj = JSON.parse(readFileSync(join(dir, "package.json"), "utf8")) as PackageJson;
    const key = `${pj.name ?? name}@${pj.version ?? "?"}`;
    if (seen.has(key)) continue;
    const lic = typeof pj.license === "string" ? pj.license : pj.license?.type;
    if (!lic) throw new Error(`${key} has no licence field`);
    seen.set(key, { name: pj.name ?? name, version: pj.version ?? "?", license: lic, text: licenseText(dir) });
    for (const d of Object.keys(pj.dependencies ?? {})) queue.push({ name: d, from: dir });
  }
  return [...seen.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

export function render(notices: PackageNotice[], assets: AssetNotice[] = []): string {
  const out = ["# Third-party notices (JavaScript packages in the manager frontend)", ""];
  for (const n of notices) {
    out.push(`## ${n.name} ${n.version} (${n.license})`, "");
    out.push(n.text ? ["```text", n.text.trimEnd(), "```"].join("\n") : `Licence: ${n.license} (no licence file in the package).`);
    out.push("");
  }
  if (assets.length > 0) {
    out.push("# Bundled assets (fonts and icons in the manager frontend)", "");
    for (const a of assets) {
      out.push(`## ${a.name} (${a.license})`, "", `${a.version}. Source: ${a.source}`, "");
      out.push(["```text", a.text.trimEnd(), "```"].join("\n"), "");
    }
  }
  return `${out.join("\n")}\n`;
}

if (import.meta.main) {
  const [appDir, out] = process.argv.slice(2);
  if (!appDir || !out) {
    console.error("usage: bun installer/scripts/js-notices.ts <app dir> <out.md>");
    process.exit(2);
  }
  try {
    const pj = JSON.parse(readFileSync(join(appDir, "package.json"), "utf8")) as PackageJson;
    const roots = [...Object.keys(pj.dependencies ?? {}), ...BUNDLED_RUNTIME];
    const notices = collect(appDir, roots);
    const assets = collectAssets(appDir);
    writeFileSync(out, render(notices, assets));
    console.log(`wrote ${out} (${notices.length} packages, ${assets.length} bundled assets)`);
  } catch (e) {
    console.error(String(e));
    process.exit(1);
  }
}
