# NIN Reference

Complete reference for NIN Shell 0.1.0 (language format version `"1"`). For a guided introduction see *Learn NIN*;
for plugin/package authoring see *Module Development Method*.

Contents: 1 Document · 2 Expressions · 3 Statements · 4 Functions · 5 Object model · 6 Errors · 7 Static checks ·
8 Workflow · 9 Processes · 10 Builtins · 11 Standard library · 12 CLI · 13 Configuration · 14 Diagnostic codes

Pipeline: JSON parser → schema validator → AST → semantic analyzer → type system → runtime → workflow scheduler → Windows backend.

---

## 1. Document layout

`.nin` files are JSON in a *script dialect*: `//` and `/* */` comments and trailing commas are accepted; **duplicate keys are an error**;
unknown keys are errors with a "did you mean" hint. Files must be UTF-8 (a BOM is ignored).

| Key | Meaning |
|---|---|
| `"nin"` | **required.** Format version string, `"1"` |
| `"name"`, `"description"` | strings |
| `"imports"` | array of `{"use":"std.fs"}` \| `{"file":"./x.nin"}` \| `{"package":"pkg"}`; optional `"as"` alias (default: last segment / file stem / package name) |
| `"const"` | name → value, immutable globals |
| `"vars"` | name → value, mutable globals |
| `"enums"` | `{"Color":["Red","Green"]}` or `{"Color":{"Red":1,"Green":2}}` |
| `"interfaces"`, `"classes"` | §5 |
| `"functions"` | name → function object (§4) |
| `"main"` | array of statements: the program body |
| `"workflow"` | workflow object (§8) |

An imported `file`/`package` module exposes its functions, classes, interfaces, enums and globals as members of the alias
(`util.helper()`, `new util.Box<int>(1)`, `util.PI`). Its `main` is never run. `{"use":"plugin:path"}` loads a native plugin (see *Module Development Method*).
A script with a `workflow` and no `main` runs the workflow when started with `nin run`.

## 2. Expressions

**Value positions** (`value`, `print`, `return`, `throw`, `args` items, `with`/`env`/`outputs` values): JSON literals are literals; arrays/objects are
evaluated recursively; a string may contain `${expr}`. A string that is exactly one `${expr}` yields the typed value, any other string yields a string. `$${` is a literal `${`.

**Expression positions** (`if`, `while`, `assert`, `match` subject, `for…in`, `call`, `set` target): a string is parsed as an expression
(a surrounding `${…}` is also accepted); JSON booleans/numbers are literals.

**Literals:** ints `1 0xFF 1_000`, floats, strings `'single'`/`"double"` (escapes `\n \t \\ \' \" \$ \u{…}`), `true false null`, lists `[a, b]`, maps `{key: v, "k2": v, [dyn]: v}`.

**Postfix and primaries:** names, `this`, `a.b`, `a?.b`, `a[i]` (negative index counts from the end), `a[i:j]`, `f(x)`, `obj.m(x)`, `new Type<Args>(x)`,
`super.m(x)`, lambda `fn(a, b) => expr`, `await e`, `e is Type`, `e as Type`, `c ? a : b`.

**Precedence, loosest → tightest:** `?:` · `??` · `|| or` · `&& and` · `== !=` · `< <= > >= in is` · `+ -` · `* / %` · `**` (right-assoc) · unary `! not - await` · postfix.

**Semantics**
* `+` concatenates strings and lists. `==` is deep for lists/maps/strings/numbers (`1 == 1.0`); objects compare by identity unless the class defines `equals`.
* `/` is an int when both operands are ints and the division is exact (`6/3 == 2`), else a float (`7/2 == 3.5`). `**` with ints and a non-negative exponent stays int (overflow → float).
  Int overflow in `+ - *` and division/modulo by zero throw `ArithmeticError`.
* Conditions (`if while && || ! ?:` `assert`) must be `bool` or `null` (null = false); other types throw `TypeError`.
* Map member access `m.k` returns `null` for a missing key; index access `m["k"]` throws `KeyError`. List index out of range throws `IndexError`.
* Lists, maps and objects are shared references; strings and numbers are immutable values.

