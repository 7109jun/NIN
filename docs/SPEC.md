# NIN language & shell specification (v1) — implementation contract

`.nin` files are **JSON** documents (script dialect: `//` and `/* */` comments and trailing commas
are tolerated; duplicate keys are an error). Configuration is TOML. The pipeline is

```
JSON parser (nin-syntax) → schema validator (nin-schema) → AST lowering (nin-ast::lower)
  → semantic analyzer (nin-sema) → type system (nin-types) → runtime (nin-runtime)
  → workflow scheduler (nin-workflow) → Windows backend (nin-win)
```

AST types live in `crates/nin-ast/src/lib.rs` (the authoritative shape). This file defines the JSON
surface syntax that lowers onto it, and the runtime semantics.

## 1. Document layout

Top-level object (all keys optional except `"nin"`; unknown keys are errors with a "did you mean" hint):

| key | meaning |
|---|---|
| `"nin"` | required string, format version, `"1"` |
| `"name"`, `"description"` | strings |
| `"imports"` | array of `{"use":"std.fs"}` \| `{"file":"./x.nin"}` \| `{"package":"pkg"}`, each with optional `"as"` alias (default: last path segment / file stem / package name) |
| `"const"` | object name → value (immutable globals) |
| `"vars"` | object name → value (mutable globals) |
| `"enums"` | `{"Color": ["Red","Green"]}` or `{"Color": {"Red":1,"Green":2}}` |
| `"interfaces"` | see §4 |
| `"classes"` | see §4 |
| `"functions"` | object name → function object (§3) |
| `"main"` | array of statements (§2): the program body |
| `"workflow"` | workflow object (§6) |

A module imported by `file`/`package` exposes its functions, classes, interfaces, enums and globals
as members of the alias (`util.helper()`, `new util.Box<int>(1)`, `util.PI`). `main` of an imported
module is never run. Std modules are native (§8).

## 2. Values, expressions, statements

### Value positions vs. expression positions
* **Value positions** (`value`, `args` items, `with`/`env`/`outputs` values, `print`, `return`, `throw`, …):
  a JSON literal is a literal; arrays/objects are evaluated recursively; a **string** may contain
  `${expr}` interpolations. A string that is exactly one `${expr}` yields the raw typed value, anything
  else yields a string. Escape a literal `${` as `$${`.
* **Expression positions** (condition-like fields: `if`, `while`, `assert`, `match` subject, `in`
  of `for`, `call`, `set` target, `outputs`... see each statement): a string is parsed *as an expression*
  (`"if": "x > 3 && ready"`); a surrounding `${...}` is also accepted; a JSON boolean/number is a literal.

### Expression language
Literals: ints (`1`, `0xFF`, `1_000`), floats, strings `'single'` or `"double"` (escapes `\n \t \\ \' \" \$ \u{...}`),
`true false null`, lists `[a, b]`, maps `{key: v, "k2": v, [dyn]: v}`.
Names, `this`, member `a.b`, optional `a?.b`, index `a[i]` (negative indexes count from end for lists/strings),
slice `a[i:j]`, calls `f(x, y)`, `obj.m(x)`, `new Type<Args>(x)`, `super.m(x)`, lambda `fn(a, b) => expr`,
`await e`, `e is Type`, `e as Type`, ternary `c ? a : b`.
Operators, loosest → tightest: `?:` · `??` · `|| or` · `&& and` · `== !=` · `< <= > >= in is` ·
`+ -` · `* / %` · `**` (right assoc) · unary `! not - await` · postfix (`.` `?.` `[]` `()` `as`).
`+` concatenates strings and lists; `==` is deep structural for lists/maps/strings/numbers (`1 == 1.0` is true);
objects compare by identity unless the class declares `equals`.
Arithmetic: `/` yields an int when both operands are ints and the division is exact (`6/3 == 2`), otherwise a float (`7/2 == 3.5`);
`%` is remainder; `**` with int operands and a non-negative exponent stays int (overflow → float). Integer overflow in `+ - *`
throws `ArithmeticError`; division/modulo by zero throws `ArithmeticError`.
Conditions (`if while && || ! ?:`, `assert`) must be `bool` or `null` (null counts as false); any other type throws `TypeError`
("condition must be bool").

### Types of values
`null, bool, int (i64), float (f64), string (UTF-8), list, map (insertion-ordered, string keys), object (class instance),
function (closure/bound method/native), class, enum value, task, error objects, module`.
Lists/maps/objects are **reference** values (shared, thread-safe), strings/numbers are immutable values.

