# NIN Plugin ABI (version 1)

The plugin ABI is the stable contract between `nin` and native libraries (`.dll` on Windows, `.so` on Linux). It is a plain C ABI with JSON payloads, so plugins can be written in Rust, C, C++, Zig, Go (cgo) or anything that can export C symbols. The authoritative definitions are in `crates/nin-plugin/src/abi.rs`; this document describes the rules around them. For a tutorial see *Module Development Method*.

## Goals and non-goals

* **Stable**: ABI v1 will not change layout. New capabilities are added by appending fields and bumping the version number only when old plugins would break.
* **Language-neutral**: no Rust types cross the boundary; values are UTF-8 JSON.
* **Isolated failures**: a plugin error or panic becomes a NIN error; it must never take down the host.
* **Non-goal**: sandboxing. A plugin is native code running in the NIN process with the user's permissions (see `SECURITY.md`).

## Exports

```c
const NinPluginInfo* nin_plugin_entry(uint32_t host_abi_version, const NinHostApi* host);  // required
void                 nin_plugin_shutdown(void);                                            // optional
```

`nin_plugin_entry` is called once, right after the library is loaded. It returns a pointer to a `NinPluginInfo` that must stay valid until `nin_plugin_shutdown` (static storage is the usual choice), or `NULL` if the plugin does not support `host_abi_version`. `nin_plugin_shutdown` is called once when the host unloads the plugin (normally at process exit).

## Types

```c
typedef struct { uint8_t* ptr; size_t len; size_t cap; } NinBuf;
typedef int32_t (*NinCallFn)(const char* args_json, size_t args_len, NinBuf* out);
typedef void    (*NinFreeBufFn)(NinBuf buf);
typedef void    (*NinLogFn)(int32_t level, const char* msg);      // 0 debug, 1 info, 2 warn, 3 error; msg NUL-terminated

typedef struct {
  const char* name;        // NUL-terminated, unique within the plugin
  const char* doc;         // one-line signature/description, may be empty
  int32_t     min_args;
  int32_t     max_args;    // -1 = variadic
  NinCallFn   call;
} NinFunction;

typedef struct {
  uint32_t abi_version;    // 1
  const char* name;        // module name suggestion
  const char* version;     // plugin version string
  const char* description;
  uint32_t function_count;
  const NinFunction* functions;
  NinFreeBufFn free_buf;   // required
} NinPluginInfo;

typedef struct {
  size_t   size;           // sizeof(NinHostApi) as known to the host
  uint32_t abi_version;
  NinLogFn log;
  const char* host_name;
  const char* host_version;
} NinHostApi;
```

All structs are `#[repr(C)]` on the Rust side and use natural C alignment.

## Calling convention

1. The host validates the argument count against `min_args`/`max_args` and throws a NIN error itself when it is wrong (it also rejects functions that declare an invalid arity at load time), so the plugin function can assume a count in range.
2. It converts NIN values to a JSON **array** and calls `call(args_json, args_len, &out)`. `args_json` is UTF-8, `args_len` bytes, **not** NUL-terminated.
3. On success the plugin returns `0` and stores the JSON result in `out`. On failure it returns non-zero and stores a UTF-8 error message in `out`.
4. The host copies the bytes out of `out` and immediately returns the buffer to the plugin with `free_buf`.
5. A success result that is not valid JSON is reported as an error (`invalid result JSON`); the plugin must always write a JSON value (use `null` for "no result").

Return codes used by the Rust SDK: `0` ok, `1` function error, `2` caught panic, `3` bad arguments. Hosts treat every non-zero code as an error and show the code with the message (`plugin error (1): …`); plugins in other languages may use any non-zero value.

## Memory rules

* Buffers written to `out` are allocated by the plugin and freed **only** by the plugin's `free_buf`. The host never calls `free` on them, so plugin and host may use different allocators (and different C runtimes on Windows).
* Strings in `NinPluginInfo` and `NinFunction` are owned by the plugin and must outlive the plugin.
* `NinHostApi` and the `msg` passed to `log` are valid only for the duration of the call.

## Threading

Calls can arrive concurrently from several threads (`parallel`, `spawn`, workflow jobs). Plugin functions must be thread-safe or lock internally. `nin_plugin_entry` and `nin_plugin_shutdown` are each called once.

## Values

| NIN | JSON |
|---|---|
| `null`, `bool`, `int`, `float`, `string` | same |
| `list` | array |
| `map` | object (insertion order preserved) |
| object, function, task, module, class | not supported: the host throws a `TypeError` before calling |

Integers outside the 64-bit range and non-finite floats are not representable; use strings.

## Versioning rules

* `NinHostApi.size` lets a plugin built against a newer ABI detect an older host: read a field only if `size` covers it.
* A plugin must refuse (`return NULL`) any `host_abi_version` it does not know to be compatible. An incompatible future ABI will use a different `abi_version`; the intent is for hosts to keep accepting ABI-1 plugins, but this is a goal, not yet a guarantee.
* Adding functions to a plugin is a minor change; changing or removing one is breaking for the scripts that use it.

## Loading

`{"use":"plugin:PATH","as":"alias"}` loads the library once per process. `PATH` is tried as written, then relative to the importing script, with `.dll`/`.so` appended when there is no extension. The functions become members of the alias. `nin plugin info|call|list` load the same way for diagnostics.

## Error mapping

A plugin error reaches scripts as an `Error` whose message is ``plugin `fn`: plugin error (code): message``, so `catch` blocks can match on the message text and `code`. A host-side failure (library not found, missing `nin_plugin_entry`, ABI mismatch) is an `ImportError`.

## Testing a plugin

See *Module Development Method* §3.4. The repository's `plugins/nin-hello` shows a minimal Rust implementation, and `crates/nin-plugin/tests/hello_plugin.rs` shows how the host is tested against it.
