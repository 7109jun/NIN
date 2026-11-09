# NIN Architecture

NIN Shell is a Rust workspace of small crates, one per stage of a fixed pipeline. Every stage has a narrow job and talks to the next one through plain data (text → JSON tree → AST → results), so each can be tested alone.

```
 .nin text
    │  nin-syntax   JSON "script dialect" parser (comments, trailing commas, duplicate-key errors, spans)
    ▼
 JSON tree + spans
    │  nin-schema   structural validation: allowed keys per statement, "did you mean" hints
    ▼
 validated JSON
    │  nin-ast      lowering to the AST; expression parser for "${...}" and expression positions
    ▼
 Program (AST)
    │  nin-sema + nin-types   static analysis, gradual type checks, warnings
    ▼
 checked Program
    │  nin-runtime  tree-walking interpreter, object model, async tasks     ← nin-std (native modules)
    │  nin-workflow DAG scheduler on top of the runtime                     ← nin-win / nin-plugin / nin-pkg
    ▼
 Windows / Linux backend (processes, job objects, console, registry, cron)
```

## Crates

| Crate | Role | Depends on |
|---|---|---|
| `nin-diag` | `Span`, `Diagnostic`, source map, caret rendering, `suggest` (did-you-mean) | – |
| `nin-syntax` | JSON dialect parser and canonical printer (`nin fmt`) | diag |
| `nin-ast` | AST, lowering from JSON, expression parser, type expressions | syntax, schema, diag |
| `nin-schema` | per-construct schema tables and validator | syntax, diag |
| `nin-types` | `Ty`, `ClassTable`, `assignable` | ast |
| `nin-sema` | semantic analyzer (names, classes, calls, types, reachability, workflow graph) | ast, types, diag |
| `nin-runtime` | `Interp`/`Exec`, values, classes, builtins, reflection, import system, prelude error classes | ast, diag, win, plugin, pkg |
| `nin-std` | `std.*` native modules (fs, process, net, json, yaml, toml, time, …) | runtime, win |
| `nin-workflow` | job/step engine, matrix, retries, timeouts, reports | runtime, win |
| `nin-win` | process execution, job objects/rlimits, console, file association, system info | – (`windows-sys` on Windows, `libc` on Unix) |
| `nin-sched` | scheduled tasks: triggers, store, in-process runner, OS registration | – |
| `nin-pkg` | `.nnpkg` format, registries, resolver, install/remove/update | – |
| `nin-plugin` | plugin ABI v1: `abi` (types), `sdk` (plugin side), `host` (loader) | – |
| `nin-compile` | bundle format used by `nin build` | ast, runtime, pkg |
| `nin-cli` | the `nin` executable: commands, REPL, config, icons/manifest | everything |

Rule of thumb: dependencies point **down the pipeline**. `nin-diag` knows nothing about the language; `nin-ast` never executes anything; `nin-runtime` never prints directly except through the `Sink` it is given.

## Key design decisions

**JSON as the source form.** Scripts are JSON so any tool can read or emit them. The dialect adds comments and trailing commas, and rejects duplicate keys, because silent last-wins duplicates are a common source of bugs. Every node keeps a `Span` so errors point at the exact source text, not at a position in a re-serialized tree.

**Strings are data *or* code.** A value position treats strings as templates (`"${a + b}"`); an expression position treats the whole string as an expression. This is decided by the schema, not by inspecting the text, so `"1 + 2"` is a literal string in `print` and an expression in `if`.

**Tree-walking interpreter.** The runtime evaluates the AST directly. It is simple, easy to debug and fast enough for orchestration workloads, whose time is spent in child processes. Each OS thread has its own `Exec` (call stack, cancel-token chain, optional output `Sink`); the shared `Interp` holds classes, modules, natives, plugins and the bundle VFS.

**Cancellation is cooperative and chained.** An `Exec` carries a `Vec<Arc<AtomicBool>>`. Timeouts, fail-fast, Ctrl+C and `task.cancel` push or set tokens; every loop iteration, call and blocking std function checks them, and process waits poll them and kill the process tree. See `WORKFLOW-ENGINE.md`.

**Gradual typing.** Types are optional. Where both sides are known the analyzer reports a mismatch before running; where either side is `any` the check is deferred to run time (`TypeError`). Generic type arguments are reified, so `new Box<int>("x")` fails at run time even when the analyzer cannot see it.

**Errors are objects.** The error hierarchy (`Error`, `TypeError`, …) is written in NIN itself (a JSON prelude loaded at start-up), so user classes extend it like any other class.

**Platform layer isolation.** Everything OS-specific lives in `nin-win` (processes, console, association), `nin-sched/os.rs` (scheduler registration) and `nin-plugin/host.rs` (dynamic loading). Each has a portable fallback so the logic is unit-testable on Linux, and a Linux implementation (rlimits, XDG, cron) so the same binary design runs on both.

## Execution of `nin run FILE`

1. `nin-cli` reads the file, `Exec::parse_source` runs syntax → schema → lowering and renders diagnostics.
2. `nin-sema` analyzes the program; errors stop the run (exit 3), warnings print.
3. `Exec::run_program` resolves imports (files, packages, std, plugins), defines functions/enums/interfaces/classes in dependency order, evaluates globals and static fields, then runs `main`.
4. A program with a workflow and no `main` hands the module environment to `nin-workflow::run`.
5. Uncaught errors are rendered with a NIN call stack; the exit code is 1 (130 when cancelled).

## Where things live at run time

| Item | Location |
|---|---|
| Config | `%APPDATA%\nin\config.toml` (Linux: `~/.config/nin/config.toml`) |
| Packages | `<packages root>/<name>/<version>/` + `installed.toml` |
| Scheduled tasks | `schedules.toml` next to the config |
| Plugins | any path; default listing folder `<config dir>/plugins` |