### Builtin globals (no import needed)
`len(x) str(x) int(x) float(x) bool(x) typeof(x) range(a,b?,step?) keys(m) values(m) has(c,k) push(l,x) pop(l)
 error(msg) panic(msg) assert_eq(a,b) clone(x) deep_clone(x) env(name,default?) args()` (script arguments as list of strings),
`print`-family are statements. Builtin methods on values: string (`upper lower trim split join starts_with ends_with
contains replace find substr repeat pad_left pad_right chars bytes lines to_int to_float format`), list (`map filter reduce
sort sort_by reverse slice index_of contains join first last flat any all each unique concat insert remove clear len`), map
(`keys values items get has remove merge each map_values filter len clear`). Exact set is documented in docs/stdlib.md.

### Statements
Each statement is an object with one **head key**; other keys are modifiers.

| head | modifiers | meaning |
|---|---|---|
| `"let": "x"` | `"type"`, `"value"` | mutable variable |
| `"const": "x"` | `"type"`, `"value"` | immutable variable |
| `"set": "target"` | `"value"`, `"op"` (`+ - * / % ** ??`) | assignment; target is expression-position: `x`, `a.b`, `xs[0]`, `this.n` |
| `"call": "expr"` | | evaluate expression for effect |
| `"print": V` / `"eprint": V` | `"end"` (string, default `"\n"`) | stdout / stderr; array value → items joined by space |
| `"if": COND` | `"then"`, `"else"` (blocks) | |
| `"while": COND` | `"do"` | |
| `"for": "x"` or `["k","v"]` | `"in": EXPR`, `"do"` | list items / string chars / map keys; two vars: (index,item) for lists, (key,value) for maps |
| `"match": EXPR` | `"cases": [{"when": V or [V..], "do": [...]}]`, `"default"` | first matching case runs |
| `"break": true`, `"continue": true` | | |
| `"return": V` | | |
| `"throw": V` | | any value; `new Error(...)` preferred |
| `"try": [...]` | `"catch"`+`"as"`+`"type"` or `"catches": [{"type","as","do"}]`, `"finally"` | |
| `"run": V` (program) | `"args"`, `"shell"`, `"cwd"`, `"env"`, `"stdin"`, `"stdin_file"`, `"stdout"`, `"stderr"`, `"into"`, `"check"`, `"timeout"` | external process (§7) |
| `"parallel": [[stmts],[stmts]]` | `"max"` | run branches concurrently, join all |
| `"spawn": [stmts]` | `"as": "name"` | async task; handle bound to `name` |
| `"await": EXPR` | `"as": "name"` | wait for a task / list of tasks |
| `"block": [...]` | | scope |
| `"assert": COND` | `"message"` | throws `AssertionError` |
| `"output": "name"` | `"value"` | workflow step output |
| `"super": [args]` | | parent constructor call |
| `"exit": V` | | exit process with code (default 0) |

A bare string statement `"echo hi"` is **not** allowed. Blocks are arrays of statements.
Scoping: lexical; `let` shadows within a block; functions are closures over their defining scope for lambdas,
top-level functions see globals only.

### Errors
`throw` accepts any value. Builtin error class hierarchy (all have `message`, `code` (string), `cause`, `stack` (list of strings)):
`Error` ← `TypeError ValueError KeyError IndexError ArithmeticError AssertionError IOError ProcessError TimeoutError
CancelledError StateError AccessError ImportError`. `catch` with `"type"` filters by class (subclasses match); a catch without
`"type"` catches every thrown value including `CancelledError`. `exit` is not catchable. A non-error thrown value is wrapped for
reporting only. Uncaught errors terminate `nin run` with exit code 1 and a rendered diagnostic with the NIN call stack
(file:line:col of each frame). Runtime diagnostics use codes E05xx.

## 3. Functions

```json
"functions": {
  "add": { "params": ["a: int", "b: int = 1", "...rest"], "returns": "int",
           "async": false, "generics": ["T"], "doc": "...", "body": [ {"return": "${a + b}"} ] }
}
```
Params: `"name"`, `"name: Type"`, `"name: Type = default-expression"`, `"...name"` (variadic list), or objects
`{"name","type","default","variadic"}`. Defaults are expression-position (evaluated at call time). Arguments
are positional; a trailing map argument can't implicitly become named args (use `uses`/`with` in workflows).
Type annotations are checked at runtime (and statically by nin-sema where decidable). `async` functions return a `task<T>`;
`await` yields the value or rethrows the error. Recursion is allowed; call depth limit 1000 → `StateError("stack overflow")`.

