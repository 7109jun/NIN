# NIN

NIN Shell — a Windows x64 shell and JSON-based scripting language with classes, workflows, parallel execution, packages (`.nnpkg`), native plugins and a task scheduler.

Documentation
* [Learn NIN](docs/Learn-NIN.md) — tutorial, from "hello" to workflows
* [NIN Reference](docs/Reference-NIN.md) — language, workflow engine, standard library, CLI, configuration
* [Module Development Method — NIN](docs/Module-Development-Method-NIN.md) — file modules, packages, native plugins, std modules

## Build

Linux → Windows x64 cross-build (see `scripts/env.sh` for the toolchain notes): `scripts/build-win.sh -p nin-cli`.
Host tests: `cargo test --workspace`. Prebuilt `nin.exe` (Windows x64, cross-built, Wine-tested only): [bin/nin.exe](bin/nin.exe), SHA-256 `2bc96e19432896020d95822c82c736a7fba1a6667c4e7454500f302719d1fd8b`. Spec: [docs/SPEC.md](docs/SPEC.md).
