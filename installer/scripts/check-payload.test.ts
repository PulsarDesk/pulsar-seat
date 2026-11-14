// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  authenticodeHashes,
  checkArm64ec,
  checkBlocklist,
  hasHybridMetadata,
  checkDirs,
  checkSigned,
  hasCertificateTable,
  checkMachine,
  checkNotices,
  findNameViolations,
  allowedMachines,
  remoteDriverFile,
  parseArgs,
  parseBlocklist,
  peMachine,
} from "./check-payload.ts";

/** Minimal PE header: MZ, e_lfanew = 0x40, "PE\0\0", machine. */
function pe(machine: number): Uint8Array {
  const b = new Uint8Array(0x80);
  b[0] = 0x4d;
  b[1] = 0x5a;
  const v = new DataView(b.buffer);
  v.setUint32(0x3c, 0x40, true);
  v.setUint32(0x40, 0x00004550, true);
  v.setUint16(0x44, machine, true);
  return b;
}

const rules = (paths: string[], allow = false) => findNameViolations(paths, allow).map((v) => `${v.rule}:${v.path}`);

describe("peMachine", () => {
  test("reads the machine field", () => {
    expect(peMachine(pe(0xaa64))).toBe(0xaa64);
    expect(peMachine(pe(0x8664))).toBe(0x8664);
  });
  test("rejects non-PE data", () => {
    expect(peMachine(new Uint8Array(10))).toBeNull();
    const bad = pe(0xaa64);
    bad[0x40] = 0;
    expect(peMachine(bad)).toBeNull();
  });
  test("checkMachine", () => {
    expect(checkMachine("a.exe", pe(0xaa64), "arm64")).toBeNull();
    expect(checkMachine("a.exe", pe(0x8664), "arm64")?.rule).toBe("wrong_machine");
    expect(checkMachine("a.exe", new Uint8Array(4), "x64")?.rule).toBe("not_a_pe_file");
  });
});

