// SPDX-License-Identifier: GPL-3.0-only
//
// Writes the licence notices of the JavaScript packages bundled into the manager's
// frontend: the app's `dependencies` plus the runtime parts of its build tooling that end
// up in the bundle (svelte, @sveltejs/kit client runtime), with everything they depend on.
// Listing a little more than the bundle holds is harmless; missing one is not.
//
// Fails (exit 1) when a package in the closure cannot be found or has no licence field.
//
// Usage: bun installer/scripts/js-notices.ts <app dir> <out.md>

import { existsSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

/** Build-time packages whose runtime is bundled into the frontend output. */
export const BUNDLED_RUNTIME = ["svelte", "@sveltejs/kit"];

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

export function render(notices: PackageNotice[]): string {
  const out = ["# Third-party notices (JavaScript packages in the manager frontend)", ""];
  for (const n of notices) {
    out.push(`## ${n.name} ${n.version} (${n.license})`, "");
    out.push(n.text ? ["```text", n.text.trimEnd(), "```"].join("\n") : `Licence: ${n.license} (no licence file in the package).`);
    out.push("");
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
    writeFileSync(out, render(notices));
    console.log(`wrote ${out} (${notices.length} packages)`);
  } catch (e) {
    console.error(String(e));
    process.exit(1);
  }
}
