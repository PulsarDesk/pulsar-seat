// SPDX-License-Identifier: GPL-3.0-only
// Pure helpers only: no network, no extraction.
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  type DriverPackage,
  type StreamManifest,
  decodeInf,
  exeFileName,
  globMatch,
  infProblems,
  isDroppedPath,
  isWebUiPath,
  localServerDist,
  packagesFor,
  type ServerDist,
  serverDist,
  sha256Hex,
  validateDrivers,
  validateLocalBuild,
  validateStream,
} from "./fetch-deps.ts";

describe("fetch-deps local Pulsar build (--stream-local)", () => {
  const m: StreamManifest = JSON.parse(readFileSync(join(import.meta.dir, "..", "remote", "stream-server.json"), "utf8"));
  const sha = "b".repeat(64);
  const local: ServerDist = {
    flavour: "pulsar_fork",
    version: `${m.version}-pulsar.1`,
    file: "sunshine.exe",
    sha256: sha,
    source_url: `https://github.com/SunshineStream/Sunshine/tree/${m.version} + stream-server/sunshine-patches`,
    licence: "GPL-3.0-only",
  };

  test("accepts the output of build-sunshine.ps1", () => {
    expect(validateLocalBuild(m, local, sha)).toEqual([]);
    // The service resolves `file` inside remote\stream-server\ and refuses a path (gaming PC
    // round 4: "stream-server\\sunshine.exe" failed every remote seat with bad_manifest).
    expect(localServerDist(local, "stream-server/sunshine.exe")).toEqual({ ...local, file: "sunshine.exe" });
    expect(exeFileName("stream-server\\sunshine.exe")).toBe("sunshine.exe");
  });

  test("refuses a changed exe, another version or flavour", () => {
    expect(validateLocalBuild(m, local, "c".repeat(64)).join()).toContain("sha256");
    expect(validateLocalBuild(m, { ...local, version: "v1.0.0-pulsar.1" }, sha).join()).toContain("not a Pulsar build");
    expect(validateLocalBuild(m, { ...local, flavour: "upstream" }, sha).join()).toContain("pulsar_fork");
    expect(validateLocalBuild(m, { ...local, file: "other.exe" }, sha).join()).toContain("sunshine.exe");
    expect(validateLocalBuild(m, { ...local, licence: "MIT" }, sha).join()).toContain("GPL-3.0-only");
  });
});

const root = join(import.meta.dir, "..", "remote");