## 4. Object model

```json
"interfaces": { "Shape": { "generics": [], "extends": [], "methods": { "area": {"params": [], "returns": "float"} } } },
"classes": {
  "Circle": {
    "extends": "Base", "implements": ["Shape"], "abstract": false, "final": false,
    "generics": ["T", "K: Comparable"], "doc": "...",
    "fields": { "r": {"type": "float", "default": 1.0, "access": "private", "readonly": false, "static": false}, "n": "int" },
    "constructors": [ {"params": ["r: float"], "body": [ {"super": []}, {"set": "this.r", "value": "${r}"} ]} ],
    "methods": {
      "area": {"returns": "float", "body": [ {"return": "this.r * this.r * 3.14159"} ]},
      "scale": [ {"params": ["k: int"], "body": []}, {"params": ["k: float"], "body": []} ]
    }
  }
}
```
* `"fields"` value may be a type string (shorthand) or an object. `"constructor"` (single object) is accepted besides `"constructors"` (array).
* `"methods"` value is a function object or an **array of overloads**. Method flags: `"access"` (`public|protected|private`, default public),
  `"static"`, `"abstract"` (no body), `"override"`, `"final"`, `"async"`.
* **Encapsulation:** `private` visible only inside the declaring class; `protected` also in subclasses; violation throws `AccessError`
  (and is a static error in nin-sema when decidable). Fields are accessed as `obj.f` / `this.f` (never bare).
* **Inheritance:** single `extends`; many `implements`. Constructors: derived constructor must call `{"super":[...]}` first if the
  parent has no zero-arg constructor (else implicit). Field initializers run (parent first) before the constructor body.
  A class with no constructor gets a default one (and, if abstract-less and parent has none, no-args).
* **Polymorphism:** dynamic dispatch on the runtime class; `super.m()` for parent calls; `override` required when redefining an inherited method
  with the same signature (sema error otherwise).
* **Abstract classes:** cannot be instantiated; abstract methods must be implemented by concrete subclasses; interface methods must be
  implemented by concrete classes.
* **Overloading:** by arity, then by parameter types (exact match > subtype > widening int→float > `any`); ambiguous call → `TypeError`
  at call time (and a sema warning when decidable). Variadic and default params participate by arity range.
* **Generics:** classes, interfaces and functions can declare type parameters (optional bound `T: Bound`). Instantiation `new Box<int>(1)`
  records type arguments in the object (reified): field/param/return type checks use the substituted types; `list<T>` etc. work.
  Missing type arguments are inferred from constructor args where possible, else `any`.
* **Reflection** (global object `reflect`, no import): `reflect.class_of(x)`, `reflect.class_name(x)`, `reflect.class(name)` → class value,
  `reflect.new(classOrName, args...)`, `reflect.fields(x|class)` (list of `{name,type,access,static,readonly}`), `reflect.methods(x|class)`
  (list of `{name,params:[{name,type}],returns,access,static,abstract,overloads}`), `reflect.get(obj,name)`, `reflect.set(obj,name,v)`,
  `reflect.invoke(obj,name,args...)`, `reflect.parent(class)`, `reflect.interfaces(class)`, `reflect.is_a(x, class|name)`, `reflect.type_args(x)`,
  `reflect.is_abstract(class)`, `reflect.classes()` (all in scope). `reflect.get/set/invoke` respect access control unless called from inside the class.
* Instances have builtin methods `to_string()`, `equals(other)`, `hash()`; classes may override `to_string` (used by `str()` and `${}`).
* Enum values: `Color.Red` has `.name`, `.value`, `.ordinal`; `Color.values()`, `Color.from_name("Red")`; enum values compare by identity.
* Static members: `Class.field`, `Class.method()`; static fields initialized once at class definition.

## 5. Static semantics (nin-sema / nin-types)

Gradual typing. Un-annotated things are `any`. Checks (errors unless noted): unknown identifiers/functions/classes/types/modules
(with "did you mean"), duplicate declarations, arity of calls to known functions/constructors/methods (overload-aware), assignment to
`const`, `this` outside methods, `super` misuse, abstract instantiation, unimplemented abstract/interface methods, `override` mismatch,
inheritance cycles, extending `final`, final method override, access violations, generic arity & bound violations, type mismatches where
both sides are statically known (`let x: int = "a"`), unreachable code after `return`/`throw` (warning), unused variables (warning),
missing return in non-void function (warning), `break/continue` outside loops, `output` outside workflow scripts, workflow graph:
unknown `needs`, dependency cycles (with the cycle path), duplicate step ids, unknown `uses` function.

