# Learn NIN

NIN Shell is a Windows x64 shell and scripting language. Programs are **JSON documents** with the `.nin` extension
(`//` comments and trailing commas are allowed). This tutorial walks from "hello" to workflows, in the order you will need things.

> Everything here was run against NIN Shell 0.1.0. Process examples (`cmd`) need Windows.

## 1. Run something

```
nin                       # interactive shell (REPL)
nin run hello.nin         # run a script (working directory = the script's folder)
nin hello.nin             # same thing; also what double-clicking a .nin file does
nin check hello.nin       # validate + analyze without running
nin eval "1 + 2 * 3"      # evaluate one expression
```

`hello.nin`:

```json
{
  "nin": "1",
  "main": [
    {"print": "Hello, NIN!"}
  ]
}
```

`"nin": "1"` is the only required key. `main` is the program body: an array of **statements**.
Each statement is an object with one *head key* (`print`, `let`, `if`, …) plus modifier keys.

Double-click support: run `nin assoc install` once. After that, double-clicking a `.nin` file runs it in its own folder
and keeps the console open until you press a key.

## 2. Values and `${}`

Types: `null`, `bool`, `int` (64-bit), `float`, `string`, `list`, `map` (ordered, string keys), objects, functions, tasks.

Inside a **value position** (`value`, `print`, `return`, `args`, …) a string may contain `${expression}`:

```json
{"let": "n", "value": 4},
{"print": "n squared is ${n * n}"},         // string with interpolation
{"let": "xs", "value": "${[3, 1, 2].sort()}"} // exactly one ${...} keeps the real type (a list)
```

A string that is *exactly one* `${...}` yields the typed value; anything else yields a string. Write `$${` for a literal `${`.
In **expression positions** (`if`, `while`, `for … in`, `match`, `call`, `set` target) the string *is* the expression: `"if": "n > 3 && ready"`.

Operators: `+ - * / % **`, `== != < <= > >=`, `&& || !` (also `and or not`), `??` (null-coalesce), `?:`, `in`, `is`, `as`, `?.`, indexing `a[i]`, slicing `a[1:3]`.
`6 / 3` is the int `2`, `7 / 2` is `3.5`. Conditions must be `bool` or `null`; anything else is a `TypeError`.

## 3. Variables and control flow

```json
{"let": "total", "value": 0},
{"const": "limit", "value": 5},
{"for": "i", "in": "range(limit)", "do": [
  {"set": "total", "op": "+", "value": "${i}"}
]},
{"while": "total > 0", "do": [{"set": "total", "op": "-", "value": 4}]},
{"if": "total < 0", "then": [{"print": "negative"}], "else": [{"print": "ok"}]},
{"for": ["k", "v"], "in": "{a: 1, b: 2}", "do": [{"print": "${k}=${v}"}]},
{"match": "total", "cases": [
  {"when": 0, "do": [{"print": "zero"}]},
  {"when": [1, 2], "do": [{"print": "small"}]}
], "default": [{"print": "other"}]}
```

`break` / `continue` work inside loops. `set` can target `x`, `a.b`, `xs[0]`, `this.n`, with an optional `"op"`.
Lists and maps have many builtin methods (`map filter reduce sort join keys values …`); lambdas look like `fn(n) => n * n`.

## 4. Functions

```json
"functions": {
  "add":  {"params": ["a: int", "b: int = 1"], "returns": "int", "body": [{"return": "${a + b}"}]},
  "fact": {"params": ["n: int"], "returns": "int", "body": [
    {"if": "n <= 1", "then": [{"return": 1}]},
    {"return": "${n * fact(n - 1)}"}
  ]},
  "sum":  {"params": ["...xs"], "body": [{"return": "${xs.sum()}"}]}
}
```

Parameters are `"name"`, `"name: Type"`, `"name: Type = default"` or `"...rest"`. Typing is gradual: leave a type off and it is `any`.
`"async": true` functions return a task; use `await`.

## 5. Errors

```json
{"try": [{"throw": "${new ValueError('bad input')}"}],
 "catch": [{"print": "caught: ${err.message}"}],
 "as": "err", "type": "ValueError",
 "finally": [{"print": "cleanup"}]}
```

Built-in hierarchy: `Error` ← `TypeError ValueError KeyError IndexError ArithmeticError AssertionError IOError ProcessError TimeoutError CancelledError StateError AccessError ImportError NameError`.
Uncaught errors print a diagnostic with the NIN call stack and exit with code 1. `{"assert": "x > 0", "message": "…"}` throws `AssertionError`.

## 6. Classes and objects

```json
"interfaces": {"Speaker": {"methods": {"speak": {"returns": "string"}}}},
"classes": {
  "Animal": {
    "abstract": true, "implements": ["Speaker"],
    "fields": {"name": {"type": "string", "access": "private"}},
    "constructors": [{"params": ["name: string"], "body": [{"set": "this.name", "value": "${name}"}]}],
    "methods": {
      "speak": {"abstract": true, "returns": "string"},
      "intro": {"returns": "string", "body": [{"return": "${this.name} says ${this.speak()}"}]}
    }
  },
  "Dog": {
    "extends": "Animal",
    "constructors": [{"params": ["n: string"], "body": [{"super": ["${n}"]}]}],
    "methods": {"speak": {"override": true, "returns": "string", "body": [{"return": "woof"}]}}
  }
},
"main": [
  {"let": "d", "value": "${new Dog('Rex')}"},
  {"print": "${d.intro()}"}          // Rex says woof
]
```