describe("fetch-deps helpers", () => {
  test("sha256", () => {
    expect(sha256Hex(new TextEncoder().encode("hello"))).toBe(
      "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    );
  });

  test("glob", () => {
    expect(globMatch("*.inf", "MttVDD.INF")).toBe(true);
    expect(globMatch("mtt*.inf", "MttVDD.inf")).toBe(true);
    expect(globMatch("*.inf", "MttVDD.cat")).toBe(false);
    expect(globMatch("a.inf", "xa.inf")).toBe(false);
  });

  test("inf checks per arch and hardware id", () => {
    const inf = "[Manufacturer]\n%Mfg%=Std,NTamd64\n[Std.NTamd64]\n%Dev%=Inst, Root\\MttVDD\n";
    expect(infProblems(inf, "x64", "Root\\MttVDD")).toEqual([]);
    expect(infProblems(inf, "arm64")).toEqual(["INF has no NTarm64 models section"]);
    expect(infProblems(inf, "x64", "Root\\Other")).toEqual(["INF does not contain the root hardware id Root\\Other"]);
  });

  test("UTF-16LE INF decoding", () => {
    const s = "[Version]";
    const bytes = new Uint8Array(2 + s.length * 2);
    bytes[0] = 0xff;
    bytes[1] = 0xfe;
    for (let i = 0; i < s.length; i++) bytes[2 + i * 2] = s.charCodeAt(i);
    expect(decodeInf(bytes)).toBe(s);
    expect(decodeInf(new TextEncoder().encode(s))).toBe(s);
  });

  test("web UI detection", () => {
    expect(isWebUiPath("assets/web/index.html", ["assets/web"])).toBe(true);
    expect(isWebUiPath("assets\\web\\js\\a.js", ["assets/web"])).toBe(true);
    expect(isWebUiPath("assets/shaders/x.hlsl", ["assets/web", "web"])).toBe(false);
  });

  test("repo manifests are pinned and per-arch", () => {
    const drivers: DriverPackage[] = JSON.parse(readFileSync(join(root, "drivers.json"), "utf8"));
    expect(validateDrivers(drivers)).toEqual([]);
    expect(packagesFor(drivers, "x64").map((p) => p.id).sort()).toEqual(["usbip-win2", "vdd", "vigembus"]);
    expect(packagesFor(drivers, "arm64").map((p) => p.id).sort()).toEqual(["usbip-win2", "vdd", "vigembus"]);
    const stream: StreamManifest = JSON.parse(readFileSync(join(root, "stream-server.json"), "utf8"));
    expect(Object.keys(stream.builds).sort()).toEqual(["arm64", "x64"]);
    expect(validateStream(stream)).toEqual([]);
    const d = serverDist(stream, "stream-server/sunshine.exe", "a".repeat(64));
    expect(d).toEqual({
      flavour: "upstream",
      version: "v2026.516.143833",
      file: "sunshine.exe",
      sha256: "a".repeat(64),
      source_url: stream.source_url,
      licence: "GPL-3.0-only",
    });
  });

  test("stream binary must come from the source-offer repo and tag", () => {
    const m: StreamManifest = {
      flavour: "upstream",
      version: "v1",
      licence: "GPL-3.0-only",
      source_url: "https://github.com/Org/Repo/tree/v1",
      exe: "s.exe",
      install_dir: "s",
      web_ui_dirs: [],
      builds: { x64: { url: "https://github.com/org/repo/releases/download/v1/s.zip", sha256: "a".repeat(64) } },
    };
    expect(validateStream(m)).toEqual([]);
    const other = { ...m, builds: { x64: { url: "https://github.com/Other/Repo/releases/download/v1/s.zip", sha256: "a".repeat(64) } } };
    expect(validateStream(other)).toEqual(["x64: build repo Other/Repo != source repo Org/Repo"]);
    const tag = { ...m, builds: { x64: { url: "https://github.com/Org/Repo/releases/download/v2/s.zip", sha256: "" } } };
    expect(validateStream(tag)).toEqual(["x64: build tag v2 != version v1", "x64: sha256 not pinned"]);
    expect(validateStream({ ...m, source_url: "https://github.com/Org/Repo" }).length).toBe(1);
  });

  test("unpinned hash is refused", () => {
    const p = {
      id: "x",
      version: "1",
      arch: ["x64"],
      url: "https://e/x.exe",
      sha256: "",
      licence: "MIT",
      licence_url: "https://e/L",
      install: { kind: "official_installer", args: [] },
      detect: { kind: "hardware_id", value: "X" },
    } as DriverPackage;
    expect(validateDrivers([p])).toEqual(["x: sha256 not pinned"]);
    expect(validateDrivers([{ ...p, url: "http://e/x" }]).length).toBe(2);
  });
});

describe("fetch-deps drop list (merge step 10)", () => {
  test("upstream installers and service wrapper are left out", () => {
    const stream: StreamManifest = JSON.parse(readFileSync(join(root, "stream-server.json"), "utf8"));
    const drop = stream.drop_paths ?? [];
    expect(isDroppedPath("scripts/vigembus_installer.exe", drop)).toBe(true);
    expect(isDroppedPath("scripts\\install-service.bat", drop)).toBe(true);
    expect(isDroppedPath("Tools/SunshineSvc.exe", drop)).toBe(true);
    expect(isDroppedPath("tools/audio-info.exe", drop)).toBe(false);
    expect(isDroppedPath("sunshine.exe", drop)).toBe(false);
    expect(isDroppedPath("scripts", drop)).toBe(false);
    expect(isDroppedPath("x", undefined)).toBe(false);
  });
});