## 6. Workflow engine

```json
"workflow": {
  "name": "ci", "max_parallel": 4, "env": {"CI": "1"},
  "inputs": {"profile": {"type":"string","default":"debug"}, "fast": {"type":"bool","required":false}},
  "defaults": {"timeout": "10m", "retries": 0},
  "jobs": {
    "build": {
      "name": "Build ${matrix.target}", "needs": ["lint"], "if": "success()", "env": {}, "workdir": ".",
      "timeout": "5m", "retries": 1, "retry_delay": "2s", "continue_on_error": false,
      "strategy": {"matrix": {"target": ["a","b"], "include": [{"target":"c","extra":1}], "exclude": [{"target":"b"}]}, "fail_fast": true, "max_parallel": 2},
      "limits": {"memory": "512MB", "processes": 16, "cpu_time": "2m"},
      "steps": [
        {"id": "cc", "name": "compile", "run": "cargo", "args": ["build"], "env": {}, "timeout": "2m",
         "retries": 2, "retry_delay": "1s", "continue_on_error": false, "if": "always()", "outputs": {"log": "${stdout}"},
         "on_error": [ {"print": "failed: ${error.message}"} ]},
        {"id": "s2", "script": [ {"output": "answer", "value": 42} ]},
        {"id": "s3", "uses": "my_function", "with": {"x": 1}}
      ],
      "outputs": {"artifact": "${steps.cc.outputs.log}"},
      "finally": [ {"run": "cleanup.exe"} ]
    }
  }
}
```
* **Graph:** jobs form a DAG via `needs`. All jobs whose dependencies are done start **automatically in parallel**, bounded by
  `max_parallel` (default CPU count). Matrix jobs expand into one instance per combination (cartesian product of dims + `include` − `exclude`);
  instance id `build[target=a]`; `needs` on a matrix job waits for all instances.
* **Results:** each job/step has a result: `success | failure | skipped | cancelled`. A job runs only if all `needs` succeeded
  **unless** its `if` says otherwise (`always()`, `failure()`, `success()`, `cancelled()` are available in `if`).
  `continue_on_error` makes failure not fail downstream/the workflow (result still `failure`, workflow `outcome` unaffected).
* **Step kinds:** `run` (same keys as the `run` statement incl. `args/shell/cwd/stdin`, stdout/stderr always captured and tee'd live
  to the console with a `[job/step]` prefix), `script` (statements in an isolated child scope; sees globals/functions/classes; `output` statements set outputs;
  `inputs`, `env`, `matrix`, `steps`, `jobs` are readable context objects), `uses` (call a program/module function with named `with` args mapped by parameter name).
* **Step context for expressions:** `steps.<id>.{result,outputs,code,stdout,stderr}`; `outputs` on a `run` step map evaluated after the process with `stdout`,
  `stderr`, `code` bound; `jobs.<id>.{result,outputs}`; `matrix.*`; `inputs.*`; `env.*`; `workflow.{name,run_id,start_time}`; `job.{id,name,status}`; `error` in `on_error`.
* **Error handling:** step `retries` + `retry_delay` (re-run on failure/timeout), per-step/job `timeout` (kills the whole process tree via job object),
  `on_error` handler, `continue_on_error`, job `finally` steps (always run, even after cancel/timeout), workflow-level `fail_fast` cancels running jobs and
  skips pending ones (running processes are killed) when a job fails and `continue_on_error` is false. Ctrl+C cancels the whole workflow gracefully.
* **Resource limits:** `limits` apply to every process of the job through a Windows Job Object (memory, active processes, CPU time). Exceeding → failure with
  a clear message.
* **Result collection:** a report object `{name, outcome, duration_ms, jobs:[{id,result,duration_ms,attempts,outputs,steps:[{id,result,code,duration_ms,attempts,outputs}]}]}`;
  `nin workflow run --report out.json` writes it; a summary table is printed. Exit code 0 iff outcome == success.
* **`parallel`/`spawn`/`await` statements and `async` functions** use the same cancellation token as the workflow (cancel → `CancelledError`).

