# Module Development Method — NIN

How to extend NIN with your own code. There are four kinds of module; pick the lightest one that does the job.

| Kind | Written in | Distributed as | Imported with | Use it when |
|---|---|---|---|---|
| **File module** | NIN | a `.nin` file | `{"file": "./util.nin"}` | sharing code inside one project |
| **Package** | NIN (+ data files) | `.nnpkg` | `{"package": "name"}` | reusing/versioning code across projects |
| **Plugin** | Rust, C, C++, … | `.dll` (C ABI) | `{"use": "plugin:path"}` | speed, OS/native APIs, existing native libraries |
| **Std module** | Rust, inside NIN | built into `nin.exe` | `{"use": "std.name"}` | core functionality shipped with the shell |

Recommended order of work, whichever kind you build: **design the public surface → implement → check → test → package → publish → version**.

---

## 1. File modules

A module is an ordinary `.nin` file. Everything it declares (`functions`, `classes`, `interfaces`, `enums`, `const`, `vars`) becomes a member of the importer's alias.
Its `main` never runs when imported, so put demonstrations there.

`mathx.nin`
```json
{
  "nin": "1",
  "const": {"TAU": 6.283185307179586},
  "functions": {
    "clamp": {"params": ["x: float", "lo: float", "hi: float"], "returns": "float",
              "body": [{"if": "x < lo", "then": [{"return": "${lo}"}]},
                       {"if": "x > hi", "then": [{"return": "${hi}"}]},
                       {"return": "${x}"}]}
  },
  "classes": {"Counter": {"fields": {"n": {"type": "int", "default": 0, "access": "private"}},
                          "methods": {"inc": {"returns": "int", "body": [{"set": "this.n", "op": "+", "value": 1}, {"return": "${this.n}"}]}}}},
  "main": [{"print": "${clamp(5.0, 0.0, 1.0)}"}]
}
```

`app.nin`
```json
{"nin": "1", "imports": [{"file": "./mathx.nin", "as": "m"}],
 "main": [{"print": "${m.clamp(5.0, 0.0, 1.0)} ${m.TAU}"},
          {"let": "c", "value": "${new m.Counter()}"}, {"print": "${c.inc()}"}]}
```

Rules of thumb
* Paths are relative to the importing file. Cycles between modules are an `ImportError`.
* Annotate parameters and returns on the public surface: the checker and runtime then catch misuse for you.
* Keep fields `private`; expose methods. Private members are reachable only through the declaring class.
* Use `nin check mathx.nin` on the module itself; it is analysed like any script.

## 2. Packages (`.nnpkg`)

A package is a folder with a manifest and files, packed into one archive.

```
mypkg/
  nin-package.toml
  main.nin            # entry file
  helpers.nin
```

`nin-package.toml`
```toml
[package]
name        = "mypkg"            # [a-z0-9][a-z0-9_-]{0,63}
version     = "1.2.0"            # semver
description = "Small helpers"
authors     = ["Your Name"]
license     = "MIT"
entry       = "main.nin"         # file imported by {"package": "mypkg"}

[dependencies]
other-pkg = "^1.0"               # semver requirement
```

Workflow
```
nin check mypkg\main.nin                          # validate the entry
nin pkg pack mypkg .                              # → mypkg-1.2.0.nnpkg
nin pkg install .\mypkg-1.2.0.nnpkg               # install locally (dependencies resolved from registries)
nin pkg info mypkg
```
Then in any script: `{"package": "mypkg"}` → `mypkg.some_function()`. Inside the package, import sibling files with `{"file": "./helpers.nin"}`.

**Registries** are plain directories (or `http://` URLs serving `index.toml`) holding `<name>-<version>.nnpkg` files.
```
nin pkg publish mypkg-1.2.0.nnpkg D:\registry     # copies the file and refreshes index.toml
```
Configure sources in `config.toml` → `[registry] sources = ["D:\\registry"]`; then `nin pkg search`, `install name[@version]`, `update [name]`, `remove name`.
HTTPS registries are not supported yet.

