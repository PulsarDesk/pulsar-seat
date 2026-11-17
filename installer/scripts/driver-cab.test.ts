// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { DRIVERS, cabStem, decodeInf, listCab, makeDdf, parseInf, planPackage, type Listing } from "./driver-cab";

const INF = `; comment
[Version]
Signature   = "$WINDOWS NT$"
DriverVer   = 11/16/2026,0.1.0.0 ; stamped
CatalogFile = seat-wfp.cat

[SourceDisksFiles]
seatwfp.sys = 1,,

[SourceDisksFiles.arm64]
"Extra.DLL" = 1

[Strings]
Instance1.Altitude = "UNALLOCATED"
`;

function listing(over: Partial<Listing> = {}): Listing {
  return {
    packageDir: "P",
    pdbDir: "D",
    packageFiles: ["seat-wfp.inf", "seatwfp.sys", "extra.dll", "seat-wfp.cat"],
    pdbFiles: ["seatwfp.pdb", "Extra.pdb", "vc145.pdb"],
    ...over,
  };
}

const arm = () => 0xaa64;

describe("INF parsing", () => {
  test("version, catalog, source files and altitude", () => {
    const f = parseInf(INF);
    expect(f.driverVer).toBe("11/16/2026,0.1.0.0");
    expect(f.catalogFile).toBe("seat-wfp.cat");
    expect(f.sourceFiles).toEqual(["seatwfp.sys", "extra.dll"]);
    expect(f.altitude).toBe("UNALLOCATED");
    expect(parseInf("[Version]\nDriverVer=1").catalogFile).toBeNull();
  });

  test("UTF-16LE and UTF-8 BOM decoding", () => {
    const utf16 = new Uint8Array([0xff, 0xfe, ...[..."[A]"].flatMap((c) => [c.charCodeAt(0), 0])]);
    expect(decodeInf(utf16)).toBe("[A]");
    expect(decodeInf(new Uint8Array([0xef, 0xbb, 0xbf, 0x41]))).toBe("A");
    expect(decodeInf(new TextEncoder().encode("plain"))).toBe("plain");
  });

  test("every repo INF parses with DriverVer and CatalogFile", () => {
    const repo = resolve(import.meta.dir, "..", "..");
    for (const d of DRIVERS) {
      const path = d === "split-pad" ? join(repo, "drivers", d, "split_pad.inx") : join(repo, "drivers", d, `${d}.inf`);
      const f = parseInf(decodeInf(readFileSync(path)));
      expect(f.driverVer, d).not.toBeNull();
      expect(f.catalogFile, d).not.toBeNull();
      expect(f.sourceFiles.length, d).toBeGreaterThan(0);
    }
  });
});

describe("package plan", () => {
  test("a good package: INF + binaries + PDBs, no catalog, folders per platform", () => {
    const p = planPackage("seat-wfp", "ARM64", listing(), INF.replace("UNALLOCATED", "409500"), arm);
    expect(p.problems).toEqual([]);
    expect(p.files.map((f) => f.dest).sort()).toEqual(
      ["seat-wfp\\ARM64\\Extra.pdb", "seat-wfp\\ARM64\\extra.dll", "seat-wfp\\ARM64\\seat-wfp.inf", "seat-wfp\\ARM64\\seatwfp.pdb", "seat-wfp\\ARM64\\seatwfp.sys"].sort(),
    );
    expect(p.files.some((f) => f.dest.endsWith(".cat"))).toBe(false);
    expect(p.files.some((f) => f.dest.endsWith("vc145.pdb"))).toBe(false);
  });

  test("placeholder altitude refused unless allowed", () => {
    expect(planPackage("seat-wfp", "ARM64", listing(), INF, arm).problems.join()).toContain("placeholder");
    expect(planPackage("seat-wfp", "ARM64", listing(), INF, arm, true).problems).toEqual([]);
  });

  test("wrong machine, missing PDB, missing source file, INF count", () => {
    const x = planPackage("seat-wfp", "x64", listing(), INF, arm, true).problems.join("\n");
    expect(x).toContain("machine 0xaa64, want 0x8664");
    const noPdb = planPackage("seat-wfp", "ARM64", listing({ pdbFiles: [] }), INF, arm, true).problems.join("\n");
    expect(noPdb).toContain("no seatwfp.pdb");
    const noFile = planPackage("seat-wfp", "ARM64", listing({ packageFiles: ["seat-wfp.inf", "seatwfp.sys"] }), INF, arm, true).problems.join("\n");
    expect(noFile).toContain("extra.dll");
    const twoInf = planPackage("seat-wfp", "ARM64", listing({ packageFiles: ["a.inf", "b.inf", "seatwfp.sys", "extra.dll"] }), INF, arm, true);
    expect(twoInf.problems.join()).toContain("2 INF files");
    expect(planPackage("seat-wfp", "ARM64", listing({ packageFiles: [] }), null, arm).problems.join()).toContain("no .sys / .dll");
    expect(planPackage("x", "ARM64", listing(), "[Version]\n", arm, true).problems.join()).toContain("no DriverVer");
  });
});

