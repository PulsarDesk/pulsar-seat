// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test } from "bun:test";
import { lintWorkflow } from "./lint-ci";

const ok = `name: ci
on:
  push:
permissions:
  contents: read
env:
  BUN_VERSION: "1"
jobs:
  a:
    runs-on: windows-2025
    strategy:
      matrix:
        include:
          - arch: x64
    steps:
      - uses: actions/checkout@v4
      - name: one
        run: cargo build --target \${{ matrix.arch }}
  b:
    needs: [a]
    runs-on: windows-2025
    steps:
      - name: two
        run: |
          $PSNativeCommandUseErrorActionPreference = $true
          cargo build
          bun test
      - uses: oven-sh/setup-bun@v2
        with:
          bun-version: \${{ env.BUN_VERSION }}
`;

const msgs = (t: string) => lintWorkflow(t).map((f) => `${f.where}: ${f.msg}`);

describe("lint-ci", () => {
  test("clean workflow", () => {
    expect(msgs(ok)).toEqual([]);
  });

  test("the repository workflow is clean", async () => {
    const text = await Bun.file(new URL("../../.github/workflows/ci.yml", import.meta.url)).text();
    expect(msgs(text)).toEqual([]);
  });

  test("multi-command pwsh block without exit handling", () => {
    const t = ok.replace("          $PSNativeCommandUseErrorActionPreference = $true\n", "");
    expect(msgs(t).join("\n")).toContain("only the last exit code");
    // A bash step is not a pwsh block.
    const bash = t.replace("      - name: two\n", "      - name: two\n        shell: bash\n");
    expect(msgs(bash)).toEqual([]);
  });

  test("unknown needs, matrix and env references", () => {
    expect(msgs(ok.replace("needs: [a]", "needs: [zz]")).join("\n")).toContain('needs unknown job "zz"');
    expect(msgs(ok.replace("matrix.arch", "matrix.target")).join("\n")).toContain("matrix.target is not defined");
    expect(msgs(ok.replace("env.BUN_VERSION", "env.NOPE")).join("\n")).toContain("env.NOPE is not defined");
  });

  test("step shape", () => {
    expect(msgs(ok.replace("      - uses: actions/checkout@v4\n", "      - uses: actions/checkout\n")).join("\n")).toContain("owner/repo@ref");
    const both = ok.replace("      - name: one\n", "      - name: one\n        uses: a/b@v1\n");
    expect(msgs(both).join("\n")).toContain("exactly one of uses / run");
    expect(msgs(ok.replace("runs-on: windows-2025\n    strategy", "strategy")).join("\n")).toContain('missing "runs-on"');
    expect(msgs(ok.replace("    steps:\n      - name: two", "    stepz:\n      - name: two")).join("\n")).toContain('unknown key "stepz"');
  });

  test("needs cycle, tabs, YAML errors, missing top keys", () => {
    const cyc = ok.replace("  a:\n    runs-on", "  a:\n    needs: [b]\n    runs-on");
    expect(msgs(cyc).join("\n")).toContain("needs cycle");
    expect(msgs(ok.replace("    runs-on: windows-2025\n    strategy", "\truns-on: windows-2025\n    strategy")).join("\n")).toContain("tab");
    expect(msgs("jobs: [").join("\n")).toContain("YAML does not parse");
    expect(msgs(ok.replace("permissions:\n  contents: read\n", "")).join("\n")).toContain('missing "permissions"');
    expect(msgs(ok.replace("${{ matrix.arch }}", "${{ matrix.arch }")).join("\n")).toContain("unbalanced");
  });
});
