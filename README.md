# NIN

NIN Shell — a Windows x64 shell and JSON-based scripting language with classes, workflows, parallel execution, packages (`.nnpkg`), native plugins and a task scheduler.

Documentation (start with *Learn*; see [CONTRIBUTING.md](CONTRIBUTING.md) to contribute)
* [Learn NIN](docs/Learn-NIN.md) — tutorial, from "hello" to workflows
* [NIN Reference](docs/Reference-NIN.md) — language, workflow engine, standard library, CLI, configuration
* [Module Development Method — NIN](docs/Module-Development-Method-NIN.md) — file modules, packages, native plugins, std modules

Design and internals
* [Architecture](docs/ARCHITECTURE.md) — pipeline, crates, key design decisions
* [Workflow engine](docs/WORKFLOW-ENGINE.md) — scheduling, retries, timeouts, cancellation, limits
* [Plugin ABI](docs/PLUGIN-ABI.md) — the stable C ABI (v1) for native plugins
* [Package format](docs/PACKAGE-FORMAT.md) — `.nnpkg`, registries, resolver
* [Compilation and bundles](docs/COMPILATION-AND-BUNDLES.md) — what `nin build` produces
* [Scheduler](docs/SCHEDULER.md) — triggers, daemon, Task Scheduler / cron
* [Security model](docs/SECURITY.md) — trust boundaries and guidance
* [Building and testing](docs/BUILDING-AND-TESTING.md)
* [Roadmap and limitations](docs/ROADMAP-AND-LIMITATIONS.md)

## Build

Linux → Windows x64 cross-build (see `scripts/env.sh` for the toolchain notes): `scripts/build-win.sh -p nin-cli`.
Host tests: `cargo test --workspace`. Prebuilt `nin.exe` (Windows x64, cross-built, Wine-tested only): [bin/nin.exe](bin/nin.exe), SHA-256 `2bc96e19432896020d95822c82c736a7fba1a6667c4e7454500f302719d1fd8b`. Spec: [docs/SPEC.md](docs/SPEC.md).

Linux x64 build (glibc 2.39+; rlimit-based process limits, `nin assoc` via XDG, `nin schedule register` via cron/XDG autostart): [bin/nin-linux-x64](bin/nin-linux-x64), SHA-256 `0f20b7fe6eb007452e8da7805bb9ff55f1ac378a6397a560576e1dec2457398b`.
