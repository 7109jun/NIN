# Roadmap and Known Limitations

Status of NIN Shell 0.1.0. This page is the honest list of what works, what is limited, and what is planned. Items are grouped by how much they affect users.

## Verification status

* The whole workspace passes its tests on Linux (about 280).
* The Windows x64 binary is cross-compiled and was exercised under **Wine** (running scripts, a workflow, `nin build` bundles, the REPL). It has **not yet been tested on real Windows**: Job Object limits, Task Scheduler registration, double-click launch and the installer are implemented but unverified there.
* The Linux x64 binary (glibc 2.39+) was tested on the build machine: scripts, workflows, rlimit-based limits, bundles, `assoc` (XDG) and cron registration (the cron daemon itself was not run, so actual firing was not observed).
* macOS is not supported.

## Known limitations

| Area | Limitation |
|---|---|
| Workflow output | process output is shown after each process finishes, not streamed live |
| Bundles | `plugin:` imports cannot be embedded; non-`.nin` files are not embedded; `--icon` is accepted but not implemented; a bundle with a bad CRC is ignored and starts as plain `nin` instead of failing |
| Reflection | `reflect.get/set/invoke` reach public members only |
| Packages | no signatures or hash pinning; HTTPS registries are unsupported (no TLS stack); CRC-32 detects corruption only |
| Networking | `std.net` HTTP is plain-text only; HTTPS goes through the system `curl` |
| Scheduler | times are naive local time (no DST handling); Windows repetition is 1-minute granular; cron cannot express every interval; Linux `at` tasks are yearly cron lines guarded by the year |
| Linux | requires `crontab` for `schedule register`; no systemd unit integration; glibc 2.39+ binary only |
| Language | no `import` of individual names (modules are imported as a whole); no user-defined operators; single inheritance; no pattern-matching destructuring in `match` |
| Performance | tree-walking interpreter: fast enough for orchestration, not for number crunching |
| Diagnostics | imported files are parsed and validated at load time but are not statically analyzed by `nin check` of the importing script |
| REPL | line-based input: no history navigation, completion or syntax highlighting beyond what the terminal provides |
| Windows console | very old consoles without VT support get plain text; non-UTF-8 code pages are switched to UTF-8 at start-up |

## Roadmap

Ordered roughly by priority; nothing here is a promise of dates.

**0.2 — hardening**
1. Test on real Windows 10/11 and fix whatever it finds; add a Windows CI job.
2. Make a corrupt embedded bundle a hard error.
3. Live streaming of process output in workflows (line-buffered, prefixed).
4. Analyze imported modules in `nin check` (full cross-module checking).

**0.3 — packaging and trust**
5. Package signing and `nin pkg verify`; hash pinning in a lock file (`nin.lock`).
6. TLS for registries and `std.net` (bundled TLS stack, no `curl` dependency).
7. `nin build --icon`, version info and manifest options through `UpdateResource`.
8. Embed data files and plugins in bundles.

**0.4 — language**
9. Destructuring `let`/`match`, selective imports, `async` iteration.
10. Better REPL: history, completion, multi-line editing.
11. A language server (diagnostics, hover, completion) driven by `nin-sema`.

**Later**
12. Bytecode compiler for hot paths; optional JIT-free speed-ups in the interpreter.
13. Plugin ABI v2 with callbacks and streaming values, keeping v1 loadable.
14. Windows service mode for the scheduler daemon; systemd user units on Linux.
15. macOS support.

## How to influence the roadmap

Open an issue describing the problem you want solved (not just the feature), with an example `.nin` script if you can. Small, focused pull requests that fix a limitation from the table above are the fastest way to move something up. See `CONTRIBUTING.md`.