Versioning: follow semver. Removing or changing a public function/class signature is a **major** bump; additions are **minor**; fixes are **patch**.
The resolver picks versions satisfying every requirement and reports conflicts; `remove` also deletes dependencies nothing else needs.
Archive limits: 20,000 files, 256 MiB uncompressed; unsafe paths (`..`, drive letters, absolute) are rejected on pack and install.

A script that imports packages can still be compiled: `nin build app.nin` embeds the package files in the executable.

## 3. Native plugins (DLL, stable C ABI)

A plugin is a DLL that exports a few C functions and a table of functions. Arguments and results are **JSON**, so any language can implement one and
NIN values (null, bool, int, float, string, list, map) map directly. Objects, functions and tasks cannot cross the boundary.

### 3.1 In Rust (recommended)

`Cargo.toml`
```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
nin-plugin = { path = "../nin-shell/crates/nin-plugin" }   # SDK only: no host code, no libloading
```

`src/lib.rs`
```rust
use nin_plugin::sdk;
use nin_plugin::serde_json::{json, Value};

fn args(v: &Value) -> &[Value] { v.as_array().map(|a| a.as_slice()).unwrap_or(&[]) }

fn greet(v: &Value) -> Result<Value, String> {
    let name = match args(v).first() {
        None | Some(Value::Null) => "world".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(o) => return Err(format!("greet: name must be a string, got {o}")),
    };
    sdk::log(0, &format!("greet called for {name}"));   // forwarded to the host
    Ok(json!(format!("Hello, {name}!")))
}

nin_plugin::export_plugin! {
    name: "mine", version: "0.1.0", description: "my first plugin",
    functions: [ ("greet", "greet(name?) -> string", 0, 1, greet) ]
}
```
Each function is `fn(&Value) -> Result<Value, String>` and receives the **argument array**. `Err(msg)` becomes a NIN error. A panic is caught by the SDK
(ABI code 2) and reported as an error: it never unwinds into the host. The tuple is `(name, doc, min_args, max_args, function)`; `max_args = -1` means variadic.

Build for Windows x64 (from Linux or Windows):
```
rustup target add x86_64-pc-windows-gnu            # or use the default MSVC target on Windows
cargo build --release --target x86_64-pc-windows-gnu
```
The result is `my_plugin.dll`. The repository's `plugins/nin-hello` is a complete example.

### 3.2 Using a plugin

```json
{"nin": "1",
 "imports": [{"use": "plugin:./my_plugin.dll", "as": "mine"}],
 "main": [{"print": "${mine.greet('NIN')}"},
          {"try": [{"call": "mine.fail('x')"}], "catch": [{"print": "caught: ${e.message}"}], "as": "e"}]}
```
The path is tried as given, then relative to the script, with `.dll` appended when there is no extension. Each plugin is loaded once per process.
Plugin functions become members of the alias (`mine.greet(...)`); argument counts are checked against `min_args`/`max_args`; errors surface as `Error: plugin \`fn\`: plugin error (1): message`.

Command-line tools for development:
```
nin plugin info my_plugin.dll                 # name, version, function table
nin plugin call my_plugin.dll greet "[\"NIN\"]"   # call with a JSON array
nin plugin list                               # DLLs in the configured plugins folder
```

### 3.3 The ABI (any language)

ABI version **1**. Everything is `#[repr(C)]`; see `crates/nin-plugin/src/abi.rs` for the authoritative definitions.

Exports of the DLL
```c
const NinPluginInfo* nin_plugin_entry(uint32_t host_abi_version, const NinHostApi* host);  // required
void                 nin_plugin_shutdown(void);                                            // optional
```
`nin_plugin_entry` must return `NULL` if it does not support `host_abi_version`. The returned info, and everything it points to, must stay valid until shutdown.

