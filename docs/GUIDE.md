# NIN Shell guide

`nin.exe` is a standalone Windows x64 shell whose scripts (`.nin`) are JSON (comments and trailing commas allowed).
Authoritative language/CLI contract: `SPEC.md`.

## Quick start
```
nin                      REPL        nin run examples\hello.nin
nin check FILE.nin       validate    nin build FILE.nin -o app.exe
nin workflow run examples\build.nin --report out.json
```
Double-click a `.nin` file (after `nin assoc install`): runs in the script's folder, console stays open.

## Script layout
`{"nin":"1","imports":[..],"vars":{..},"const":{..},"enums":{..},"interfaces":{..},"classes":{..},"functions":{..},"main":[..],"workflow":{..}}`
Strings containing `${expr}` are templates. Statements: let/const/set/print/if/while/for/match/break/continue/return/throw/try/run/parallel/spawn/await/assert/exit.

## Workflow
Jobs form a DAG via `needs`; independent jobs/matrix cells run in parallel (`--max-parallel`). Jobs/steps support `if`, `timeout`, `retries`, `continue_on_error`, `limits`, `finally`, `on_error`. Ctrl+C cancels everything.

## Packages / plugins / scheduler
`nin pkg install|remove|update|search|list|info|pack|publish` (`.nnpkg`); `nin plugin info|call|list` (DLL ABI, see nin-plugin);
`nin schedule add NAME "every 5m" SCRIPT.nin`, `daemon`, `register` (Windows Task Scheduler; triggers: every/daily/weekly/monthly/at/logon/boot).

## Config
`%APPDATA%\nin\config.toml`: `[shell] prompt history_size color`, `[run] pause_on_exit`, `[paths] packages plugins`, `[registry] sources`, `[workflow] max_parallel`.
Exit codes: 0 ok, 1 failure, 2 usage, 3 invalid script, 130 cancelled.
