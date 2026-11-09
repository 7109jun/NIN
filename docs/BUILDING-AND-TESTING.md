# Building and Testing

## Requirements

* Rust **1.84 or newer** (workspace `rust-version = "1.84"`), `cargo`.
* To build for Windows from Linux: the `x86_64-pc-windows-gnu` target and a MinGW-w64 toolchain (`x86_64-w64-mingw32-gcc`, `windres`). To run Windows binaries on Linux for testing: Wine 9 (64-bit).
* Optional: Inno Setup (`iscc`) to build the installer, Python 3 with Pillow if you regenerate icons.

## Layout

```
crates/*        the pipeline and platform crates (see ARCHITECTURE.md)
plugins/*       example plugins (nin-hello)
examples/       runnable .nin scripts
installer/      Inno Setup script
scripts/        build helpers (env.sh, build-win.sh)
docs/           documentation
bin/            prebuilt binaries (Windows x64, Linux x64)
```

## Build for the current platform

```
cargo build --release -p nin-cli          # → target/release/nin  (nin.exe on Windows)
```

On Windows with an MSVC or GNU Rust toolchain the same command produces `nin.exe`. The `nin-cli` build script uses `windres` to embed the icons and manifest (`crates/nin-cli/assets/`); if `windres` is missing it prints a warning and builds without them.

Release profile: `opt-level 3`, thin LTO, 4 codegen units, symbols stripped. A release binary is about 6 MB.

## Cross-compiling for Windows x64 (from Linux)

```
source scripts/env.sh                      # toolchain environment (see below)
scripts/build-win.sh -p nin-cli            # → target/x86_64-pc-windows-gnu/release/nin.exe
```

`build-win.sh` runs `cargo build --release --target x86_64-pc-windows-gnu -Zbuild-std=std,panic_abort`. The repository's `scripts/env.sh` documents the environment of the machine the first builds were made on: a distribution `rustc` 1.84 driven with `RUSTC_BOOTSTRAP=1` and a locally patched standard library source, and `rsbegin.o`/`rsend.o` stub objects in the workspace root for the linker. **If you have a normal rustup toolchain you do not need any of that** — install the target (`rustup target add x86_64-pc-windows-gnu`) and run `cargo build --release --target x86_64-pc-windows-gnu`. Adjust the paths in `env.sh` to your machine before using it.

## Tests

```
cargo test --workspace                     # about 280 tests
cargo test -p nin-runtime                  # one crate
cargo test -p nin-workflow -- --nocapture  # show output
```

| Crate | What the tests cover |
|---|---|
| `nin-syntax`, `nin-schema`, `nin-ast` | dialect parsing, duplicate keys, schema errors and suggestions, expression grammar, lowering |
| `nin-sema` | analyzer errors and warnings on small programs |
| `nin-types` | type lowering and assignability |
| `nin-runtime` | language semantics (`tests/lang.rs`) and whole-program runs (`tests/run.rs`): classes, overloads, generics, reflection, errors, async |
| `nin-std` | std modules against real files, processes and sockets |
| `nin-workflow` | DAG order, matrices, retries, timeouts, cancellation, fail-fast, limits, reports |
| `nin-win` | quoting, process execution, limits, console fallbacks, association entries |
| `nin-sched` | trigger parsing/next-run, store semantics, runner with an injected clock, OS command/XML generation |
| `nin-pkg` | archive round-trips and hostile-input cases, resolver, install/remove/update with rollback |
| `nin-plugin` | ABI structs, SDK, loading the `nin-hello` plugin end to end |
| `nin-compile` | bundle round-trip and corruption handling |

Tests that start processes use portable commands where possible and gate Windows-only ones with `#[cfg(windows)]`. Some tests need `sh` (Linux) or `cmd` (Windows).

### Testing the Windows build under Wine

```
source scripts/env.sh
scripts/build-win.sh -p nin-cli
$WINE target/x86_64-pc-windows-gnu/release/nin.exe run examples/hello.nin
$WINE target/x86_64-pc-windows-gnu/release/nin.exe workflow run examples/build.nin
$WINE target/x86_64-pc-windows-gnu/release/nin.exe build examples/hello.nin -o /tmp/hello.exe && $WINE /tmp/hello.exe
```

Wine is good for logic and console behaviour; it does not replace testing Job Object limits, Task Scheduler registration or double-click behaviour on real Windows. Use `WINEDEBUG=-all` to silence Wine's own logging.

## Smoke checklist before a release

1. `cargo test --workspace` is green on Linux.
2. Build the Windows binary and run the Wine commands above.
3. On real Windows: `nin run`, a workflow with `limits`, `nin build` + run the result, `nin assoc install` then double-click an example, `nin schedule add … && nin schedule register`.
4. Run `nin check` over `examples/`: no errors and no unexpected warnings.
5. Update the version in `Cargo.toml` (`[workspace.package]`), `crates/nin-cli/assets/nin.rc`, `installer/nin.iss` and the docs; rebuild binaries and update the SHA-256 in the README.

## Building the installer

Copy `nin.exe` to `dist\` and run `iscc installer\nin.iss`. The installer offers to associate `.nin` files (runs `nin assoc install`) and add NIN to the user's `PATH`; uninstall runs `nin assoc uninstall`.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `windres failed; building without icon resources` | install MinGW binutils (`x86_64-w64-mingw32-windres`) |
| link errors about `rsbegin.o` / `rsend.o` | the workspace-root stub objects are missing; restore them from the repository or use a standard rustup toolchain |
| tests that need `sh` fail on Windows | run only the Windows-gated tests there, or use WSL |
