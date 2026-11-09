# Security Model

NIN is an automation shell: **its purpose is to run programs and touch the file system, network and registry on your behalf.** A NIN script has the same power as any shell script you run. This document states what NIN protects against, what it deliberately does not, and what to do before running code you did not write.

## Trust boundaries

| You run… | NIN assumes | Risk if untrusted |
|---|---|---|
| a `.nin` script | you trust it like a shell script | it can run any program, read/write any file you can, use the network |
| `{"file":…}` imports | same as the script | same |
| a package (`.nnpkg`) | same as a script | same, and it runs automatically when imported |
| a plugin (`.dll`/`.so`) | same as an executable | **native code in the NIN process**; can corrupt memory, read anything |
| a bundle (`app.exe`) | same as an executable | embedded scripts run with your rights |
| a scheduled task | same as the script/program it starts | it runs unattended, repeatedly |

There is **no sandbox**. `limits` in workflows constrain memory, process count and CPU time for the processes a job starts; they are resource guards, not a security boundary.

## What NIN does protect

* **Parsing is safe**: the JSON/AST pipeline is written in safe Rust, never evaluates anything while parsing, and reports duplicate keys instead of silently choosing one.
* **No re-parsing of arguments**: `run` passes `args` as a real argv list, quoted per platform; template values are inserted as data, never re-split, which prevents argument injection through `${...}` (`shell: true` is the explicit opt-in to shell parsing, where *you* own the quoting).
* **Archive hardening** (`.nnpkg`): every length is checked against the buffer, sizes are capped (20 000 files, 256 MiB, 64 KiB manifest), and paths are validated (no `..`, absolute paths, drive letters, backslashes, NUL, reserved device names) both when reading and when extracting, so a hostile package cannot write outside its directory ("zip-slip").
* **Atomic installs**: packages are staged and renamed into place; failures roll back.
* **Plugin boundary**: panics inside plugins written with the SDK are caught and become errors; results are validated as JSON.
* **Cancellation and tree-kill**: timeouts and Ctrl+C kill whole process trees (Job Objects on Windows, process groups on Linux), so abandoned children do not keep running.
* **No hidden execution**: opening or editing a `.nin` file does not run it. Double-click execution is an explicit file association you install (`nin assoc install`) and can remove.

## What NIN does not protect

* **Package integrity is CRC-only.** There are no signatures, no hash pinning and no authentication of registries. HTTP registries are plain `http://` (no TLS stack); anyone on the network path can alter what you download. Use directory registries you control, or verify archives yourself.
* **Plugins are fully trusted.** The ABI cannot limit what native code does.
* **Bundles are not secret.** Embedded source is readable by anyone holding the executable.
* **Secrets in scripts or `env`** are ordinary strings; they appear in process environments, and in workflow reports if you output them. Keep secrets out of reports and logs, and out of `.nin` files that are shared or committed.
* **`std.net`'s HTTPS** delegates to the system `curl`; certificate validation is whatever `curl` does on your machine.
* **Task Scheduler / cron entries** run later with your rights, using the script as it is *at that time*. Anyone who can modify the script can change what runs. Protect the script's directory.

## Practical guidance

1. **Read before you run.** `nin check` shows diagnostics without executing; `nin fmt FILE` prints a normalized copy you can skim. Look for `run`, `shell: true`, `std.process`, `std.net`, `std.fs.remove*`, and `plugin:` imports.
2. **Prefer `args` over `shell: true`.** If you must use a shell line, never interpolate untrusted values into it.
3. **Install packages from trusted sources** and pin by exact version (`name@1.2.0`) in automation.
4. **Treat plugins like installing software.** Build them yourself or verify the vendor.
5. **Run unattended jobs with the least privilege** that works, avoid `boot` (SYSTEM) tasks unless needed, and keep script directories writable only by you.
6. **Use workflow `limits` and `timeout`** for anything that runs foreign code, to contain runaway resource use.

## Reporting a vulnerability

Please report security problems privately to the maintainers (open a private security advisory on the repository, or contact the repository owner) rather than in a public issue. Include the version (`nin version`), your platform, a minimal reproduction and the impact. Fixes for parsing, archive handling, plugin loading and path handling are treated as high priority.
