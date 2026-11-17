// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BUNDLED_ASSETS, collect, collectAssets, render } from "./js-notices.ts";

function pkg(root: string, name: string, json: object, license?: string) {
  const d = join(root, "node_modules", name);
  mkdirSync(d, { recursive: true });
  writeFileSync(join(d, "package.json"), JSON.stringify({ name, ...json }));
  if (license) writeFileSync(join(d, "LICENSE"), license);
}

describe("js-notices", () => {
  test("collects the dependency closure with licence texts", () => {
    const root = mkdtempSync(join(tmpdir(), "pseat-notices-"));
    try {
      const app = join(root, "apps", "manager");
      mkdirSync(app, { recursive: true });
      pkg(root, "a", { version: "1.0.0", license: "MIT", dependencies: { b: "^1" } }, "MIT text A");
      pkg(root, "b", { version: "1.2.0", license: "ISC" });
      const n = collect(app, ["a"]);
      expect(n.map((x) => `${x.name}@${x.version}:${x.license}`)).toEqual(["a@1.0.0:MIT", "b@1.2.0:ISC"]);
      const md = render(n);
      expect(md).toContain("MIT text A");
      expect(md).toContain("no licence file");
      pkg(root, "c", { version: "0.1.0" });
      expect(() => collect(app, ["c"])).toThrow("no licence field");
      expect(() => collect(app, ["missing"])).toThrow("not found");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("adds the bundled fonts and icons with their licence texts", () => {
    const root = mkdtempSync(join(tmpdir(), "pseat-assets-"));
    try {
      const asset = { name: "Font X (font)", version: "1.0", license: "OFL-1.1", source: "https://example.invalid/x", licenseFile: "fonts/OFL-x.txt" };
      expect(() => collectAssets(root, [asset])).toThrow("licence text of Font X");
      mkdirSync(join(root, "fonts"));
      writeFileSync(join(root, "fonts", "OFL-x.txt"), "SIL Open Font License text");
      const md = render([], collectAssets(root, [asset]));
      expect(md).toContain("# Bundled assets");
      expect(md).toContain("## Font X (font) (OFL-1.1)");
      expect(md).toContain("SIL Open Font License text");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("the manager's real asset list resolves from apps/manager", () => {
    const app = join(import.meta.dir, "..", "..", "apps", "manager");
    const assets = collectAssets(app);
    expect(assets.length).toBe(BUNDLED_ASSETS.length);
    for (const a of assets) expect(a.text.length).toBeGreaterThan(100);
    expect(assets.filter((a) => a.license === "OFL-1.1").length).toBe(3);
  });
});