**Types:** `null bool int float string list map object function class enum task module` plus error classes. Annotations: `int float string bool any void`, `list<T>`, `map<T>`,
`T?` (optional), `A | B` (union), class/interface/enum names, generic parameters.

## 3. Statements

One **head key** per statement; the other keys are modifiers. A bare string is not a statement. Blocks are arrays of statements.

| Head | Modifiers | Meaning |
|---|---|---|
| `let` / `const` | `type`, `value` | declare a variable |
| `set` | `value`, `op` (`+ - * / % ** ??`) | assign; target `x`, `a.b`, `xs[0]`, `this.n` |
| `call` | | evaluate an expression for effect |
| `print` / `eprint` | `end` (default `"\n"`) | stdout / stderr; an array value is joined with spaces |
| `if` | `then`, `else` | |
| `while` | `do` | |
| `for` (`"x"` or `["k","v"]`) | `in`, `do` | list items / string chars / map keys; two vars = (index,item) for lists, (key,value) for maps |
| `match` | `cases:[{when, do}]`, `default` | `when` is a value or an array of values; first match runs |
| `break`, `continue` | | loops only |
| `return` | | |
| `throw` | | any value; prefer `new Error(...)` |
| `try` | `catch`+`as`+`type`, or `catches:[{type,as,do}]`, `finally` | |
| `run` | see §9 | external process |
| `parallel` | `max` | array of blocks run concurrently, joined |
| `spawn` | `as` | async task; handle bound to the name |
| `await` | `as` | wait for a task (or list of tasks) |
| `block` | | new scope |
| `assert` | `message` | throws `AssertionError` |
| `output` | `value` | set a workflow step output (workflow `script` steps only) |
| `super` | | parent constructor call (constructors only) |
| `exit` | | exit the process with a code (default 0); not catchable |

Scoping is lexical; `let` shadows inside a block. Lambdas close over their defining scope; top-level functions see globals only.
A `catch` without `type` catches everything including `CancelledError`.

## 4. Functions

```json
"functions": {
  "add": {"params": ["a: int", "b: int = 1", "...rest"], "returns": "int",
          "async": false, "generics": ["T"], "doc": "…", "body": [{"return": "${a + b}"}]}
}
```

Params: `"name"`, `"name: Type"`, `"name: Type = default"`, `"...name"` (variadic list), or `{name,type,default,variadic}`. Defaults are expressions evaluated at call time.
Arguments are positional. Annotations are checked at run time and, where decidable, statically. `async` functions return a task; `await` yields the value or rethrows.
Call depth limit is 1000 (`StateError: stack overflow`).

## 5. Object model

```json
"interfaces": {"Shape": {"generics": [], "extends": [], "methods": {"area": {"params": [], "returns": "float"}}}},
"classes": {"Circle": {
  "extends": "Base", "implements": ["Shape"], "abstract": false, "final": false, "generics": ["T"],
  "fields": {"r": {"type": "float", "default": 1.0, "access": "private", "readonly": false, "static": false}, "n": "int"},
  "constructors": [{"params": ["r: float"], "body": [{"super": []}, {"set": "this.r", "value": "${r}"}]}],
  "methods": {
    "area": {"returns": "float", "body": [{"return": "this.r * this.r * 3.14159"}]},
    "scale": [{"params": ["k: int"], "body": []}, {"params": ["k: float"], "body": []}]
  }}}
```

* `fields`: a type string (shorthand) or an object. `constructor` (one object) is accepted as well as `constructors` (array).
* `methods`: a function object or an **array of overloads**. Method flags: `access` (`public|protected|private`), `static`, `abstract` (no body), `override`, `final`, `async`.
* **Encapsulation:** `private` = declaring class only; `protected` = also subclasses. Violations throw `AccessError` (static error when decidable). Always access fields as `obj.f` / `this.f`.
* **Inheritance:** one `extends`, many `implements`. Field initializers run parent-first before the constructor body. A class without constructors inherits its parent's.
  A derived constructor must call `{"super":[…]}` first when the parent has no zero-argument constructor.