describe("DDF and cabinet", () => {
  test("CAB name: both platforms or one", () => {
    expect(cabStem("seat-wfp", ["ARM64", "x64"])).toBe("seat-wfp");
    expect(cabStem("seat-wfp", ["x64", "ARM64"])).toBe("seat-wfp");
    expect(cabStem("seat-wfp", ["ARM64"])).toBe("seat-wfp-ARM64");
  });

  test("DDF: explicit options, one DestinationDir per folder, quoted sources", () => {
    const d = makeDdf("x.cab", "C:\\o ut", [
      { src: "C:\\a b\\x.inf", dest: "x\\ARM64\\x.inf" },
      { src: "C:\\a b\\x.sys", dest: "x\\ARM64\\x.sys" },
      { src: "C:\\c\\x.inf", dest: "x\\x64\\x.inf" },
    ]);
    expect(d).toContain(".OPTION EXPLICIT");
    expect(d).toContain(".Set CabinetNameTemplate=x.cab");
    expect(d).toContain('.Set DiskDirectoryTemplate="C:\\o ut"');
    expect(d.match(/\.Set DestinationDir=/g)?.length).toBe(2);
    expect(d).toContain('.Set DestinationDir=x\\x64\r\n"C:\\c\\x.inf"');
    expect(d.endsWith("\r\n")).toBe(true);
  });

  test("listCab reads the CFFILE names with their folders", () => {
    // CFHEADER (36 bytes) with coffFiles = 36, cFiles = 2, then two CFFILE records.
    const rec = (name: string, utf = false) => {
      const n = new TextEncoder().encode(name);
      const r = new Uint8Array(16 + n.length + 1);
      new DataView(r.buffer).setUint16(14, utf ? 0x80 : 0, true);
      r.set(n, 16);
      return r;
    };
    const h = new Uint8Array(36);
    h.set([0x4d, 0x53, 0x43, 0x46]);
    const dv = new DataView(h.buffer);
    dv.setUint32(16, 36, true);
    dv.setUint16(28, 2, true);
    const a = rec("drv\\ARM64\\a.inf");
    const b = rec("drv\\x64\\ü.sys", true);
    const cab = new Uint8Array([...h, ...a, ...b]);
    expect(listCab(cab)).toEqual(["drv\\ARM64\\a.inf", "drv\\x64\\ü.sys"]);
    expect(() => listCab(new Uint8Array(40))).toThrow("MSCF");
    expect(() => listCab(cab.subarray(0, 40))).toThrow("truncated");
  });

  test.skipIf(process.platform !== "win32")("makecab round trip keeps the folder layout", () => {
    const dir = mkdtempSync(join(tmpdir(), "driver-cab-"));
    try {
      const src = join(dir, "src");
      mkdirSync(src);
      writeFileSync(join(src, "a.inf"), "[Version]\r\n");
      writeFileSync(join(src, "a.sys"), "x".repeat(100));
      const out = join(dir, "out");
      mkdirSync(out);
      const ddf = join(out, "a.ddf");
      writeFileSync(
        ddf,
        makeDdf("a.cab", out, [
          { src: join(src, "a.inf"), dest: "a\\ARM64\\a.inf" },
          { src: join(src, "a.sys"), dest: "a\\x64\\a.sys" },
        ]),
      );
      const r = Bun.spawnSync(["makecab.exe", "/F", ddf], { stdout: "pipe", stderr: "pipe" });
      expect(r.exitCode).toBe(0);
      expect(listCab(readFileSync(join(out, "a.cab")))).toEqual(["a\\ARM64\\a.inf", "a\\x64\\a.sys"]);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
