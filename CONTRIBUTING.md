# Contributing to NIN

Thank you for helping with NIN Shell! This guide explains how to report problems, propose changes and get a pull request merged. Please read it once; it is short on purpose.

By contributing you agree that your contribution is licensed under the project's license (MIT, see `LICENSE`).

## Ways to contribute

* **Report a bug** — something crashes, gives a wrong result, or a diagnostic is misleading.
* **Suggest an improvement** — describe the problem you are trying to solve, not only the feature you imagine.
* **Fix a limitation** — `docs/ROADMAP-AND-LIMITATIONS.md` lists known gaps; these are the best first contributions.
* **Improve the docs** — typos, missing examples, anything that confused you. Documentation fixes are always welcome.
* **Test on real Windows** — the Windows build is mostly verified under Wine; real-machine reports are very valuable.
* **Write a plugin, package or example script** — see `docs/Module-Development-Method-NIN.md`.

## Reporting bugs

Open an issue with:

1. `nin version` and your platform (Windows 10/11 build, or Linux distribution + glibc version).
2. The smallest `.nin` script (or command line) that shows the problem.
3. What you expected, and what happened instead — paste the full diagnostic output, including the `E…`/`W…` code.
4. For workflow problems, the `--report out.json` file if it is not sensitive.

**Security problems:** do not open a public issue. Follow the private reporting steps in `docs/SECURITY.md`.

## Before you start coding

* For anything larger than a small fix, open an issue (or comment on an existing one) first so we can agree on the approach. Changes to the **language format**, the **plugin ABI** or the **package format** need this discussion — they are compatibility promises.
* Read `docs/ARCHITECTURE.md`: dependencies point down the pipeline (`diag → syntax → schema → ast → sema → runtime → workflow → cli`). A change that makes a lower crate depend on a higher one is almost certainly in the wrong place.
* `docs/Reference-NIN.md` is the contract for language behaviour. If your change alters behaviour, the Reference changes in the same pull request.

## Development setup

```
git clone https://github.com/7109jun/NIN
cd NIN
cargo build -p nin-cli            # debug build for your platform
cargo test --workspace            # everything must pass
```

Rust 1.84+ is required. Cross-building for Windows, running under Wine and the release checklist are in `docs/BUILDING-AND-TESTING.md`.

## Making a change

1. Create a branch from `main` (`fix/short-description` or `feat/short-description`).
2. Keep the change **focused**: one problem per pull request. Unrelated clean-ups belong in their own PR.
3. **Add tests.** Every bug fix gets a regression test that fails without the fix; every feature gets tests for the normal path and the failure paths. Where to put them:
   * language behaviour → `crates/nin-runtime/tests/` (small script in, output or error out);
   * analyzer rules → `crates/nin-sema/tests/sema.rs`;
   * workflow behaviour → `crates/nin-workflow/tests/wf.rs`;
   * platform helpers, formats, parsers → unit tests next to the code.
4. Run `cargo test --workspace` and make sure there are no new compiler warnings.
5. Update documentation: the Reference for behaviour, the relevant specialised doc (`WORKFLOW-ENGINE.md`, `PACKAGE-FORMAT.md`, …) for internals, and `docs/ROADMAP-AND-LIMITATIONS.md` if you remove or add a limitation.
6. Open the pull request and fill in what changed, why, and how you tested it (including the platform).

## Code guidelines

**General**
* Match the style of the surrounding code; do not reformat files you are not changing.
* Prefer simple, readable code over clever code. A tree-walking interpreter is simple on purpose.
* No `unwrap()`/`expect()` on data that comes from users, files, the network or plugins — return an error with a helpful message. `unwrap` is fine in tests.
* Parsers and format readers (`.nnpkg`, bundles, plugin results, scripts) must **never panic** on malformed input. Add a test with hostile input when you touch them.
* `unsafe` code is limited to the platform layer (`nin-win`, `nin-plugin`). Every `unsafe` block needs a `// SAFETY:` comment saying why it is sound.
* Keep OS-specific code behind `cfg` inside the platform crates, with a portable fallback so the logic can be tested on Linux.

**Diagnostics** — good error messages are a feature:
* Every user-facing error has a stable code and a source span. Use the existing ranges: `E00xx` syntax, `E01xx` schema, `E02xx` lowering/expressions, `E03xx` semantic, `E04xx` types, `E05xx` runtime, `E06xx` workflow, `E07xx` packages/plugins/scheduler, `E08xx` CLI; warnings are `W…`.
* Say what is wrong, point at the exact text, and when possible suggest the fix (`suggest()` provides "did you mean").
* Do not reuse an existing code for a different problem, and do not renumber codes: scripts and tests may match on them.

**Language and runtime**
* New statements/keys need: schema entry, lowering, analyzer rules, runtime behaviour, tests, and Reference documentation.
* Runtime errors are objects of the error hierarchy; use the most specific class (`TypeError`, `KeyError`, …).
* Blocking operations must observe cancellation (`ex.is_cancelled()`), otherwise Ctrl+C and workflow timeouts cannot stop them.

**Standard library**
* A new function goes into the right module in `crates/nin-std`, validates its arguments (`TypeError`/`ValueError`), reports I/O failures as `IOError`, returns plain values, and is listed in the Reference's module table. Add a test in `crates/nin-std/tests/std.rs`.

**Compatibility rules**
* The `.nin` format version is `"1"`. Do not change the meaning of existing valid scripts; additions must be backwards compatible.
* The plugin ABI (v1) and the `.nnpkg` / bundle layouts are append-only.

## Commit messages and pull requests

* Write the subject in the imperative mood, ≤ 72 characters: `Fix cancel of nested parallel blocks`.
* The body explains *why*, not just *what*; reference the issue (`Fixes #12`).
* Keep commits logical; squash fix-up commits before review.
* Do not commit build output (`target/`) or editor files. Prebuilt binaries in `bin/` are updated only by maintainers as part of a release.

## Review process

A maintainer will review your pull request, usually asking for tests, documentation or smaller scope. Please respond to comments by pushing new commits (or amending, if you prefer); mark conversations resolved when addressed. A PR is merged when tests pass, documentation is updated and a maintainer approves.

## Community

Be respectful and constructive. Assume good intent, critique code and not people, and help newcomers; harassment or discrimination is not tolerated. Maintainers may edit, close or lock contributions that do not follow this.

Questions are welcome — open an issue labelled `question`.
