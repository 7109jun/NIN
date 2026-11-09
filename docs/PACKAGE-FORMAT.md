# Package Format and Registries (`.nnpkg`)

This document specifies how NIN packages are laid out, archived, published and installed. The implementation is the `nin-pkg` crate.

## Manifest: `nin-package.toml`

```toml
[package]
name        = "mypkg"            # required, [a-z0-9][a-z0-9_-]{0,63}
version     = "1.2.0"            # required, semver 2.0
description = "Small helpers"    # optional
authors     = ["Your Name"]      # optional, array of strings
license     = "MIT"              # optional
entry       = "main.nin"         # optional, relative path imported by {"package":"mypkg"}

[dependencies]
other-pkg = "^1.0"               # semver requirement (cargo syntax)
```

The manifest is limited to 64 KiB. An `entry` must be a safe relative path (same rules as archive paths). A package without `entry` can still be installed and its files used by path, but `{"package":"name"}` needs an entry.

## Archive layout

```
magic        8 bytes   "NNPKG\x01\0\0"
manifest_len u32 LE
manifest     UTF-8 TOML
file_count   u32 LE                      (<= 20 000)
file entry × file_count:
  path_len   u16 LE
  path       UTF-8, relative, '/' separated
  method     u8        0 = stored, 1 = raw deflate
  raw_len    u32 LE
  comp_len   u32 LE
  crc32      u32 LE    of the uncompressed bytes
  data       comp_len bytes
trailer      u32 LE    CRC32 of every byte before the trailer
```

The manifest travels inside the archive; `nin-package.toml` itself is not repeated as a file entry. All integers are little-endian. Files are sorted by path when packing, so packing the same tree twice yields identical bytes.

### Reader guarantees

The reader never panics on malformed input and rejects: bad magic, any length that runs past the buffer, a CRC mismatch (per file or trailer), duplicate paths, more than 20 000 files, more than 256 MiB of uncompressed data, and **unsafe paths** — empty, absolute, containing `..`, `:` (drive letters/streams), NUL, backslashes, empty segments, or Windows reserved device names (`CON`, `NUL`, `COM1`, …). These checks run again when extracting, so a hostile archive cannot write outside the package directory.

## Registries

A registry is a base location holding `<name>-<version>.nnpkg` files and an optional `index.toml`:

```toml
[[package]]
name = "mypkg"
version = "1.2.0"
description = "Small helpers"
file = "mypkg-1.2.0.nnpkg"
size = 4821
[package.dependencies]
other-pkg = "^1.0"
```

* **Directory registries** (`D:\registry`, `/srv/nin`): the index is only a cache. Entries whose file is missing or whose size changed are ignored, and files missing from the index are indexed on the fly by reading their manifest. `nin pkg publish FILE REGISTRY` copies the archive and rewrites the index.
* **HTTP registries** (`http://host/path`): must serve `index.toml` and the archives. HTTPS is not supported (no TLS stack in the binary); an HTTPS source fails with a clear error. Use a local mirror or a reverse proxy for now.
* Sources are tried in `[registry] sources` order; a version found in several sources is taken from the first.

## Installation

`nin pkg install SPEC` where SPEC is `name`, `name@version` (exact), `name@^1.2` (requirement) or a path to a `.nnpkg`.

1. **Resolve.** The resolver is a depth-first backtracking solver. Each package takes the highest candidate satisfying every requirement collected so far; on conflict it backtracks to the next candidate. Installed versions are tried first when the caller prefers them (`install`); `update` prefers the newest. Matching follows the Rust `semver` crate, so pre-release versions are only matched by requirements that name a pre-release. An unsatisfiable set fails with the conflicting requirements listed.
2. **Download/read** each chosen archive and validate it.
3. **Stage** into `<root>/.staging-*` and extract.
4. **Commit** by renaming each staged directory to `<root>/<name>/<version>/` and updating `installed.toml`.

Any failure removes the staging directory and rolls back directories already moved, so a failed install leaves nothing behind.

### On-disk state

```
<root>/installed.toml        [[package]] name, version, explicit (true = asked for, false = pulled in as dependency)
<root>/<name>/<version>/...  extracted files, including nin-package.toml
```

One version per name is active. `<root>` defaults to the packages folder next to the config; `[paths] packages` overrides it.

## Removal and updates

* `remove NAME` refuses when another installed package depends on it (unless `--force`) and afterwards deletes dependencies that are no longer needed and were not installed explicitly.
* `update [NAME]` re-resolves with "newest wins" and reports `name old -> new` for every change; nothing changes when everything is current.
* `list` marks dependencies that were not installed explicitly; `info NAME` prints the manifest data and install directory.

## Using a package from a script

`{"package":"name"}` imports the entry file of the active version. Inside the package use `{"file":"./other.nin"}` for sibling files. `nin build` embeds the entry file and every `.nin` it imports; other files in the package directory (data, DLLs) are **not** embedded.

## Versioning guidance

Follow semver: breaking change to a public function/class/enum or removal → major; additions → minor; fixes → patch. Depend with `^` ranges. Never republish different bytes under an existing `name@version`; registries are append-only by convention (`publish` replaces the file but consumers cache by version).

## Integrity and trust

CRC-32 values detect corruption, **not tampering**: there are no signatures yet. Install only from registries you control or trust. See `SECURITY.md`.
