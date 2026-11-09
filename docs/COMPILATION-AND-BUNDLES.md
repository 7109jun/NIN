# Compilation and Bundles (`nin build`)

`nin build app.nin -o app.exe` produces a standalone executable. NIN does **not** generate machine code: the output is a copy of the `nin` runtime with your validated program appended to it. This is deliberate — the result behaves exactly like `nin run`, needs no installation, and cross-platform behaviour stays identical.

## What is embedded

* The entry script and every `.nin` file reachable through `{"file":…}` and `{"package":…}` imports, collected transitively. Each file is parsed first; a file with errors stops the build (`run nin check first`).
* The **source text** of those files. It is not encrypted or obfuscated: anyone with the executable can extract it. Do not embed secrets.

## What is not embedded

* Data files your script reads at run time (`std.fs`), other files from a package directory, and native plugins — `{"use":"plugin:…"}` imports make the build fail with an explanatory error.
* Standard-library modules: they are part of the runtime itself.

## File format

```
[runtime executable bytes]
[deflate( JSON {"main": KEY, "files": {KEY: source text, …}} )]
[MAGIC "NINBUNDLE\x01"   10 bytes]
[payload_len             u64 LE]
[crc32 of the payload    u32 LE]
```

The trailer is the last 22 bytes of the file. Appending data after an executable image does not affect how the OS loads it, so the file stays a valid `.exe`/ELF. `KEY` is the normalized absolute path of the file at build time (lower-cased and `/`-separated on Windows).

## Start-up

On every start `nin` reads the trailer of its own image (`read_embedded`). If a valid bundle is present — magic, length and CRC all match — the CLI is skipped:

1. the bundle files are loaded into the interpreter's virtual file system (`Interp.vfs`);
2. the entry file is parsed and analyzed like any script;
3. imports are resolved from the VFS first, so a bundle runs on machines where the original paths do not exist;
4. command-line arguments go to the script (`args()`), not to the CLI.

If the magic is absent, or the stored length does not fit in the file, the file is treated as a plain `nin`. If the magic is present but the CRC does not match, `read_embedded` returns an error; **the current CLI ignores that error and starts as a plain `nin`** instead of refusing to run (listed in `ROADMAP-AND-LIMITATIONS.md`). Building from an executable that already carries a bundle replaces the old bundle rather than stacking a second one.

## Behaviour differences from `nin run`

* The working directory is where the executable is started (not the script directory).
* Relative `file` imports resolve against the **build-time** directory, inside the VFS.
* `nin.exe` options (`check`, `fmt`, `pkg`, …) are not available in a bundle: every argument belongs to your program. The pause-on-exit rule for double-click still applies.

## Platform and size

The bundle is built from the executable that runs `nin build`: `nin.exe` produces a Windows bundle, `nin-linux-x64` a Linux one. A bundle is the size of the runtime (about 6 MB) plus the compressed sources. There is no cross-building of bundles between platforms.

## Icons

Bundles carry the runtime's icon and manifest. `nin build --icon FILE.ico` is accepted but not implemented yet; replacing the icon requires `UpdateResource` on the finished PE file (see `ROADMAP-AND-LIMITATIONS.md`).

## Reproducibility

Files are stored in sorted key order and compressed with a fixed deflate level (6), so building the same inputs with the same runtime should yield the same bytes.