* **Polymorphism:** dynamic dispatch on the runtime class; `super.m()` calls the parent. `override` is required to redefine a concrete inherited method with the same signature.
* **Abstract / interface:** not instantiable; concrete subclasses must implement every abstract/interface method.
* **Overloading:** by arity, then parameter types: exact match, then subtype, then int→float widening, then `any`. An ambiguous call throws `TypeError`.
* **Generics:** `new Box<int>(1)` records type arguments (reified) so field/parameter/return checks use them. Missing arguments are inferred from constructor arguments, else `any`.
* **Enums:** `Color.Red` has `.name .value .ordinal`; `Color.values()`, `Color.from_name("Red")`, `Color.from_value(1)`. Compared by identity.
* **Statics:** `Class.field`, `Class.method()`; static fields initialize once at class definition.
* **Builtin instance methods:** `to_string()` (overridable; used by `str()` and `${}`), `equals(other)`, `hash()`.
* **Reflection** (`reflect`, global): `class_of class_name class classes new fields methods get set invoke parent interfaces is_a type_args is_abstract`.
  `get/set/invoke` only reach public members.

## 6. Errors

`throw` takes any value. Every error object has `message`, `code` (lower-cased class name), `cause`, `stack`. Hierarchy:

```
Error
├─ TypeError  ValueError  KeyError  IndexError  ArithmeticError  AssertionError
├─ IOError  ProcessError (has .result)  TimeoutError  CancelledError
└─ StateError  AccessError  ImportError  NameError
```

Uncaught errors end `nin run` with exit code 1 and a rendered diagnostic with the NIN call stack (`file:line:col` per frame).

## 7. Static checks (`nin check`, run before every `nin run`)

Gradual typing: unannotated things are `any`. **Errors:** unknown names/functions/classes/types/modules, duplicate declarations, call arity (overload-aware),
assignment to `const`, `this`/`super` misuse, abstract/interface/enum instantiation, unimplemented abstract methods, missing/needless `override`, inheritance cycles,
extending a final class, overriding a final method, reduced visibility on override, access violations, generic arity, type mismatches between known types,
non-bool conditions, bad operand types, `break/continue` outside loops, `output` outside workflow scripts, and workflow graph errors (unknown `needs`, cycles, duplicate step ids,
unknown `uses` function or `with` key). **Warnings:** unused variables (`_`-prefixed names are exempt), unreachable code, possibly missing return.
Errors give exit code 3 and stop the run; warnings are printed and the run continues.

## 8. Workflow

```json
"workflow": {
  "name": "ci", "max_parallel": 4, "env": {"CI": "1"},
  "inputs": {"profile": {"type": "string", "default": "debug"}, "fast": {"type": "bool", "required": false}},
  "defaults": {"timeout": "10m", "retries": 0},
  "jobs": {"build": {
    "name": "Build ${matrix.target}", "needs": ["lint"], "if": "success()", "env": {}, "workdir": ".",
    "timeout": "5m", "retries": 1, "retry_delay": "2s", "continue_on_error": false,
    "strategy": {"matrix": {"target": ["a","b"], "include": [{"target":"c"}], "exclude": [{"target":"b"}]}, "fail_fast": true, "max_parallel": 2},
    "limits": {"memory": "512MB", "processes": 16, "cpu_time": "2m"},
    "steps": [
      {"id": "cc", "run": "cargo", "args": ["build"], "outputs": {"log": "${stdout}"}, "on_error": [{"print": "failed: ${error.message}"}]},
      {"id": "s2", "script": [{"output": "answer", "value": 42}]},
      {"id": "s3", "uses": "my_function", "with": {"x": 1}}
    ],
    "outputs": {"artifact": "${steps.cc.outputs.log}"},
    "finally": [{"script": [{"print": "cleanup"}]}]
  }}
}
```

* **Graph:** jobs form a DAG via `needs`; every job whose dependencies finished starts automatically, in parallel, bounded by `max_parallel` (default: CPU count).
  Matrix jobs expand to one instance per combination (cartesian product + `include` − `exclude`), id `build[target=a]`; `needs` on a matrix job waits for all instances.
* **Results:** `success | failure | skipped | cancelled`. A job runs only if all `needs` succeeded, unless its `if` says otherwise (`success() failure() always() cancelled()`).
  `continue_on_error` keeps a failure from failing dependents and the workflow.
