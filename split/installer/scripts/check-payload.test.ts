// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { checkDirs, findViolations } from "./check-payload.ts";

const rules = (paths: string[]) => findViolations(paths).map((v) => `${v.rule}:${v.path}`);

describe("check-payload", () => {
  test("clean payload passes", () => {
    expect(
      findViolations([
        "pulsar-split-service.exe",
        "drivers/vdd/MttVDD.inf",
        "drivers/vdd/mttvdd.cat",
        "drivers/vdd/MttVDD.dll",
        "drivers/usbip-win2/USBip-0.9.8.1-x64.exe",
        "stream-server/sunshine.exe",
        "stream-server/assets/shaders/directx/convert.hlsl",
        "licenses/vdd-MIT.txt",
        "drivers.json",
      ]),
    ).toEqual([]);
  });

  test("Microsoft runtime and system DLLs are rejected", () => {
    const r = rules([
      "vcruntime140.dll",
      "vcruntime140_1.dll",
      "msvcp140.dll",
      "ucrtbase.dll",
      "api-ms-win-crt-runtime-l1-1-0.dll",
      "runtimes/win-x64/native/WebView2Loader.dll",
      "RdpIdd.dll",
      "rfxvmt.dll",
      "AcRes.dll",
    ]);
    expect(r.length).toBe(9);
    expect(r.every((x) => x.startsWith("microsoft_binary:"))).toBe(true);
  });

  test("certificates and keys are rejected", () => {
    expect(rules(["credentials/cacert.pem", "credentials/cakey.pem", "a/usbip.pfx", "x.cer", "y.crt"])).toEqual([
      "certificate_or_key:credentials/cacert.pem",
      "certificate_or_key:credentials/cakey.pem",
      "certificate_or_key:a/usbip.pfx",
      "certificate_or_key:x.cer",
      "certificate_or_key:y.crt",
    ]);
  });

  test("web UI assets are rejected", () => {
    expect(rules(["stream-server/assets/web/index.html"]).sort()).toEqual([
      "web_ui_asset:stream-server/assets/web/index.html",
    ]);
    expect(rules(["stream-server/assets/web/assets/app.js"])).toEqual([
      "web_ui_asset:stream-server/assets/web/assets/app.js",
    ]);
    expect(rules(["docs/readme.htm"])).toEqual(["web_ui_asset:docs/readme.htm"]);
  });

  test("vulnerable drivers fail, other kernel drivers warn", () => {
    const v = findViolations(["x/RTCore64.sys", "y/usbip2_ude.sys"]);
    expect(v).toEqual([
      { path: "x/RTCore64.sys", rule: "vulnerable_driver", severity: "error" },
      { path: "y/usbip2_ude.sys", rule: "kernel_driver_review", severity: "warning" },
    ]);
  });

  test("backslash paths are normalised", () => {
    expect(rules(["stream-server\\assets\\web\\a.js"])).toEqual(["web_ui_asset:stream-server/assets/web/a.js"]);
  });

  test("directory walk finds nested files", () => {
    const root = mkdtempSync(join(tmpdir(), "ps-payload-"));
    try {
      mkdirSync(join(root, "a", "web"), { recursive: true });
      writeFileSync(join(root, "a", "web", "x.js"), "");
      writeFileSync(join(root, "ok.exe"), "");
      writeFileSync(join(root, "msvcp140.dll"), "");
      const v = checkDirs([root]).map((x) => x.rule).sort();
      expect(v).toEqual(["microsoft_binary", "web_ui_asset"]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