```c
typedef struct { uint8_t* ptr; size_t len; size_t cap; } NinBuf;           // plugin-allocated bytes
typedef int32_t (*NinCallFn)(const char* args_json, size_t args_len, NinBuf* out);
typedef void    (*NinFreeBufFn)(NinBuf buf);
typedef void    (*NinLogFn)(int32_t level, const char* msg);               // 0 debug 1 info 2 warn 3 error

typedef struct { const char* name; const char* doc; int32_t min_args; int32_t max_args /* -1 variadic */; NinCallFn call; } NinFunction;

typedef struct {
  uint32_t abi_version;                    // = 1
  const char *name, *version, *description;
  uint32_t function_count; const NinFunction* functions;
  NinFreeBufFn free_buf;                   // required
} NinPluginInfo;

typedef struct { size_t size; uint32_t abi_version; NinLogFn log; const char* host_name; const char* host_version; } NinHostApi;
```

Call contract
* `args_json` is a UTF-8 JSON **array** (`args_len` bytes, not NUL-terminated).
* Return `0` and write result JSON into `*out`; or return non-zero and write a UTF-8 error message. SDK codes: `1` function error, `2` panic, `3` bad arguments.
* Buffers written to `out` are allocated by the plugin and released by the host through `free_buf`. The host never frees them itself.
* `NinHostApi` is **append-only**: new fields are only added at the end; check `size` before reading a field you did not know in v1. Check `host != NULL`.
* Never let a panic/C++ exception cross the boundary.
* Calls may come from several threads (parallel steps, `spawn`): keep plugin state thread-safe.

### 3.4 Testing a plugin

1. Unit-test the `fn(&Value) -> Result<Value, String>` functions directly with `serde_json::json!` input (no host needed).
2. `nin plugin info` / `nin plugin call` for smoke tests of the built DLL.
3. A `.nin` script that imports the DLL and uses `assert_eq`; run it with `nin run`.
4. Cover: wrong argument types, empty/null arguments, very large lists, error returns, and a deliberate panic.

### 3.5 Compatibility rules

* Do not rely on ABI internals beyond `abi.rs`. A host older than your plugin returns `NULL` from the entry check on your side; a newer host keeps loading ABI-1 plugins.
* Changing a function's meaning or removing it is a breaking change for scripts: bump your plugin's **major** version and document it.
* Ship the DLL next to the scripts that use it (or in the configured `plugins` folder). Compiled executables (`nin build`) cannot embed `plugin:` imports yet.

## 4. Std modules (contributing to NIN itself)

Standard modules live in `crates/nin-std` and are registered in `register_all` as `std.<name>`.

```rust
// crates/nin-std/src/misc.rs
pub fn greeting() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.c("VERSION", Value::str("1"));
    m.f("hello", 1, Some(1), |ex, a| {            // (name, min_args, max_args, closure)
        let who = a_str(ex, a, 0, "hello")?;
        Ok(Value::str(format!("Hello, {who}!")))
    });
    m.done()
}
// crates/nin-std/src/lib.rs → add ("greeting", misc::greeting()) to the list in register_all, and "greeting" to MODULES.
```
Conventions: validate argument types and throw `TypeError`/`ValueError` through `ex.err(kind, msg)`; I/O failures become `IOError`; honour cancellation
(`ex.is_cancelled()`) in anything that blocks or loops; keep results as plain values (lists/maps/strings/numbers). Add tests in `crates/nin-std/tests/std.rs`
and the module to the table in the Reference. Run `cargo test -p nin-std`, then build for Windows with `scripts/build-win.sh` and re-test under Wine.

## 5. Checklist before you publish

- [ ] Public surface is small, documented (`doc` on functions, descriptions in the manifest), and typed.
- [ ] `nin check` passes with no warnings on every entry file.
- [ ] Tests exist and pass (a `.nin` script using `assert`/`assert_eq`, or Rust tests for plugins/std).
- [ ] Error cases throw the right error class, with helpful messages.
- [ ] Version bumped following semver; dependencies use ranges (`^1.0`), not `*`.
- [ ] `nin pkg pack` / `cargo build --release` succeeds from a clean checkout; file paths in archives are relative with `/`.
- [ ] Tried installing it on a machine (or folder) that does not have your source tree.