describe("name rules", () => {
  test("clean payload passes", () => {
    expect(rules(["pulsar-seat-service.exe", "pulsar-seat-host.exe", "licenses/NOTICES.md"])).toEqual([]);
  });
  test("runtime DLLs and certificates are rejected", () => {
    expect(rules(["vcruntime140.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "WebView2Loader.dll", "x.pfx"])).toEqual([
      "microsoft_binary:vcruntime140.dll",
      "microsoft_binary:api-ms-win-crt-runtime-l1-1-0.dll",
      "microsoft_binary:WebView2Loader.dll",
      "certificate_or_key:x.pfx",
    ]);
  });
  test("drivers need --allow-drivers and a full package", () => {
    expect(rules(["drivers/kbdmou/pseat.sys"])).toEqual(["driver_not_allowed:drivers/kbdmou/pseat.sys"]);
    expect(rules(["drivers/kbdmou/pseat.sys", "drivers/kbdmou/pseat.inf"], true)).toEqual([
      "driver_without_package:drivers/kbdmou/pseat.sys",
      "driver_without_package:drivers/kbdmou/pseat.inf",
    ]);
    expect(rules(["drivers/kbdmou/pseat.sys", "drivers/kbdmou/pseat.inf", "drivers/kbdmou/pseat.cat"], true)).toEqual([]);
    expect(rules(["RTCore64.sys"], true)).toEqual(["vulnerable_driver:RTCore64.sys"]);
  });
});

describe("checkDirs + args", () => {
  test("walks a folder and checks PE machines", () => {
    const d = mkdtempSync(join(tmpdir(), "pseat-payload-"));
    try {
      writeFileSync(join(d, "pulsar-seat-service.exe"), pe(0xaa64));
      mkdirSync(join(d, "sub"));
      writeFileSync(join(d, "sub", "helper.dll"), pe(0x8664));
      writeFileSync(join(d, "readme.txt"), "x");
      const v = checkDirs([d], "arm64");
      expect(v.map((x) => x.rule)).toEqual(["wrong_machine"]);
      expect(v[0].path.endsWith("sub/helper.dll")).toBe(true);
      expect(checkDirs([d], "x64").map((x) => x.rule)).toEqual(["wrong_machine"]);
    } finally {
      rmSync(d, { recursive: true, force: true });
    }
  });
  test("parseArgs", () => {
    expect(parseArgs(["--arch", "arm64", "out"])).toEqual({
      arch: "arm64",
      allowDrivers: false,
      machineOnly: false,
      release: false,
      notices: false,
      blocklist: null,
      dirs: ["out"],
    });
    expect(parseArgs(["--arch", "x64", "--machine-only", "--blocklist", "b.xml", "o"])).toEqual({
      arch: "x64",
      allowDrivers: false,
      machineOnly: true,
      release: false,
      notices: false,
      blocklist: "b.xml",
      dirs: ["o"],
    });
    expect(parseArgs(["--arch", "x64", "--notices", "o"])?.notices).toBe(true);
    expect(parseArgs(["--arch", "x64", "--blocklist"])).toBeNull();
    expect(parseArgs(["--arch", "x86", "out"])).toBeNull();
    expect(parseArgs(["out"])).toBeNull();
  });
  test("--notices needs every licence text inside licenses/", () => {
    const full = [
      { path: "PFiles/Pulsar Seat/licenses/LICENSE.txt", size: 35149 },
      { path: "PFiles/Pulsar Seat/licenses/NOTICES.md", size: 1970 },
      { path: "PFiles/Pulsar Seat/licenses/SOURCE-OFFER.txt", size: 451 },
      { path: "PFiles/Pulsar Seat/licenses/THIRD-PARTY-NOTICES-RUST.md", size: 200000 },
      { path: "PFiles/Pulsar Seat/licenses/THIRD-PARTY-NOTICES-JS.md", size: 9000 },
    ];
    expect(checkNotices(full)).toEqual([]);
    // The locally built MSI of 2026-11-13: only the three tracked texts.
    expect(checkNotices(full.slice(0, 3)).map((v) => `${v.rule}:${v.path}`)).toEqual([
      "notice-missing:licenses/third-party-notices-rust.md",
      "notice-missing:licenses/third-party-notices-js.md",
    ]);
    const small = full.map((f) => (f.path.endsWith("JS.md") ? { ...f, size: 10 } : f));
    expect(checkNotices(small).map((v) => v.rule)).toEqual(["notice-empty"]);
    // Outside a licenses/ folder does not count.
    const elsewhere = full.map((f) => ({ ...f, path: f.path.replace("licenses/", "") }));
    expect(checkNotices(elsewhere)).toHaveLength(5);
    // Through checkDirs on a real folder.
    const d = mkdtempSync(join(tmpdir(), "pseat-notices-"));
    try {
      mkdirSync(join(d, "licenses"));
      writeFileSync(join(d, "licenses", "LICENSE.txt"), "x".repeat(2048));
      expect(checkDirs([d], "arm64", { notices: true }).filter((v) => v.rule === "notice-missing")).toHaveLength(4);
      expect(checkDirs([d], "arm64")).toEqual([]);
    } finally {
      rmSync(d, { recursive: true, force: true });
    }
  });
  test("machine-only accepts loose driver binaries (CI drivers job)", () => {
    const d = mkdtempSync(join(tmpdir(), "pseat-drv-"));
    try {
      mkdirSync(join(d, "seat-storage-filter"));
      writeFileSync(join(d, "seat-storage-filter", "seatgate.sys"), pe(0xaa64));
      expect(checkDirs([d], "arm64", { allowDrivers: true }).map((x) => x.rule)).toEqual(["driver_without_package"]);
      expect(checkDirs([d], "arm64", { machineOnly: true })).toEqual([]);
      expect(checkDirs([d], "x64", { machineOnly: true }).map((x) => x.rule)).toEqual(["wrong_machine"]);
    } finally {
      rmSync(d, { recursive: true, force: true });
    }
  });
});

describe("apihelper folders", () => {
  test("each folder carries its own machine", () => {
    expect(checkMachine("p", pe(0x014c), "arm64", "apihelper/x86/pulsar-seat-api.dll")).toBeNull();
    expect(checkMachine("p", pe(0x8664), "arm64", "apihelper/x64/pulsar-seat-api.dll")).toBeNull();
    expect(checkMachine("p", pe(0xaa64), "x64", "apihelper/arm64/pulsar-seat-api.dll")).toBeNull();
    expect(checkMachine("p", pe(0x8664), "arm64", "apihelper/arm64ec/pulsar-seat-api.dll")).toBeNull();
    expect(checkMachine("p", pe(0xa641), "arm64", "apihelper/arm64ec/pulsar-seat-api.dll")).toBeNull();
    expect(checkMachine("p", pe(0x8664), "arm64", "apihelper/x86/pulsar-seat-api.dll")?.rule).toBe("wrong_machine");
    expect(checkMachine("p", pe(0x014c), "arm64", "apihelper/mips/x.dll")?.rule).toBe("wrong_machine");
    // Outside apihelper/ the target arch rule is unchanged.
    expect(checkMachine("p", pe(0x014c), "arm64", "x86/pulsar-seat-api.dll")?.rule).toBe("wrong_machine");
  });
});

/**
 * PE32+ with one section (.rdata, RVA 0x1000, raw 0x200) holding a load configuration of
 * `lcSize` bytes; `chpe` goes into CHPEMetadataPointer (offset 0xC8).
 */
function pe64lc(machine: number, chpe: bigint, lcSize = 0x140, withLc = true): Uint8Array {
  const b = new Uint8Array(0x400);
  const v = new DataView(b.buffer);
  b[0] = 0x4d;
  b[1] = 0x5a;
  v.setUint32(0x3c, 0x40, true);
  v.setUint32(0x40, 0x00004550, true);
  v.setUint16(0x44, machine, true);
  v.setUint16(0x46, 1, true); // sections
  v.setUint16(0x54, 240, true); // SizeOfOptionalHeader (PE32+, 16 dirs)
  const opt = 0x58;
  v.setUint16(opt, 0x20b, true);
  v.setUint32(opt + 108, 16, true);
  if (withLc) {
    v.setUint32(opt + 112 + 10 * 8, 0x1000, true);
    v.setUint32(opt + 112 + 10 * 8 + 4, lcSize, true);
  }
  const sec = opt + 240;
  v.setUint32(sec + 8, 0x200, true);
  v.setUint32(sec + 12, 0x1000, true);
  v.setUint32(sec + 16, 0x200, true);
  v.setUint32(sec + 20, 0x200, true);
  v.setUint32(0x200, lcSize, true);
  v.setBigUint64(0x200 + 0xc8, chpe, true);
  return b;
}

describe("arm64ec helper", () => {
  test("needs hybrid metadata, not just the x64 machine", () => {
    expect(hasHybridMetadata(pe64lc(0x8664, 0x18010da90n))).toBe(true);
    expect(hasHybridMetadata(pe64lc(0x8664, 0n))).toBe(false);
    expect(hasHybridMetadata(pe64lc(0x8664, 0x1n, 0x90))).toBe(false); // old, short load config
    expect(hasHybridMetadata(pe64lc(0x8664, 0x1n, 0x140, false))).toBe(false);
    expect(hasHybridMetadata(pe(0x014c))).toBeNull();
    expect(checkArm64ec("a", pe64lc(0x8664, 0n))?.rule).toBe("not_arm64ec");
    expect(checkArm64ec("a", pe64lc(0x8664, 5n))).toBeNull();
  });

  test("checkDirs applies it to apihelper/arm64ec only", () => {
    const d = mkdtempSync(join(tmpdir(), "pulsar-seat-ec-"));
    try {
      mkdirSync(join(d, "apihelper", "arm64ec"), { recursive: true });
      mkdirSync(join(d, "apihelper", "x64"), { recursive: true });
      writeFileSync(join(d, "apihelper", "x64", "pulsar-seat-api.dll"), pe64lc(0x8664, 0n));
      writeFileSync(join(d, "apihelper", "arm64ec", "pulsar-seat-api.dll"), pe64lc(0x8664, 0n));
      const v = checkDirs([d], "arm64", { machineOnly: true }).map((x) => x.rule);
      expect(v).toEqual(["not_arm64ec"]);
      writeFileSync(join(d, "apihelper", "arm64ec", "pulsar-seat-api.dll"), pe64lc(0x8664, 0x1000n));
      expect(checkDirs([d], "arm64", { machineOnly: true })).toEqual([]);
    } finally {
      rmSync(d, { recursive: true, force: true });
    }
  });

  test("the real ARM64EC build passes when present", () => {
    const f = join(import.meta.dir, "..", "..", "target", "wf", "apihelper", "arm64ec-pc-windows-msvc", "release", "pulsar_seat_api.dll");
    if (!existsSync(f)) return;
    expect(hasHybridMetadata(new Uint8Array(readFileSync(f)))).toBe(true);
  });
});

describe("vulnerable driver block rules", () => {
  /** PE32+ image with an optional header, a fake checksum and a certificate table. */
  function pe64(checksum: number, cert: number[]): Uint8Array {
    const body = 0x200;
    const b = new Uint8Array(body + cert.length);
    b.set(pe(0x8664).subarray(0, 0x46));
    const v = new DataView(b.buffer);
    const opt = 0x40 + 24;
    v.setUint16(opt, 0x20b, true);
    v.setUint32(opt + 64, checksum, true);
    v.setUint32(opt + 108, 16, true); // NumberOfRvaAndSizes
    const certEntry = opt + 112 + 4 * 8;
    v.setUint32(certEntry, cert.length ? body : 0, true);
    v.setUint32(certEntry + 4, cert.length, true);
    for (let i = 0x100; i < body; i++) b[i] = i & 0xff;
    b.set(cert, body);
    return b;
  }
  test("Authenticode hash ignores checksum and signature", () => {
    const a = authenticodeHashes(pe64(1, []));
    const b = authenticodeHashes(pe64(0xdeadbeef, [9, 9, 9, 9, 9, 9, 9, 9]));
    expect(a).not.toBeNull();
    expect(a?.sha256).toBe(b?.sha256 ?? "");
    expect(a?.sha1).toBe(b?.sha1 ?? "");
    expect(authenticodeHashes(new Uint8Array(8))).toBeNull();
  });
  test("blocklist hash and file name rules", () => {
    const img = pe64(0, []);
    const h = authenticodeHashes(img);
    const xml = `<SiPolicy><FileRules>
      <Deny ID="ID_DENY_A" FriendlyName="a" Hash="${h?.sha256.toLowerCase()}" />
      <Deny ID="ID_DENY_B" FriendlyName="b" FileName="evil.sys" MinimumFileVersion="0.0.0.0" />
      <Allow ID="ID_ALLOW_C" FriendlyName="c" Hash="00" />
    </FileRules></SiPolicy>`;
    const bl = parseBlocklist(xml);
    expect(bl.hashes.size).toBe(1);
    expect(bl.names.has("evil.sys")).toBe(true);
    // Renamed copy of a denied driver is still caught by its hash.
    expect(checkBlocklist("drivers/x/renamed.sys", img, bl)?.detail).toBe("blocklist hash");
    expect(checkBlocklist("drivers/x/EVIL.sys", pe64(0, [1]), bl)?.detail).toBe("blocklist file name");
    const other = pe64(0, []);
    other[0x150] ^= 0xff;
    expect(checkBlocklist("drivers/x/ok.sys", other, bl)).toBeNull();
  });

  test("release gate: the LogonUI DLL must carry a signature", () => {
    const signed = pe64(0, [8, 0, 0, 0, 0, 2, 2, 0, 1, 2, 3, 4]);
    const unsigned = pe64(0, []);
    expect(hasCertificateTable(signed)).toBe(true);
    expect(hasCertificateTable(unsigned)).toBe(false);
    expect(hasCertificateTable(new Uint8Array(8))).toBeNull();
    expect(checkSigned("x", signed)).toBeNull();
    expect(checkSigned("x", unsigned)?.rule).toBe("unsigned_system_dll");
    // Table that points past the end of the file is not a signature.
    const cut = signed.subarray(0, signed.length - 4);
    expect(hasCertificateTable(cut)).toBe(false);

    const d = mkdtempSync(join(tmpdir(), "pseat-sig-"));
    try {
      writeFileSync(join(d, "pulsar-seat-credprov.dll"), unsigned);
      writeFileSync(join(d, "pulsar-seat-service.exe"), unsigned);
      // Not a release: unsigned test builds pass.
      expect(checkDirs([d], "x64").map((x) => x.rule)).toEqual([]);
      // Release: only the LogonUI DLL is required to be signed here.
      const v = checkDirs([d], "x64", { release: true });
      expect(v.map((x) => x.rule)).toEqual(["unsigned_system_dll"]);
      expect(v[0].path.endsWith("pulsar-seat-credprov.dll")).toBe(true);
      writeFileSync(join(d, "pulsar-seat-credprov.dll"), signed);
      expect(checkDirs([d], "x64", { release: true })).toEqual([]);
      expect(parseArgs(["--arch", "x64", "--release", d])?.release).toBe(true);
    } finally {
      rmSync(d, { recursive: true, force: true });
    }
  });
});

describe("bundle-config", () => {
  test("strips $comment keys and keeps the rest", async () => {
    const { stripComments } = await import("./bundle-config.ts");
    expect(stripComments({ $comment: "x", a: [{ $comment: "y", b: 1 }] })).toEqual({ a: [{ b: 1 }] });
    const real = stripComments(JSON.parse(await Bun.file(new URL("../tauri-bundle.json", import.meta.url)).text())) as {
      productName: string;
      bundle: { windows: { wix: { componentRefs: string[] } } };
    };
    expect(real.productName).toBe("Pulsar Seat");
    expect(JSON.stringify(real)).not.toContain("$comment");
    // Every component the snippet references exists in the WiX fragment.
    const wxs = await Bun.file(new URL("../wix/PulsarSeat.wxs", import.meta.url)).text();
    for (const id of real.bundle.windows.wix.componentRefs) expect(wxs).toContain(`<Component Id="${id}"`);
  });
});

// Remote seat kind (merge step 10): rules ported from Pulsar Split's check-payload tests.
describe("remote kind payload", () => {
  test("pinned remote payload passes without --allow-drivers", () => {
    expect(
      rules([
        "pulsar-seat-service.exe",
        "remote/drivers/vdd/VirtualDisplayDriver/MttVDD.inf",
        "remote/drivers/vdd/VirtualDisplayDriver/mttvdd.cat",
        "remote/drivers/vdd/VirtualDisplayDriver/MttVDD.dll",
        "remote/drivers/usbip-win2/USBip-0.9.8.1-x64.exe",
        "remote/stream-server/sunshine.exe",
        "remote/stream-server/assets/shaders/directx/convert.hlsl",
        "remote/drivers.json",
        "licenses/VirtualDisplayDriver-MIT.txt",
      ]),
    ).toEqual([]);
  });

  test("a pinned INF still needs its catalog; drivers elsewhere still need --allow-drivers", () => {
    expect(rules(["remote/drivers/vdd/MttVDD.inf", "remote/drivers/vdd/MttVDD.dll"])).toEqual([
      "driver_without_package:remote/drivers/vdd/MttVDD.inf",
    ]);
    expect(rules(["drivers/vdd/MttVDD.inf", "drivers/vdd/mttvdd.cat"])).toEqual(["driver_not_allowed:drivers/vdd/MttVDD.inf"]);
    expect(rules(["remote/x.inf"])).toEqual(["driver_not_allowed:remote/x.inf"]);
  });

  test("more Microsoft system files are rejected", () => {
    const r = rules(["RdpIdd.dll", "rfxvmt.dll", "AcRes.dll", "d3dcompiler_47.dll", "runtimes/win-x64/native/WebView2Loader.dll"]);
    expect(r.length).toBe(5);
    expect(r.every((x) => x.startsWith("microsoft_binary:"))).toBe(true);
  });

  test("web UI assets are rejected, backslashes normalised", () => {
    expect(rules(["remote/stream-server/assets/web/index.html"])).toEqual(["web_ui_asset:remote/stream-server/assets/web/index.html"]);
    expect(rules(["remote/stream-server/assets/web/assets/app.js"])).toEqual([
      "web_ui_asset:remote/stream-server/assets/web/assets/app.js",
    ]);
    expect(rules(["docs/readme.htm"])).toEqual(["web_ui_asset:docs/readme.htm"]);
    expect(rules(["remote\\stream-server\\www\\a.js"])).toEqual(["web_ui_asset:remote/stream-server/www/a.js"]);
  });

  test("vulnerable drivers fail, pinned kernel drivers warn", () => {
    const v = findNameViolations(["x/DuoDriver.sys", "remote/drivers/usbip/usbip2_ude.sys", "remote/drivers/usbip/u.inf", "remote/drivers/usbip/u.cat"], false);
    expect(v).toEqual([
      { path: "x/DuoDriver.sys", rule: "vulnerable_driver", severity: "error" },
      { path: "remote/drivers/usbip/usbip2_ude.sys", rule: "kernel_driver_review", severity: "warning" },
    ]);
  });

  test("the shim module ships only in shim/", () => {
    expect(rules(["shim/x64/PulsarSplitShim.dll", "shim/x86/PulsarSplitShim.dll"])).toEqual([]);
    expect(rules(["PFiles/Pulsar Seat/PulsarSplitShim.dll"])).toEqual(["shim_outside_folder:PFiles/Pulsar Seat/PulsarSplitShim.dll"]);
  });

  test("remoteDriverFile", () => {
    expect(remoteDriverFile("PFiles/Pulsar Seat/remote/drivers/vdd/a/b.dll")).toEqual({ id: "vdd", rest: "a/b.dll" });
    expect(remoteDriverFile("remote/drivers.json")).toBeNull();
    expect(remoteDriverFile("drivers/vdd/a.inf")).toBeNull();
  });

  test("machines: shim x64 package only, installer stubs may be x86", () => {
    expect(allowedMachines("shim/x64/PulsarSplitShim.dll", "x64")).toEqual([0x8664]);
    expect(allowedMachines("shim/x86/PulsarSplitShim.dll", "x64")).toEqual([0x014c]);
    expect(typeof allowedMachines("shim/x64/PulsarSplitShim.dll", "arm64")).toBe("string");
    expect(typeof allowedMachines("shim/arm64/a.dll", "x64")).toBe("string");
    expect(allowedMachines("remote/drivers/usbip-win2/USBip-arm64.exe", "arm64")).toEqual([0xaa64, 0x014c]);
    expect(allowedMachines("remote/drivers/vdd/VirtualDisplayDriver/MttVDD.dll", "arm64")).toEqual([0xaa64]);
    expect(allowedMachines("remote/stream-server/sunshine.exe", "x64")).toEqual([0x8664]);
    expect(checkMachine("shim/x86/l.exe", pe(0x014c), "x64")).toBeNull();
    expect(checkMachine("shim/x86/l.exe", pe(0x8664), "x64")?.rule).toBe("wrong_machine");
    expect(checkMachine("shim/x64/l.exe", pe(0x8664), "arm64")?.rule).toBe("wrong_machine");
    expect(checkMachine("remote/stream-server/tools/x.exe", pe(0x014c), "arm64")?.rule).toBe("wrong_machine");
  });

  test("directory walk over a remote payload", () => {
    const root = mkdtempSync(join(tmpdir(), "ps-remote-"));
    try {
      mkdirSync(join(root, "remote", "stream-server", "web"), { recursive: true });
      mkdirSync(join(root, "remote", "drivers", "u"), { recursive: true });
      writeFileSync(join(root, "remote", "stream-server", "web", "x.js"), "");
      writeFileSync(join(root, "remote", "stream-server", "s.exe"), pe(0xaa64));
      writeFileSync(join(root, "remote", "drivers", "u", "setup.exe"), pe(0x014c));
      writeFileSync(join(root, "msvcp140.dll"), pe(0xaa64));
      const v = checkDirs([root], "arm64").map((x) => x.rule).sort();
      expect(v).toEqual(["microsoft_binary", "web_ui_asset"]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