* **Steps:** exactly one of `run` (same keys as §9), `script` (statements in an isolated scope; `output` sets outputs) or `uses` (call a function; `with` keys map to parameter names).
  `finally` entries are steps too; they always run, even after cancel or timeout.
* **Context in expressions:** `steps.<id>.{result,outputs,code,stdout,stderr}`, `jobs.<id>.{result,outputs}`, `matrix.*`, `inputs.*`, `env.*`, `workflow.*`, `job.*`, `error` (in `on_error`).
* **Error handling:** `retries` + `retry_delay`, `timeout` (kills the whole process tree), `on_error`, `continue_on_error`, `finally`, fail-fast (cancels running jobs, skips pending ones),
  Ctrl+C cancels the whole workflow.
* **Limits:** `limits` use a Windows Job Object for all processes of the job; exceeding one fails the step with a clear message.
* **Report:** `{name, outcome, duration_ms, jobs:[{id,result,duration_ms,attempts,outputs,steps:[…]}]}`; `--report FILE` writes it. A summary table is printed. Exit code 0 only when the outcome is success.
  Process output is shown with a `[job/step]` prefix after each process finishes (not streamed live).

## 9. External processes

Windows API based (CreateProcessW, pipes, Job Objects). `run` modifiers:

| Key | Meaning |
|---|---|
| `run` | program name or path; resolved via PATH + PATHEXT and the script directory; `.cmd`/`.bat` go through `cmd.exe /c` |
| `args` | argv list, quoted for Windows, never re-split |
| `shell` | `true` runs the whole line through `cmd.exe /c` |
| `cwd`, `env` | working directory, extra environment |
| `stdin` / `stdin_file` | data string / file path |
| `stdout` / `stderr` | `"inherit"`, `"null"`, `"capture"`, or `{"file": path, "append": bool}`; stderr may also be `"stdout"` (merge) |
| `into` | variable receiving `{code, ok, stdout, stderr, duration_ms, timed_out, pid}` |
| `check` | `true` throws `ProcessError` (with `.result`) on non-zero exit |
| `timeout` | ms integer or duration string (`500ms 30s 5m 2h 1d 1h30m`) |

Program not found throws `ProcessError` with code `not_found`. Sizes accept `KB MB GB` strings.

## 10. Builtins

**Globals:** `len str int float bool typeof range(a,b?,step?) keys values has(c,k) push pop error panic assert_eq clone deep_clone env(name,default?) args reflect`.

**String methods:** `upper lower trim trim_start trim_end split join starts_with ends_with contains replace find index_of substr repeat pad_left pad_right chars bytes lines to_int to_float format len is_empty to_string`

**List methods:** `map filter reduce sort sort_by reverse slice index_of contains join first last flat any all each unique concat insert remove clear len push pop find sum min max is_empty to_string`

**Map methods:** `keys values items get has remove merge each map_values filter len clear set is_empty to_string`

## 11. Standard library

Import with `{"use":"std.NAME"}` (the alias defaults to `NAME`).

| Module | Members |
|---|---|
| `fs` | read_text write_text append_text read_bytes write_bytes exists is_file is_dir list_dir mkdir mkdir_all remove remove_all copy rename metadata glob walk temp_file temp_dir canonicalize cwd chdir |
| `path` | join parent file_name stem ext with_ext is_absolute normalize relative_to |
| `process` | run shell spawn pid kill list find exit which env set_env |
| `net` | tcp_connect resolve local_ip http_request http_get http_post download (plain HTTP built in; HTTPS through the system `curl`) |
| `sys` | info os arch hostname username cpu_count uptime memory known_dir |
| `env` | get set remove all expand |
| `io` | read_line stdin_all prompt confirm write ewrite |
| `log` | debug info warn error (threshold from `NIN_LOG`, default `info`) |
| `json` / `yaml` / `toml` | parse stringify (json also `pretty`; yaml also `parse_all`) |
| `object` | keys values entries from_entries merge deep_merge pick omit get_path set_path deep_equal freeze |
| `hash` | sha256 md5 crc32 base64_encode base64_decode hex_encode hex_decode |
| `regex` | is_match find find_all replace split |
| `random` | seed int float choice shuffle bytes uuid |
| `task` | spawn sleep all wait cancel race with_timeout |
| `time` | now now_ms monotonic sleep duration format parse add_days parts |
| `math` | PI E, abs sign clamp pow hypot atan2 is_nan, sqrt sin cos floor and other float functions |
| `str` / `list` / `map` | functional forms of the builtin methods in §10 |

