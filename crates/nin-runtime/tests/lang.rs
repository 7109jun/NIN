use nin_runtime::*;
use std::sync::{Arc, Mutex};

struct Cap(Mutex<String>);
impl Sink for Cap {
    fn write(&self, t: &str, e: bool) {
        self.0.lock().unwrap().push_str(&if e { format!("[err]{}", t) } else { t.to_string() });
    }
}

/// Run a program, return (stdout, Result<(), rendered error>).
fn run(src: &str) -> (String, Result<(), String>) {
    let src = src.to_string();
    std::thread::Builder::new().stack_size(512 << 20).spawn(move || run_inner(&src)).unwrap().join().unwrap()
}

fn run_inner(src: &str) -> (String, Result<(), String>) {
    let interp = InterpBuilder::new().build();
    let mut ex = Exec::new(interp.clone());
    let cap = Arc::new(Cap(Mutex::new(String::new())));
    ex.sink = Some(cap.clone());
    let prog = match ex.parse_source("test.nin", src) {
        Ok(p) => Arc::new(p),
        Err(e) => return (String::new(), Err(e)),
    };
    let r = ex.run_program(&prog, std::path::Path::new("."), "test");
    let out = cap.0.lock().unwrap().clone();
    match r {
        Ok(_) => (out, Ok(())),
        Err(Signal::Exit(c)) => (out, Err(format!("exit {}", c))),
        Err(Signal::Throw(t)) => (out, Err(interp.render_throw(&t))),
    }
}

fn ok(src: &str) -> String {
    let (o, r) = run(src);
    if let Err(e) = r {
        panic!("unexpected error: {}\n--- output so far:\n{}", e, o);
    }
    o
}
fn err(src: &str) -> String {
    let (_, r) = run(src);
    r.expect_err("expected an error")
}

