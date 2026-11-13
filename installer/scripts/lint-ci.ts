// SPDX-License-Identifier: GPL-3.0-only
//
// Structural lint of a GitHub Actions workflow (.github/workflows/ci.yml), run by
// installer/scripts/ci-local.ps1 and the CI `bun` job. No network, no third-party linter:
// Bun's YAML parser plus the rules below, which catch the mistakes a workflow that has
// never run is most likely to carry.
//
//   bun installer/scripts/lint-ci.ts [.github/workflows/ci.yml]
//
// Rules:
//   - top level: `name`, `on`, `jobs`; only known top-level keys; `permissions` present
//   - every job: `runs-on`, a non-empty `steps` list, known job keys; `needs` names jobs
//     that exist (no cycles); `${{ matrix.X }}` only for keys the job's matrix defines;
//     `${{ env.X }}` only for env defined at workflow, job or step level
//   - every step: exactly one of `uses` / `run`; `uses` pinned to a tag or SHA (`@...`);
//     only known step keys; `with` only on `uses` steps; unique step ids
//   - pwsh `run` blocks (the default shell on Windows runners) with more than one native
//     command must stop on a failing one: GitHub only checks the LAST exit code, so the
//     block needs `$PSNativeCommandUseErrorActionPreference = $true` or `$LASTEXITCODE`
//     checks
//   - `${{ ... }}` expressions are balanced; no tabs (YAML forbids them for indentation)

const TOP_KEYS = new Set(["name", "run-name", "on", "permissions", "env", "defaults", "concurrency", "jobs"]);
const JOB_KEYS = new Set([
  "name", "needs", "runs-on", "if", "strategy", "steps", "env", "timeout-minutes", "permissions",
  "defaults", "outputs", "continue-on-error", "concurrency", "environment", "services", "container",
]);
const STEP_KEYS = new Set([
  "name", "id", "if", "uses", "run", "with", "env", "shell", "working-directory", "continue-on-error",
  "timeout-minutes",
]);
const NATIVE = /^\s*(&\s*)?(\.\/|\.\\)?[\w./\\-]*(cargo|bun|bunx|rustup|winget|git|msbuild|signtool|infverif|cmd|powershell|pwsh|curl|tar)(\.exe|\.ps1)?\b/i;

export type Finding = { where: string; msg: string };

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

function exprs(text: string): string[] {
  return [...text.matchAll(/\$\{\{([^}]*)\}\}/g)].map((m) => m[1]);
}

function refs(text: string, ns: string): string[] {
  const out: string[] = [];
  for (const e of exprs(text)) {
    for (const m of e.matchAll(new RegExp(`\\b${ns}\\.([A-Za-z_][\\w-]*)`, "g"))) out.push(m[1]);
  }
  return out;
}

function nativeLines(script: string): number {
  let n = 0;
  for (const raw of script.split("\n")) {
    const l = raw.trim();
    if (!l || l.startsWith("#")) continue;
    if (NATIVE.test(l)) n++;
  }
  return n;
}