## 7. External processes (statement `run`, workflow steps)

Windows API based (CreateProcessW, pipes, job objects) through `nin-win`. `program` resolved via PATH+PATHEXT (and the script directory); `.cmd/.bat` go
through `cmd.exe /c`; `shell: true` runs a whole command line via `cmd.exe /c`. Args are passed as an argv list with Windows-correct quoting (never re-split).
Redirects: `"stdout"/"stderr"`: `"inherit"|"null"|"capture"|{"file": path, "append": bool}` (stderr also `"stdout"` = merge). `"stdin"`: string data, `"stdin_file"`: path.
Result map (`into`): `{code, ok, stdout, stderr, duration_ms, timed_out, pid}`. Without `into` and without redirects, output goes straight to the console.
`check: true` throws `ProcessError` (with `result`). `timeout` accepts ms integers or duration strings (`"500ms" "30s" "5m" "2h" "1d" "1h30m"`).
Program not found → `ProcessError` with code `"not_found"`.

## 8. Standard library (native modules, `{"use":"std.NAME"}`)

`std.fs` (read_text write_text append_text read_bytes? exists is_file is_dir list_dir mkdir mkdir_all remove remove_all copy rename metadata glob walk temp_file temp_dir
canonicalize cwd chdir), `std.path` (join parent file_name stem ext with_ext is_absolute normalize relative_to), `std.process` (run spawn pid kill list find exit which env
set_env), `std.net` (tcp_connect, http_get/http_request via WinHTTP on Windows (https ok) and plain-HTTP fallback elsewhere, resolve, local_ip, download), `std.sys` (info os arch hostname
username cpu_count memory known_dir uptime), `std.json` (parse stringify pretty), `std.yaml` (parse stringify — YAML 1.2 data), `std.toml` (parse stringify), `std.object`
(keys values entries from_entries merge pick omit get_path set_path deep_equal freeze), `std.task` (spawn sleep all race with_timeout channel? cancel_token), `std.time` (now
now_ms monotonic sleep format parse duration add_days ...), `std.math`, `std.str`, `std.list`, `std.map`, `std.regex`, `std.log` (debug info warn error), `std.io` (read_line stdin_all
prompt confirm), `std.env`, `std.hash` (sha256 md5 crc32 base64 hex encode/decode), `std.random`.

## 9. CLI

```
nin                         interactive shell (REPL)
nin run FILE.nin [ARGS...]  run a script (cwd = script dir; pauses before exit when launched by double-click)
nin check FILE.nin          parse+validate+analyze, print diagnostics, exit 1 on error
nin build FILE.nin [-o OUT.exe] [--icon FILE.ico]   compile to a self-contained executable (bundle: validated program + imports embedded in a copy of the runtime)
nin fmt FILE.nin [--write]  canonical pretty-printed JSON
nin eval "EXPR"             evaluate an expression
nin workflow run|list|graph FILE.nin [--job ID] [--input k=v]... [--report out.json] [--dry-run] [--max-parallel N]
nin pkg install|remove|update|search|list|info|pack|publish ...
nin plugin load|list|call|info ...
nin schedule add|list|remove|enable|disable|run|daemon|register|unregister ...
nin assoc install|uninstall|status
nin version | nin help [CMD]
```
Exit codes: 0 ok, 1 runtime/script failure, 2 usage error, 3 validation/semantic errors, 130 cancelled.
Config: `%APPDATA%\nin\config.toml` (`[shell] prompt history_size color`, `[run] pause_on_exit = "auto|always|never"`, `[paths] packages plugins`,
`[registry] sources = [...]`, `[workflow] max_parallel`). Windows: `NO_COLOR` respected; VT enabled at startup; UTF-8 code page.

REPL line forms: a one-line JSON statement `{"print":"hi"}`; an expression (`1 + 2`, value echoed); multi-line input continues while braces/brackets are
unbalanced; `:help :load FILE :vars :funcs :classes :reset :cd DIR :history :quit`; `!command args` runs an external program.

## 10. Packages & plugins

`.nnpkg` container + `nin-package.toml` manifest (see crates/nin-pkg docs); imported via `{"package":"name"}` (entry file from the manifest).
Plugins: DLL with C ABI (`nin_plugin_entry`), functions take/return JSON; loaded via `nin plugin load` or `{"use":"plugin:path/or/name"}`
(imports of kind Std with a `plugin:` prefix; alias exposes the functions as `alias.fn(args)`).