* **Encapsulation**: `access` is `public` (default), `protected` or `private`. Violations are `AccessError` (and a static error where the checker can tell).
* **Inheritance / polymorphism**: single `extends`, many `implements`; `override` is required when redefining a concrete inherited method; `super.m()` calls the parent.
* **Abstract classes / interfaces** cannot be instantiated; concrete subclasses must implement every abstract method.
* **Overloading**: a method may be an *array* of overloads, chosen by arity then parameter types.
* **Generics**: `"generics": ["T"]`, then `new Box<int>(7)`. Type arguments are kept at runtime.
* **Reflection**: `reflect.class_name(x)`, `reflect.fields(x)`, `reflect.methods(x)`, `reflect.invoke(obj, "m", …)`, …
* **Enums**: `"enums": {"Color": ["Red", "Green"]}` → `Color.Red.name`, `Color.Green.ordinal`, `Color.values()`.

## 7. Modules and the standard library

```json
"imports": [
  {"use": "std.fs"},                 // native standard module → fs.read_text(...)
  {"use": "std.json", "as": "j"},    // custom alias
  {"file": "./util.nin"},            // another .nin file → util.twice(4)
  {"package": "mypkg"}               // an installed .nnpkg
]
```

A file module exposes its functions, classes, enums and globals as members of its alias; its own `main` never runs.
Std modules: `fs path process net sys json yaml toml object task time math str list map regex log io env hash random`.

## 8. Running other programs

```json
{"run": "git", "args": ["status", "--short"], "into": "r", "timeout": "30s"},
{"print": "exit ${r.code}, output: ${r.stdout}"},
{"run": "cmd", "args": ["/c", "dir"], "stdout": {"file": "out.txt", "append": false}},
{"run": "build.exe", "check": true}      // throws ProcessError on failure
```

Arguments are passed as a real argv list (never re-split). `into` captures `{code, ok, stdout, stderr, duration_ms, timed_out, pid}`.
Without `into` or redirects, output goes straight to the console.

## 9. Concurrency

```json
{"parallel": [[{"print": "left"}], [{"print": "right"}]], "max": 2},
{"spawn": [{"print": "background"}], "as": "job"},
{"await": "job"}
```

`parallel` runs branches at the same time and joins them. `spawn` returns a task handle; `await` waits and rethrows errors.
Ctrl+C cancels running work with `CancelledError`.

## 10. Workflows

A workflow is a graph of **jobs**, each a list of **steps**. Jobs with no unmet `needs` start automatically, in parallel.

```json
{
  "nin": "1",
  "workflow": {
    "name": "ci",
    "inputs": {"profile": {"type": "string", "default": "debug"}},
    "jobs": {
      "lint": {"steps": [{"run": "cmd", "args": ["/c", "echo", "lint ok"]}]},
      "test": {
        "strategy": {"matrix": {"os": ["a", "b"]}},
        "retries": 1, "timeout": "5m",
        "steps": [{"script": [{"print": "testing ${matrix.os} (${inputs.profile})"}]}]
      },
      "pack": {
        "needs": ["lint", "test"],
        "steps": [{"id": "p", "script": [{"output": "file", "value": "app.zip"}]}],
        "outputs": {"artifact": "${steps.p.outputs.file}"},
        "finally": [{"script": [{"print": "done"}]}]
      }
    }
  }
}
```

```
nin workflow list ci.nin
nin workflow graph ci.nin
nin workflow run ci.nin --input profile=release --max-parallel 4 --report report.json
nin workflow run ci.nin --job pack        # a job plus its dependencies
nin workflow run ci.nin --dry-run
```

Steps are `run` (a process), `script` (NIN statements) or `uses` (call a function with `with` arguments). Each can have `if`
(`success()`, `failure()`, `always()`, `cancelled()`), `timeout`, `retries`, `retry_delay`, `continue_on_error` and `on_error`.
Jobs can also set `limits` (`memory`, `processes`, `cpu_time`) enforced with a Windows Job Object. The run exits 0 only if the workflow succeeded.

## 11. Packages, plugins, schedules

```
nin pkg pack ./mypkg .                 # build mypkg-1.0.0.nnpkg
nin pkg install mypkg-1.0.0.nnpkg      # or: nin pkg install name[@version]
nin pkg list | info NAME | update | remove NAME | search TEXT
nin plugin info hello.dll              # inspect a native plugin
nin schedule add nightly "daily 02:30" backup.nin
nin schedule list | run ID | daemon | register ID     # register → Windows Task Scheduler
```

Plugin and package authoring: see *Module Development Method*. Every command and key: see *Reference*.

## 12. Compile to an executable

```
nin build app.nin -o app.exe
```

The result is a standalone `.exe` containing the runtime plus your validated program and every file/package it imports.

## 13. The REPL

Type an expression to see its value (`_` holds the last result), a one-line JSON statement (`{"let":"a","value":5}`),
a section such as `{"functions":{…}}` to define things, `!dir` to run an external command, or a colon command:
`:help :vars :funcs :classes :load FILE :reset :cd DIR :history :quit`.
Input continues on the next line while brackets are unbalanced.