fn main_prog(body: &str) -> String {
    format!(r#"{{"nin":"1","main":{}}}"#, body)
}

#[test]
fn hello_and_arith() {
    assert_eq!(ok(&main_prog(r#"[{"print":"hi"},{"print":"${1+2*3}"},{"print":"${7/2}"},{"print":"${6/3}"},{"print":"${2**10}"}]"#)), "hi\n7\n3.5\n2\n1024\n");
}

#[test]
fn variables_loops() {
    let o = ok(&main_prog(r#"[
      {"let":"s","value":0},
      {"for":"i","in":"range(5)","do":[{"set":"s","value":"${s+i}"}]},
      {"print":"${s}"},
      {"let":"n","value":3},
      {"while":"n > 0","do":[{"set":"n","op":"-","value":1}]},
      {"print":"${n}"},
      {"for":["k","v"],"in":"{a:1,b:2}","do":[{"print":"${k}=${v}"}]}
    ]"#));
    assert_eq!(o, "10\n0\na=1\nb=2\n");
}

#[test]
fn functions_recursion_defaults() {
    let src = r#"{"nin":"1","functions":{
      "fib":{"params":["n: int"],"returns":"int","body":[{"if":"n < 2","then":[{"return":"${n}"}]},{"return":"${fib(n-1)+fib(n-2)}"}]},
      "greet":{"params":["name: string","greeting: string = 'Hello'"],"body":[{"return":"${greeting}, ${name}!"}]},
      "sum":{"params":["...xs"],"body":[{"return":"${xs.sum()}"}]}
    },"main":[{"print":"${fib(15)}"},{"print":"${greet('Bob')}"},{"print":"${greet('Bob','Hi')}"},{"print":"${sum(1,2,3)}"}]}"#;
    assert_eq!(ok(src), "610\nHello, Bob!\nHi, Bob!\n6\n");
}

#[test]
fn type_errors_and_messages() {
    let src = r#"{"nin":"1","functions":{"f":{"params":["x: int"],"body":[]}},"main":[{"call":"f('a')"}]}"#;
    let e = err(src);
    assert!(e.contains("TypeError") && e.contains("expected int, got string"), "{}", e);
    let e = err(&main_prog(r#"[{"print":"${undefined_thing}"}]"#));
    assert!(e.contains("NameError") && e.contains("undefined variable"), "{}", e);
    let e = err(&main_prog(r#"[{"print":"${1/0}"}]"#));
    assert!(e.contains("ArithmeticError"), "{}", e);
    let e = err(&main_prog(r#"[{"let":"xs","value":[1]},{"print":"${xs[5]}"}]"#));
    assert!(e.contains("IndexError"), "{}", e);
}

#[test]
fn try_catch_finally() {
    let o = ok(&main_prog(r#"[
      {"try":[{"throw":"${new ValueError('bad')}"}],"catch":[{"print":"caught ${e.message} ${e.code}"}],"as":"e","type":"ValueError","finally":[{"print":"fin"}]},
      {"try":[{"print":"${1/0}"}],"catches":[{"type":"TypeError","as":"e","do":[{"print":"no"}]},{"type":"Error","as":"e","do":[{"print":"arith: ${e.message}"}]}]}
    ]"#));
    assert_eq!(o, "caught bad valueerror\nfin\narith: division by zero\n");
}

const SHAPES: &str = r#"{"nin":"1",
 "interfaces":{"Shape":{"methods":{"area":{"returns":"float"},"name":{"returns":"string"}}}},
 "classes":{
  "Base":{"abstract":true,"implements":["Shape"],
    "fields":{"label":{"type":"string","default":"shape","access":"protected"}},
    "methods":{"name":{"returns":"string","body":[{"return":"${this.label}"}]},
               "describe":{"body":[{"return":"${this.name()} area=${this.area()}"}]}}},
  "Circle":{"extends":"Base",
    "fields":{"r":{"type":"float","access":"private"}},
    "constructors":[{"params":["r: float"],"body":[{"super":[]},{"set":"this.r","value":"${r}"},{"set":"this.label","value":"circle"}]}],
    "methods":{"area":{"returns":"float","body":[{"return":"${this.r * this.r * 3}"}]},
               "radius":{"body":[{"return":"${this.r}"}]}}},
  "Rect":{"extends":"Base",
    "fields":{"w":"float","h":"float"},
    "constructors":[{"params":["w: float","h: float"],"body":[{"set":"this.w","value":"${w}"},{"set":"this.h","value":"${h}"}]}],
    "methods":{"area":{"returns":"float","body":[{"return":"${this.w * this.h}"}]},
               "name":{"override":true,"returns":"string","body":[{"return":"rect:${super.name()}"}]}}}
 },
 "main":MAIN}"#;

fn shapes(main: &str) -> String {
    SHAPES.replace("MAIN", main)
}

#[test]
fn inheritance_polymorphism() {
    let o = ok(&shapes(r#"[
      {"let":"xs","value":"${[new Circle(2), new Rect(2,3)]}"},
      {"for":"s","in":"xs","do":[{"print":"${s.describe()}"}]},
      {"print":"${xs[0] is Shape} ${xs[0] is Rect} ${xs[1] is Base}"}
    ]"#));
    assert_eq!(o, "circle area=12.0\nrect:shape area=6.0\ntrue false true\n");
}

#[test]
fn encapsulation() {
    let e = err(&shapes(r#"[{"let":"c","value":"${new Circle(1)}"},{"print":"${c.r}"}]"#));
    assert!(e.contains("AccessError") && e.contains("private"), "{}", e);
    let e = err(&shapes(r#"[{"let":"c","value":"${new Circle(1)}"},{"print":"${c.label}"}]"#));
    assert!(e.contains("AccessError") && e.contains("protected"), "{}", e);
    let o = ok(&shapes(r#"[{"let":"c","value":"${new Circle(3)}"},{"print":"${c.radius()}"}]"#));
    assert_eq!(o, "3.0\n");
}

#[test]
fn abstract_and_interface_instantiation() {
    let e = err(&shapes(r#"[{"call":"new Base()"}]"#));
    assert!(e.contains("abstract"), "{}", e);
    let e = err(&shapes(r#"[{"call":"new Shape()"}]"#));
    assert!(e.contains("interface"), "{}", e);
}

#[test]
fn overloads_and_generics() {
    let src = r#"{"nin":"1","classes":{
      "Printer":{"methods":{"show":[
        {"params":["x: int"],"body":[{"return":"int:${x}"}]},
        {"params":["x: string"],"body":[{"return":"str:${x}"}]},
        {"params":["x: float"],"body":[{"return":"float:${x}"}]},
        {"params":["x: any"],"body":[{"return":"any"}]},
        {"params":["x: int","y: int"],"body":[{"return":"two"}]}
      ]}},
      "Box":{"generics":["T"],"fields":{"v":"T"},
        "constructors":[{"params":["v: T"],"body":[{"set":"this.v","value":"${v}"}]}],
        "methods":{"get":{"returns":"T","body":[{"return":"${this.v}"}]},"set":{"params":["v: T"],"body":[{"set":"this.v","value":"${v}"}]}}}
    },"main":[
      {"let":"p","value":"${new Printer()}"},
      {"print":"${p.show(1)} ${p.show('a')} ${p.show(1.5)} ${p.show(true)} ${p.show(1,2)}"},
      {"let":"b","value":"${new Box<int>(5)}"},
      {"print":"${b.get()} ${reflect.type_args(b)}"},
      {"try":[{"call":"b.set('x')"}],"catch":[{"print":"${e.message}"}],"as":"e","type":"TypeError"},
      {"let":"c","value":"${new Box('s')}"},
      {"print":"${reflect.type_args(c)}"}
    ]}"#;
    let o = ok(src);
    assert!(o.starts_with("int:1 str:a float:1.5 any two\n5 [\"int\"]\n"), "{}", o);
    assert!(o.contains("expected int, got string"), "{}", o);
    assert!(o.trim_end().ends_with("[\"string\"]"), "{}", o);
}

#[test]
fn enums_statics_equals_tostring() {
    let src = r#"{"nin":"1","enums":{"Color":{"Red":1,"Green":2}},
      "classes":{"P":{"fields":{"x":"int","count":{"type":"int","default":0,"static":true}},
        "constructors":[{"params":["x: int"],"body":[{"set":"this.x","value":"${x}"},{"set":"P.count","op":"+","value":1}]}],
        "methods":{"to_string":{"body":[{"return":"P(${this.x})"}]},
                   "equals":{"params":["o: any"],"body":[{"return":"${o is P && o.x == this.x}"}]},
                   "make":{"static":true,"params":["x: int"],"body":[{"return":"${new P(x)}"}]}}}},
      "main":[
        {"print":"${Color.Red.name} ${Color.Green.value} ${Color.Green.ordinal} ${Color.values().len()}"},
        {"print":"${Color.from_name('Green') == Color.Green}"},
        {"let":"a","value":"${P.make(1)}"},{"let":"b","value":"${new P(1)}"},
        {"print":"${a} ${a == b} ${P.count} ${str([a,b])}"}
      ]}"#;
    assert_eq!(ok(src), "Red 2 1 2\ntrue\nP(1) true 2 [P(1), P(1)]\n");
}

#[test]
fn reflection() {
    let o = ok(&shapes(r#"[
      {"let":"c","value":"${new Circle(2)}"},
      {"print":"${reflect.class_name(c)} ${reflect.parent(reflect.class_of(c)).name}"},
      {"print":"${reflect.fields(c).map(fn(f) => f.name)}"},
      {"print":"${reflect.methods('Circle').map(fn(m) => m.name).contains('area')}"},
      {"print":"${reflect.invoke(c,'area')} ${reflect.is_a(c,'Shape')}"},
      {"let":"d","value":"${reflect.new('Rect', 2, 2)}"},
      {"print":"${d.area()}"},
      {"try":[{"call":"reflect.get(c,'r')"}],"catch":[{"print":"denied"}],"as":"e","type":"AccessError"}
    ]"#));
    assert_eq!(o, "Circle Base\n[\"label\", \"r\"]\ntrue\n12.0 true\n4.0\ndenied\n");
}

#[test]
fn builtin_methods() {
    let o = ok(&main_prog(r#"[
      {"print":"${'a,b,c'.split(',')} ${' x '.trim().upper()} ${'abc'.pad_left(5,'-')} ${'Hi {}!'.format('you')}"},
      {"print":"${[3,1,2].sort()} ${[1,2,3].map(fn(x)=>x*2).filter(fn(x)=>x>2)} ${[1,2,3].reduce(fn(a,b)=>a+b, 10)}"},
      {"print":"${{a:1,b:2}.items()} ${{a:1}.merge({b:2})} ${'héllo'[1:3]} ${[1,2,3,4][1:3]} ${[1,2,3][-1]}"},
      {"print":"${[[1,2],[3]].flat()} ${['b','a'].sort_by(fn(x)=>x)} ${keys({x:1})}"},
      {"print":"${null ?? 'dflt'} ${(null)?.foo} ${3 in [1,2,3]} ${'a' in {a:1}}"}
    ]"#));
    assert_eq!(o, "[\"a\", \"b\", \"c\"] X --abc Hi you!\n[1, 2, 3] [4, 6] 16\n[[\"a\", 1], [\"b\", 2]] {\"a\": 1, \"b\": 2} él [2, 3] 3\n[1, 2, 3] [\"a\", \"b\"] [\"x\"]\ndflt null true true\n");
}

#[test]
fn closures_and_lambdas() {
    let o = ok(&main_prog(r#"[
      {"let":"mk","value":"${fn(n) => fn(x) => x + n}"},
      {"let":"add5","value":"${mk(5)}"},
      {"print":"${add5(1)}"},
      {"let":"counter","value":0},
      {"let":"inc","value":"${fn() => 1}"},
      {"print":"${inc()}"}
    ]"#));
    assert_eq!(o, "6\n1\n");
}

#[test]
fn match_assert_exit() {
    let o = ok(&main_prog(r#"[
      {"let":"x","value":2},
      {"match":"x","cases":[{"when":1,"do":[{"print":"one"}]},{"when":[2,3],"do":[{"print":"two-three"}]}],"default":[{"print":"other"}]},
      {"assert":"x == 2"}
    ]"#));
    assert_eq!(o, "two-three\n");
    let e = err(&main_prog(r#"[{"assert":"1 == 2","message":"nope"}]"#));
    assert!(e.contains("AssertionError") && e.contains("nope"), "{}", e);
    let (o, r) = run(&main_prog(r#"[{"print":"a"},{"exit":3},{"print":"b"}]"#));
    assert_eq!(o, "a\n");
    assert_eq!(r.unwrap_err(), "exit 3");
}

#[test]
fn async_spawn_parallel() {
    let src = r#"{"nin":"1","functions":{
        "slow":{"async":true,"params":["n: int"],"body":[{"return":"${n * 2}"}]}
      },"main":[
        {"let":"t","value":"${slow(21)}"},
        {"print":"${await t}"},
        {"let":"ts","value":"${[slow(1), slow(2), slow(3)]}"},
        {"print":"${await ts}"},
        {"spawn":[{"return":"${5}"}],"as":"job"},
        {"await":"job","as":"r"},
        {"print":"${r}"},
        {"let":"acc","value":[]},
        {"parallel":[[{"call":"acc.push(1)"}],[{"call":"acc.push(2)"}],[{"call":"acc.push(3)"}]],"max":2},
        {"print":"${acc.sort()}"}
      ]}"#;
    assert_eq!(ok(src), "42\n[2, 4, 6]\n5\n[1, 2, 3]\n");
}

#[test]
fn parallel_failure_propagates() {
    let e = err(&main_prog(r#"[{"parallel":[[{"throw":"${new StateError('boom')}"}],[{"print":"x"}]]}]"#));
    assert!(e.contains("StateError") && e.contains("boom"), "{}", e);
}

#[test]
fn process_run() {
    let cmd = if cfg!(windows) { r#"{"run":"cmd","args":["/c","echo hello"],"into":"r"}"# } else { r#"{"run":"echo","args":["hello"],"into":"r"}"# };
    let o = ok(&main_prog(&format!(r#"[{},{{"print":"${{r.code}} ${{r.stdout.trim()}} ${{r.ok}}"}}]"#, cmd)));
    assert_eq!(o, "0 hello true\n");
}

#[test]
fn process_check_and_missing() {
    if cfg!(windows) { return; }
    let e = err(&main_prog(r#"[{"run":"sh","args":["-c","exit 3"],"check":true}]"#));
    assert!(e.contains("ProcessError") && e.contains("code 3"), "{}", e);
    let e = err(&main_prog(r#"[{"run":"definitely-not-a-program-xyz"}]"#));
    assert!(e.contains("ProcessError"), "{}", e);
}

#[test]
fn process_timeout() {
    if cfg!(windows) { return; }
    let o = ok(&main_prog(r#"[{"run":"sleep","args":["5"],"timeout":"200ms","into":"r"},{"print":"${r.timed_out}"}]"#));
    assert_eq!(o, "true\n");
}

#[test]
fn stack_overflow_is_catchable() {
    let src = r#"{"nin":"1","functions":{"f":{"params":["n"],"body":[{"return":"${f(n+1)}"}]}},
      "main":[{"try":[{"call":"f(0)"}],"catch":[{"print":"${e.message}"}],"as":"e","type":"StateError"}]}"#;
    let o = ok(src);
    assert!(o.contains("stack overflow"), "{}", o);
}

#[test]
fn error_stack_trace_rendering() {
    let src = r#"{"nin":"1","functions":{"a":{"body":[{"call":"b()"}]},"b":{"body":[{"throw":"${new Error('deep')}"}]}},"main":[{"call":"a()"}]}"#;
    let e = err(src);
    assert!(e.contains("Error: deep") && e.contains("at b") && e.contains("at a"), "{}", e);
}

#[test]
fn const_assignment_and_scope() {
    let e = err(&main_prog(r#"[{"const":"k","value":1},{"set":"k","value":2}]"#));
    assert!(e.contains("constant"), "{}", e);
    let e = err(&main_prog(r#"[{"set":"nope","value":2}]"#));
    assert!(e.contains("undeclared"), "{}", e);
    let o = ok(&main_prog(r#"[{"let":"x","value":1},{"block":[{"let":"x","value":2},{"print":"${x}"}]},{"print":"${x}"}]"#));
    assert_eq!(o, "2\n1\n");
}

#[test]
fn json_roundtrip_values() {
    let j = serde_json::json!({"a":[1,2.5,"x",null,true],"b":{"c":1}});
    let v = nin_runtime::builtins::json_to_value(&j);
    let interp = InterpBuilder::new().build();
    let _ex = Exec::new(interp);
    let back = nin_runtime::builtins::value_to_json(&v).ok().unwrap();
    assert_eq!(j, back);
}

#[test]
fn file_imports() {
    let dir = std::env::temp_dir().join(format!("nin-imp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("util.nin"), r#"{"nin":"1","const":{"PI":3},"classes":{"Box":{"fields":{"v":"int"},"constructors":[{"params":["v: int"],"body":[{"set":"this.v","value":"${v}"}]}]}},"functions":{"dbl":{"params":["x"],"body":[{"return":"${x*2}"}]}},"main":[{"print":"never"}]}"#).unwrap();
    let main = dir.join("main.nin");
    std::fs::write(&main, r#"{"nin":"1","imports":[{"file":"./util.nin","as":"u"}],"main":[{"print":"${u.dbl(4)} ${u.PI} ${new u.Box(7).v}"}]}"#).unwrap();
    let interp = InterpBuilder::new().build();
    let mut ex = Exec::new(interp.clone());
    let cap = Arc::new(Cap(Mutex::new(String::new())));
    ex.sink = Some(cap.clone());
    let text = std::fs::read_to_string(&main).unwrap();
    let p = Arc::new(ex.parse_source(&main.display().to_string(), &text).unwrap());
    ex.run_program(&p, &dir, "main").ok().unwrap();
    assert_eq!(cap.0.lock().unwrap().as_str(), "8 3 7\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn circular_import_detected() {
    let dir = std::env::temp_dir().join(format!("nin-cyc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.nin"), r#"{"nin":"1","imports":[{"file":"./b.nin"}]}"#).unwrap();
    std::fs::write(dir.join("b.nin"), r#"{"nin":"1","imports":[{"file":"./a.nin"}]}"#).unwrap();
    let interp = InterpBuilder::new().build();
    let mut ex = Exec::new(interp.clone());
    let a = dir.join("a.nin");
    let text = std::fs::read_to_string(&a).unwrap();
    let p = Arc::new(ex.parse_source(&a.display().to_string(), &text).unwrap());
    let r = ex.load_program(&p, &dir, "a");
    match r {
        Err(Signal::Throw(t)) => assert!(interp.render_throw(&t).contains("circular import")),
        _ => panic!("expected circular import error"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