## 12. CLI

```
nin                              interactive shell
nin run FILE.nin [ARGS...]       run (cwd = script folder; waits for a key when launched by double-click)
nin FILE.nin [ARGS...]           same as run
nin check FILE.nin               parse + validate + analyze
nin build FILE.nin [-o OUT.exe]  compile to a self-contained executable  (--icon is accepted but not implemented yet)
nin fmt FILE.nin [--write]       canonical pretty JSON (comments are dropped)
nin eval "EXPR"                  evaluate an expression
nin workflow run|list|graph FILE.nin [--job ID]... [--input k=v]... [--report out.json] [--dry-run] [--max-parallel N]
nin pkg install|remove|update|search|list|info|pack|publish ...
nin plugin info|call|list ...
nin schedule add|list|remove|enable|disable|run|daemon|register|unregister ...
nin assoc install|uninstall|status
nin version | nin help
```

**Exit codes:** 0 success · 1 runtime/script failure · 2 usage error · 3 validation/semantic errors · 130 cancelled (Ctrl+C).

**`pkg`:** `install SPEC` (`name`, `name@version`, or a `.nnpkg` path) · `remove NAME [--force]` · `update [NAME]` · `search TEXT` · `list` · `info NAME` · `pack SRC_DIR OUT_DIR` · `publish FILE.nnpkg REGISTRY_DIR`.
**`plugin`:** `info DLL` · `call DLL FUNCTION [JSON_ARRAY]` · `list` (DLLs in the plugins folder).
**`schedule`:** `add NAME TRIGGER SCRIPT.nin [ARGS…]` or `add NAME TRIGGER -- PROGRAM [ARGS…]` · `list` · `remove|enable|disable|run ID` · `daemon` · `register|unregister ID` (Windows Task Scheduler; `boot` triggers need an elevated shell).
Triggers: `every 5m`, `daily 09:30`, `weekly mon,fri 08:00`, `monthly 15 06:00`, `at 2026-12-01 08:00`, `logon`, `boot`.
**`build`:** the output is a copy of `nin.exe` with the validated program and all imports appended; it runs without the source files. `plugin:` imports cannot be embedded yet.

**REPL:** an expression (value echoed, `_` = last), a JSON statement or script section, `!command`, and `:help :vars :funcs :classes :load FILE :reset :cd DIR :history :quit`.
Input continues while brackets are unbalanced.

## 13. Configuration

`%APPDATA%\nin\config.toml` (override the path with `NIN_CONFIG`):

```toml
[shell]
prompt = "nin> "
history_size = 500
color = true

[run]
pause_on_exit = "auto"      # auto = only when the console was opened by double-click | always | never

[paths]
packages = "C:\\Users\\me\\.nin\\packages"
plugins  = "C:\\Users\\me\\.nin\\plugins"

[registry]
sources = ["C:\\registry", "http://example.com/nin"]

[workflow]
max_parallel = 8
```

`NO_COLOR` disables colour. VT sequences and the UTF-8 code page are enabled at start-up. `NIN_LOG` sets the `std.log` threshold.

## 14. Diagnostic codes

| Range | Area |
|---|---|
| E00xx | syntax (JSON) |
| E01xx | schema / unknown keys |
| E02xx | lowering / expression syntax |
| E03xx | semantic: names, declarations, classes, calls (E0308 arity, E0310 const, E0312 loop control, E0314 this/super, E0316 access, E0319 instantiation, E0320 unimplemented method) |
| E04xx | types (E0401 mismatch, E0402 unknown type, E0403 operator, E0405 condition) |
| E05xx | runtime errors |
| E06xx | workflow (E0601 duplicate step, E0602 unknown job, E0603 cycle, E0604 `uses`) |
| E07xx | packages, plugins, scheduler |
| E08xx | CLI |
| W03xx | warnings: W0301 unused variable, W0302 unreachable code, W0303 missing return |
