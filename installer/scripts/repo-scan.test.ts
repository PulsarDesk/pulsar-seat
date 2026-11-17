// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import { REQUIRED_FILES, missingFiles, parseAllow, publishable, scanName, scanRepo, scanText } from "./repo-scan";

const rules = (path: string, text: string) => scanText(path, text).map((f) => f.rule);
const U = "somebody";
const HDR = "// SPDX-License-Identifier: GPL-3.0-only\n";

describe("text rules", () => {
  test("home-folder paths of real users, placeholders pass", () => {
    expect(rules("a.md", `see C:\\Users\\${U}\\Projects`)).toEqual(["local_path"]);
    expect(rules("a.md", `see C:/Users/${U}/x`)).toEqual(["local_path"]);
    expect(rules("a.md", "cd /home/" + U + "/src")).toEqual(["local_path"]);
    expect(rules("a.md", "/Users/" + U + "/Library/")).toEqual(["local_path"]);
    for (const ok of [
      "C:\\Users\\<you>\\x",
      "C:\\Users\\%USERNAME%\\x",
      "C:\\Users\\$env:USERNAME\\x",
      "C:\\Users\\Public\\x",
      "C:\\Users\\x\\AppData",
      "C:\\Users\\PulsarSeatHost02\\x",
      "C:\\Users\\PulsarKiosk01\\x",
      "%USERPROFILE%\\.pulsar-seat",
      "/home/<user>/x/",
      "https://x.org/home/page",
    ]) {
      expect(rules("a.md", ok), ok).toEqual([]);
    }
  });

  test("keys and tokens", () => {
    expect(rules("a.txt", "-----BEGIN " + "PRIVATE KEY-----")).toEqual(["private_key"]);
    expect(rules("a.txt", "-----BEGIN RSA " + "PRIVATE KEY-----")).toEqual(["private_key"]);
    expect(rules("a.txt", "-----BEGIN CERTIFICATE-----")).toEqual([]);
    expect(rules("a.txt", "RWQAAEIy" + "A".repeat(120))).toEqual(["minisign_secret"]);
    expect(rules("a.txt", "prefix RWQAAEIy only")).toEqual([]);
    expect(rules("a.txt", "ghp_" + "a".repeat(36))).toEqual(["token"]);
    expect(rules("a.txt", "AKIA" + "B".repeat(16))).toEqual(["token"]);
  });

  test("private addresses only in docs and config; e-mail addresses", () => {
    expect(rules("a.md", "host 192.168.1.20 and 10.0.0.5, 172.20.0.1")).toEqual(["private_ip", "private_ip", "private_ip"]);
    expect(rules("a.md", "8.8.8.8 and 172.32.0.1 and 1.192.168.1.1")).toEqual([]);
    expect(rules("a.rs", HDR + "let a = \"192.168.1.20\";")).toEqual([]);
    expect(rules("a.md", "mail someone" + "@corp.test")).toEqual(["email"]);
    expect(rules("a.md", "Co-Authored-By: x <noreply@anthropic.com>, a@example.com, fsfcomm@microsoft.com")).toEqual([]);
  });

  test("SPDX header on our sources, not on vendored ones or other types", () => {
    expect(rules("crates/a/src/x.rs", "fn main() {}")).toEqual(["missing_spdx"]);
    expect(rules("crates/a/src/x.rs", HDR + "fn main() {}")).toEqual([]);
    expect(rules("x.ps1", "# SPDX-License-Identifier: GPL-3.0-only\n")).toEqual([]);
    expect(rules("shim/module/vendor/detours/x.cpp", "int x;")).toEqual([]);
    expect(rules("README.md", "# hi")).toEqual([]);
  });
});

describe("names, required files, allow list", () => {
  test("key files, binaries, size, docs/local", () => {
    expect(scanName("x/testsign.pfx", 10).map((f) => f.rule)).toEqual(["key_file"]);
    expect(scanName("vm-creds.xml", 10).map((f) => f.rule)).toEqual(["key_file"]);
    expect(scanName(".env", 10).map((f) => f.rule)).toEqual(["key_file"]);
    expect(scanName("minisign.key", 10).map((f) => f.rule)).toEqual(["key_file"]);
    expect(scanName("a/b.dll", 10).map((f) => f.rule)).toEqual(["binary"]);
    expect(scanName("a/b.msi", 10).map((f) => f.rule)).toEqual(["binary"]);
    expect(scanName("a/big.txt", 6 * 1024 * 1024).map((f) => f.rule)).toEqual(["binary"]);
    expect(scanName("docs/local/README.md", 10).map((f) => f.rule)).toEqual(["local_doc"]);
    expect(scanName("installer/release-signers.txt", 10)).toEqual([]);
    expect(scanName("src/keyboard.rs", 10)).toEqual([]);
  });

  test("required files", () => {
    expect(missingFiles(new Set(REQUIRED_FILES))).toEqual([]);
    expect(missingFiles(new Set(REQUIRED_FILES.filter((f) => f !== "SECURITY.md"))).map((f) => f.path)).toEqual(["SECURITY.md"]);
  });

  test("allow list format", () => {
    expect(parseAllow("# c\n\na/b.md   email  # why\nspec/  private_ip\nbad\n")).toEqual([
      { path: "a/b.md", rule: "email" },
      { path: "spec/", rule: "private_ip" },
    ]);
  });

  test("this repository is clean", () => {
    const root = resolve(import.meta.dir, "..", "..");
    const allow = parseAllow(require("node:fs").readFileSync(resolve(root, "installer/repo-scan-allow.txt"), "utf8"));
    const errors = scanRepo(root, publishable(root), allow).filter((f) => f.severity === "error");
    expect(errors).toEqual([]);
  });
});
