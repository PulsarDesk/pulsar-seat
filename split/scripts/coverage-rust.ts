// SPDX-License-Identifier: GPL-3.0-only
// Rust line coverage for the whole workspace (cargo-llvm-cov).
//
// Why this script and not a plain `cargo llvm-cov`:
// - On aarch64-pc-windows-msvc (Rust 1.99.1 / LLVM 23.1.1) every native profraw is
//   "malformed instrumentation profile data: symbol name is empty", even for an empty crate.
//   So tests run for x86_64-pc-windows-msvc (x64 emulation on ARM64, native on x64).
// - A few tiny profraws still come out malformed (child processes); one bad file makes
//   `llvm-profdata merge -f <list>` fail as a whole, so they are dropped before the report.
//
// Usage: bun scripts/coverage-rust.ts   (output: target/coverage/rust/html/index.html)
import { readdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const root = join(import.meta.dir, "..");
const target = process.env.COVERAGE_TARGET ?? "x86_64-pc-windows-msvc";
const covDir = join(root, "target", "llvm-cov-target");

function run(cmd: string, args: string[]): number {
  const r = spawnSync(cmd, args, { cwd: root, stdio: "inherit" });
  return r.status ?? 1;
}

function sysroot(): string {
  const r = spawnSync("rustc", ["--print", "sysroot"], { encoding: "utf8" });
  return r.stdout.trim();
}

function hostTriple(): string {
  const r = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  return /host: (\S+)/.exec(r.stdout)?.[1] ?? "";
}

run("cargo", ["llvm-cov", "clean", "--workspace"]);
if (run("cargo", ["llvm-cov", "--workspace", "--target", target, "--no-report"]) !== 0) {
  console.error("tests failed under coverage");
  process.exit(1);
}

const profdata = join(sysroot(), "lib", "rustlib", hostTriple(), "bin", "llvm-profdata.exe");
let dropped = 0;
for (const f of readdirSync(covDir).filter((n) => n.endsWith(".profraw"))) {
  const p = join(covDir, f);
  if (spawnSync(profdata, ["show", p], { stdio: "ignore" }).status !== 0) {
    rmSync(p);
    dropped++;
  }
}
if (dropped) console.warn(`dropped ${dropped} malformed profraw file(s)`);

const out = join("target", "coverage", "rust");
let code = run("cargo", ["llvm-cov", "report", "--target", target, "--html", "--output-dir", out]);
code ||= run("cargo", ["llvm-cov", "report", "--target", target, "--summary-only"]);
process.exit(code);