export function lintWorkflow(text: string): Finding[] {
  const f: Finding[] = [];
  const add = (where: string, msg: string) => f.push({ where, msg });
  text.split("\n").forEach((l, i) => {
    if (/^\t| \t/.test(l) && !l.trimStart().startsWith("#")) add(`line ${i + 1}`, "tab character in indentation");
  });
  const open = (text.match(/\$\{\{/g) ?? []).length;
  const close = (text.match(/\}\}/g) ?? []).length;
  if (open !== close) add("file", `unbalanced \${{ }} (${open} open, ${close} close)`);

  let doc: unknown;
  try {
    doc = Bun.YAML.parse(text);
  } catch (e) {
    add("file", `YAML does not parse: ${(e as Error).message}`);
    return f;
  }
  if (!isObj(doc)) {
    add("file", "top level is not a mapping");
    return f;
  }
  for (const k of Object.keys(doc)) if (!TOP_KEYS.has(k)) add("top level", `unknown key "${k}"`);
  for (const k of ["name", "on", "jobs", "permissions"]) if (!(k in doc)) add("top level", `missing "${k}"`);
  const topEnv = new Set(Object.keys(isObj(doc.env) ? doc.env : {}));
  if (!isObj(doc.jobs) || Object.keys(doc.jobs).length === 0) {
    add("jobs", "no jobs");
    return f;
  }
  const jobs = doc.jobs as Obj;
  const ids = new Set(Object.keys(jobs));
  const needsOf = new Map<string, string[]>();
  for (const [id, j] of Object.entries(jobs)) {
    const w = `jobs.${id}`;
    if (!/^[A-Za-z_][\w-]*$/.test(id)) add(w, "job id must start with a letter or _ and use [A-Za-z0-9_-]");
    if (!isObj(j)) {
      add(w, "job is not a mapping");
      continue;
    }
    for (const k of Object.keys(j)) if (!JOB_KEYS.has(k)) add(w, `unknown key "${k}"`);
    if (!("runs-on" in j)) add(w, `missing "runs-on"`);
    const needs = j.needs === undefined ? [] : Array.isArray(j.needs) ? j.needs.map(String) : [String(j.needs)];
    for (const n of needs) if (!ids.has(n)) add(w, `needs unknown job "${n}"`);
    needsOf.set(id, needs);
    const matrix = isObj(j.strategy) && isObj(j.strategy.matrix) ? (j.strategy.matrix as Obj) : undefined;
    const mkeys = new Set<string>();
    if (matrix) {
      for (const [k, v] of Object.entries(matrix)) {
        if (k === "include" && Array.isArray(v)) {
          for (const row of v) if (isObj(row)) Object.keys(row).forEach((x) => mkeys.add(x));
        } else if (k !== "exclude") mkeys.add(k);
      }
    }
    const jobText = JSON.stringify(j);
    for (const r of refs(jobText, "matrix")) if (!mkeys.has(r)) add(w, `matrix.${r} is not defined in this job's matrix`);
    const jobEnv = new Set([...topEnv, ...Object.keys(isObj(j.env) ? j.env : {})]);
    if (!Array.isArray(j.steps) || j.steps.length === 0) {
      add(w, `missing or empty "steps"`);
      continue;
    }
    const stepIds = new Set<string>();
    j.steps.forEach((s: unknown, i: number) => {
      const sw = `${w}.steps[${i}]${isObj(s) && s.name ? ` (${s.name})` : ""}`;
      if (!isObj(s)) {
        add(sw, "step is not a mapping");
        return;
      }
      for (const k of Object.keys(s)) if (!STEP_KEYS.has(k)) add(sw, `unknown key "${k}"`);
      const hasUses = "uses" in s;
      const hasRun = "run" in s;
      if (hasUses === hasRun) add(sw, "needs exactly one of uses / run");
      if (hasUses && !/^[\w.-]+\/[\w./-]+@[\w.-]+$/.test(String(s.uses))) add(sw, `uses "${s.uses}" is not owner/repo@ref`);
      if (hasRun && "with" in s) add(sw, `"with" on a run step`);
      if (hasUses && ("shell" in s || "working-directory" in s)) add(sw, "shell / working-directory on a uses step");
      if (typeof s.id === "string") {
        if (stepIds.has(s.id)) add(sw, `duplicate step id "${s.id}"`);
        stepIds.add(s.id);
      }
      const env = new Set([...jobEnv, ...Object.keys(isObj(s.env) ? s.env : {})]);
      for (const r of refs(JSON.stringify(s), "env")) if (!env.has(r)) add(sw, `env.${r} is not defined`);
      if (hasRun) {
        const script = String(s.run);
        const shell = typeof s.shell === "string" ? s.shell : "pwsh";
        if (/^(pwsh|powershell)$/.test(shell) && nativeLines(script) > 1 &&
            !/\$PSNativeCommandUseErrorActionPreference\s*=\s*\$true/.test(script) && !/\$LASTEXITCODE/.test(script)) {
          add(sw, "pwsh block runs several native commands but only the last exit code fails the step; set $PSNativeCommandUseErrorActionPreference = $true or check $LASTEXITCODE");
        }
      }
    });
  }
  // needs cycles
  const state = new Map<string, number>();
  const visit = (id: string, path: string[]): void => {
    if (state.get(id) === 2) return;
    if (state.get(id) === 1) {
      add(`jobs.${id}`, `needs cycle: ${[...path, id].join(" -> ")}`);
      return;
    }
    state.set(id, 1);
    for (const n of needsOf.get(id) ?? []) if (ids.has(n)) visit(n, [...path, id]);
    state.set(id, 2);
  };
  for (const id of ids) visit(id, []);
  return f;
}

if (import.meta.main) {
  const file = process.argv[2] ?? ".github/workflows/ci.yml";
  const findings = lintWorkflow(await Bun.file(file).text());
  for (const x of findings) console.error(`${file}: ${x.where}: ${x.msg}`);
  if (findings.length) {
    console.error(`${findings.length} problem(s)`);
    process.exit(1);
  }
  console.log(`${file}: ok`);
}
