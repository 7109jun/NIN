# Task Scheduler

`nin schedule` keeps a list of tasks (a command plus a trigger). Tasks can be run by NIN's own daemon or handed to the operating system's scheduler. Implementation: `nin-sched`.

## Concepts

A **task** has: `id` (unique slug generated from the name), `name`, `command` (argv), optional `workdir` and `env`, a `trigger`, `enabled`, `os_registered`, and history (`last_run`, `last_exit`, `run_count`). `schedule add NAME TRIGGER SCRIPT.nin` stores the command `nin run <absolute script path>` with the current directory as `workdir`; `-- PROGRAM ARGS…` stores any program.

State lives in `schedules.toml` (`%APPDATA%\nin\` on Windows, `$XDG_CONFIG_HOME/nin/` or `~/.config/nin/` on Linux). Writes are atomic (temporary file + rename), so a crash cannot leave a half-written store.

## Trigger grammar

| Spec | Meaning |
|---|---|
| `every 5m` | repeat; units `ms s m h d w`, combinable: `1h30m` |
| `every 1h from 2026-11-05 09:00` | repeat on the grid `start + k·interval` |
| `at 2026-12-01 08:00` | once (also `once …`) |
| `daily 03:00` | every day |
| `weekly mon,fri 09:00` | on the listed days (`mon tue wed thu fri sat sun`) |
| `monthly 15 06:30` | day of month 1–31; clamped to the last day of shorter months |
| `logon` / `login` | when the user logs in |
| `boot` / `startup` | when the machine starts |

All times are **naive local time**. No DST adjustment is applied: a task at `02:30` on the night the clock skips that hour fires at the next valid evaluation, not twice.

`every` without `from` anchors on the last run (`last_run + every`), or on creation for a new task. `at` fires once; if it is already overdue when first seen it fires late (once).

## The in-process runner (`nin schedule daemon`)

Run it in a terminal, as a Windows service wrapper, a systemd user service, or an autostart entry. Semantics:

* When a task is first seen (start-up, newly added, re-enabled) its next slot is armed. Slots missed *before* that point are skipped unless catch-up is enabled, in which case the task fires once for the latest missed slot.
* Slots missed while the loop is running (a long task, a suspended laptop) also fire **once**, never in a burst.
* `logon`/`boot` tasks fire once on the first tick of the daemon (it is started at logon/boot).
* The daemon polls once per second, runs each due task as a child process (`workdir`/`env` applied) and records exit code and time.
* Ctrl+C stops it; a running task is cancelled with the same cancel token the rest of NIN uses.
* Tasks with `os_registered = true` are skipped: the OS owns them.

`nin schedule run ID` runs one task immediately and records the result without touching its schedule.

## OS integration (`register` / `unregister`)

Use it when tasks must run without a NIN process alive.

### Windows — Task Scheduler

Tasks are imported as exact Task Scheduler XML through `schtasks.exe` into the `\NIN\` folder (`NIN\<id>`).

* `boot` tasks run as SYSTEM and need an elevated shell; the `schtasks` error text is passed through verbatim.
* `logon` tasks run as the current user.
* Repetition has one-minute granularity; shorter intervals round up.
* `monthly 31` maps to "last day of month"; days 29 and 30 are literal and skipped in short months.
* The Task Scheduler cannot set environment variables, so tasks with `env` are wrapped in `cmd.exe /d /s /c "set … && command"`; because of that, such tasks may not contain `"` or `%` in arguments.

### Linux — cron and XDG autostart

* Triggers become entries in the user's crontab, each preceded by a `# nin:<id>` marker line so `unregister` removes exactly its own lines. The command is `cd 'workdir' && VAR='x' 'prog' 'arg'…`, shell-quoted.
* `every` maps to `*/m * * * *` (m divides 60), `0 */h * * *` (h divides 24) or `0 0 */d * *`. Intervals cron cannot express (for example `every 7m`) are rejected with a message pointing to the daemon.
* `at` becomes a one-year cron line guarded by `[ "$(date +%Y)" = YYYY ]`, so it cannot fire again next year. Remove it with `unregister` afterwards.
* `boot` → `@reboot`. `logon` → an XDG autostart entry (`~/.config/autostart/nin-<id>.desktop`) that runs a small script in `~/.local/share/nin/tasks/`.
* `crontab` must be installed; otherwise registration fails with a clear message. `%` in a command is escaped for cron.

`unregister` is idempotent. `os_query` reports whether a task is registered; running a registered task on demand works on both platforms.

## Choosing between daemon and OS registration

| | daemon | OS registration |
|---|---|---|
| needs a running NIN process | yes | no |
| survives reboot | only if started at boot | yes |
| trigger precision | 1 s | 1 min |
| arbitrary intervals | yes | platform-limited |
| environment variables | yes | wrapped / shell prefix |
