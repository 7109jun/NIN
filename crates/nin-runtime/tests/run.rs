use nin_runtime::*;
use std::path::Path;
use std::sync::{Arc, Mutex};

struct Cap(Mutex<String>);
impl Sink for Cap {
    fn write(&self, t: &str, e: bool) {
        let mut g = self.0.lock().unwrap();
        if e {
            g.push_str("[err]");
        }
        g.push_str(t);
    }
}

/// Run a program; `Ok(stdout)` or `Err(rendered error)`.
fn run(src: &str) -> Result<String, String> {
    let interp = InterpBuilder::new().build();
    let mut ex = Exec::new(interp.clone());
    let cap = Arc::new(Cap(Mutex::new(String::new())));
    ex.sink = Some(cap.clone());
    let prog = ex.parse_source("test.nin", src)?;
    match ex.run_program(&prog, Path::new("."), "test") {
        Ok(_) => Ok(cap.0.lock().unwrap().clone()),
        Err(Signal::Throw(t)) => Err(format!("{}", interp.render_throw(&t))),
        Err(Signal::Exit(c)) => Err(format!("exit {}", c)),
    }
}

fn ok(src: &str) -> String {
    run(src).unwrap_or_else(|e| panic!("unexpected failure:\n{}", e))
}

fn fails(src: &str) -> String {
    match run(src) {
        Ok(o) => panic!("expected failure, got output {:?}", o),
        Err(e) => e,
    }
}

fn main_of(body: &str) -> String {
    format!(r#"{{"nin":"1","main":{}}}"#, body)
}

#[test]
fn hello_and_arith() {
    assert_eq!(ok(&main_of(r#"[{"print":"hi ${1+2*3}"},{"print":"${7/2}"},{"print":"${6/3}"},{"print":"${2**10}"},{"print":"${7%3}"}]"#)), "hi 7\n3.5\n2\n1024\n1\n");
}

#[test]
fn variables_and_control_flow() {
    let s = main_of(r#"[
      {"let":"s","value":0},
      {"for":"i","in":"range(5)","do":[{"set":"s","value":"${s+i}"}]},
      {"print":"${s}"},
      {"let":"n","value":3},
      {"while":"n > 0","do":[{"set":"n","op":"-","value":1}]},
      {"print":"${n}"},
      {"if":"s == 10","then":[{"print":"ten"}],"else":[{"print":"no"}]},
      {"match":"s","cases":[{"when":[1,2],"do":[{"print":"low"}]},{"when":10,"do":[{"print":"hit"}]}],"default":[{"print":"def"}]}
    ]"#);
    assert_eq!(ok(&s), "10\n0\nten\nhit\n");
}

#[test]
fn functions_recursion_closures() {
    let s = r#"{"nin":"1",
      "functions":{"fib":{"params":["n: int"],"returns":"int","body":[{"if":"n < 2","then":[{"return":"${n}"}]},{"return":"${fib(n-1)+fib(n-2)}"}]},
                   "mk":{"params":["k"],"body":[{"return":"${fn(x) => x + k}"}]}},
      "main":[{"print":"${fib(15)}"},{"let":"f","value":"${mk(10)}"},{"print":"${f(5)}"}]}"#;
    assert_eq!(ok(s), "610\n15\n");
}

#[test]
fn type_errors_and_messages() {
    let s = r#"{"nin":"1","functions":{"f":{"params":["a: int"],"body":[]}},"main":[{"call":"f('x')"}]}"#;
    let e = fails(s);
    assert!(e.contains("TypeError"), "{}", e);
    assert!(e.contains("expected int, got string"), "{}", e);
}

#[test]
fn try_catch_finally() {
    let s = main_of(r#"[
      {"try":[{"throw":"${new ValueError('bad')}"}],"catches":[{"type":"KeyError","do":[{"print":"key"}]},{"type":"Error","as":"e","do":[{"print":"${e.message} ${e.code}"}]}],"finally":[{"print":"fin"}]},
      {"try":[{"call":"1/0"}],"catch":[{"print":"caught"}],"as":"e"},
      {"try":[{"call":"[1,2][5]"}],"catches":[{"type":"IndexError","as":"e","do":[{"print":"${e.message}"}]}]}
    ]"#);
    assert_eq!(ok(&s), "bad valueerror\nfin\ncaught\nlist index 5 out of range (length 2)\n");
}

#[test]
fn uncaught_error_has_stack() {
    let s = r#"{"nin":"1","functions":{"a":{"body":[{"call":"b()"}]},"b":{"body":[{"throw":"${new StateError('boom')}"}]}},"main":[{"call":"a()"}]}"#;
    let e = fails(s);
    assert!(e.contains("StateError: boom"), "{}", e);
    assert!(e.contains("at b"), "{}", e);
    assert!(e.contains("at a"), "{}", e);
}

const SHAPES: &str = r#"{"nin":"1",
 "interfaces":{"Shape":{"methods":{"area":{"returns":"float"}}}},
 "classes":{
  "Base":{"abstract":true,"implements":["Shape"],"fields":{"name":{"type":"string","default":"shape"},"count":{"type":"int","static":true,"default":0}},
    "constructors":[{"params":["name: string"],"body":[{"set":"this.name","value":"${name}"},{"set":"Base.count","op":"+","value":1}]}],
    "methods":{"describe":{"returns":"string","body":[{"return":"${this.name + ' area=' + str(this.area())}"}]},
               "to_string":{"body":[{"return":"${'<' + this.name + '>'}"}]}}},
  "Circle":{"extends":"Base","fields":{"r":"float"},
    "constructors":[{"params":["r: float"],"body":[{"super":["circle"]},{"set":"this.r","value":"${r}"}]}],
    "methods":{"area":{"returns":"float","override":true,"body":[{"return":"${this.r * this.r * 3}"}]}}},
  "Sq":{"extends":"Base","fields":{"s":"float"},
    "constructors":[{"params":["s: float"],"body":[{"super":["sq"]},{"set":"this.s","value":"${s}"}]}],
    "methods":{"area":{"returns":"float","body":[{"return":"${this.s * this.s}"}]}}}
 },
 "main":MAIN}"#;

fn shapes(main: &str) -> String {
    SHAPES.replace("MAIN", main)
}

#[test]
fn inheritance_polymorphism_static() {
    let s = shapes(r#"[
      {"let":"xs","value":"${[new Circle(2), new Sq(3)]}"},
      {"for":"x","in":"xs","do":[{"print":"${x.describe()}"}]},
      {"print":"${Base.count}"},
      {"print":"${xs[0]}"},
      {"print":"${xs[0] is Shape} ${xs[1] is Circle} ${xs[1] is Base}"}
    ]"#);
    assert_eq!(ok(&s), "circle area=12.0\nsq area=9.0\n2\n<circle>\ntrue false true\n");
}

#[test]
fn abstract_and_interface_instantiation_fail() {
    let e = fails(&shapes(r#"[{"call":"new Base('x')"}]"#));
    assert!(e.contains("abstract"), "{}", e);
    let e = fails(&shapes(r#"[{"call":"new Shape()"}]"#));
    assert!(e.contains("interface"), "{}", e);
}

#[test]
fn missing_abstract_impl_is_rejected() {
    let s = r#"{"nin":"1","interfaces":{"I":{"methods":{"go":{}}}},"classes":{"C":{"implements":["I"]}},"main":[]}"#;
    let e = fails(s);
    assert!(e.contains("must implement abstract method `go`"), "{}", e);
}

#[test]
fn encapsulation() {
    let s = r#"{"nin":"1","classes":{
      "Acct":{"fields":{"bal":{"type":"int","access":"private","default":5},"id":{"type":"int","readonly":true,"default":1}},
        "methods":{"get":{"body":[{"return":"${this.bal}"}]},
                   "secret":{"access":"private","body":[{"return":1}]},
                   "peek":{"body":[{"return":"${this.secret()}"}]}}}},
      "main":[{"let":"a","value":"${new Acct()}"},{"print":"${a.get()} ${a.peek()}"},
              {"try":[{"call":"a.bal"}],"catch":[{"print":"${e.code}"}],"as":"e"},
              {"try":[{"set":"a.bal","value":1}],"catch":[{"print":"${e.code}"}],"as":"e"},
              {"try":[{"call":"a.secret()"}],"catch":[{"print":"${e.code}"}],"as":"e"},
              {"try":[{"set":"a.id","value":2}],"catch":[{"print":"${e.code}"}],"as":"e"}]}"#;
    assert_eq!(ok(s), "5 1\naccesserror\naccesserror\naccesserror\nstateerror\n");
}

#[test]
fn overloading() {
    let s = r#"{"nin":"1","classes":{"P":{"methods":{"f":[
        {"params":["x: int"],"body":[{"return":"int"}]},
        {"params":["x: float"],"body":[{"return":"float"}]},
        {"params":["x: string"],"body":[{"return":"string"}]},
        {"params":["x: any"],"body":[{"return":"any"}]},
        {"params":["x: int","y: int"],"body":[{"return":"two"}]}]}}},
      "main":[{"let":"p","value":"${new P()}"},
        {"print":"${p.f(1)} ${p.f(1.5)} ${p.f('a')} ${p.f(true)} ${p.f(1,2)}"},
        {"try":[{"call":"p.f(1,2,3)"}],"catch":[{"print":"arity"}],"as":"e"}]}"#;
    assert_eq!(ok(s), "int float string any two\narity\n");
}

#[test]
fn generics_reified() {
    let s = r#"{"nin":"1","classes":{"Box":{"generics":["T"],"fields":{"v":"T"},
        "constructors":[{"params":["v: T"],"body":[{"set":"this.v","value":"${v}"}]}],
        "methods":{"get":{"returns":"T","body":[{"return":"${this.v}"}]}}}},
      "main":[{"let":"b","value":"${new Box<int>(3)}"},{"print":"${b.get()} ${reflect.type_args(b)}"},
        {"try":[{"call":"new Box<int>('x')"}],"catch":[{"print":"${e.code}"}],"as":"e"},
        {"let":"c","value":"${new Box('s')}"},{"print":"${reflect.type_args(c)}"},
        {"try":[{"set":"b.v","value":"zz"}],"catch":[{"print":"${e.code}"}],"as":"e"}]}"#;
    assert_eq!(ok(s), "3 [\"int\"]\ntypeerror\n[\"string\"]\ntypeerror\n");
}

#[test]
fn inherited_ctor_and_super_method() {
    let s = r#"{"nin":"1","classes":{
      "A":{"fields":{"x":"int"},"constructors":[{"params":["x: int"],"body":[{"set":"this.x","value":"${x}"}]}],"methods":{"hi":{"body":[{"return":"A${this.x}"}]}}},
      "B":{"extends":"A","methods":{"hi":{"override":true,"body":[{"return":"${'B+' + super.hi()}"}]}}}},
      "main":[{"print":"${new B(4).hi()}"}]}"#;
    assert_eq!(ok(s), "B+A4\n");
}

#[test]
fn enums() {
    let s = r#"{"nin":"1","enums":{"Color":["Red","Green"],"Lvl":{"Lo":10,"Hi":20}},
      "main":[{"print":"${Color.Green.name} ${Color.Green.ordinal} ${Lvl.Hi.value}"},
              {"print":"${Color.values().len()} ${Color.from_name('Red') == Color.Red} ${Color.Red == Color.Green}"},
              {"print":"${Color.Red}"}]}"#;
    assert_eq!(ok(s), "Green 1 20\n2 true false\nColor.Red\n");
}

#[test]
fn reflection() {
    let s = shapes(r#"[
      {"let":"c","value":"${new Circle(1)}"},
      {"print":"${reflect.class_name(c)} ${reflect.parent(Circle).name} ${reflect.is_a(c,'Shape')} ${reflect.is_abstract(Base)}"},
      {"print":"${reflect.fields(c).map(fn(f) => f.name)}"},
      {"print":"${reflect.invoke(c,'area')} ${reflect.get(c,'r')}"},
      {"call":"reflect.set(c,'r',2.0)"},{"print":"${c.area()}"},
      {"print":"${reflect.new('Sq', 2.0).area()}"}
    ]"#);
    assert_eq!(ok(&s), "Circle Base true true\n[\"name\", \"count\", \"r\"]\n3.0 1.0\n12.0\n4.0\n");
}

#[test]
fn builtin_methods() {
    let s = main_of(r#"[
      {"print":"${'a,b,c'.split(',').map(fn(x) => x.upper()).join('-')}"},
      {"print":"${[3,1,2].sort()} ${[3,1,2].sort_by(fn(x) => -x)} ${[1,2,3,4].filter(fn(x) => x % 2 == 0)} ${[1,2,3].reduce(fn(a,b) => a+b, 0)}"},
      {"print":"${{a:1,b:2}.keys()} ${{a:1}.get('z', 9)} ${{a:1}.merge({b:2})}"},
      {"print":"${'abc'[1:]} ${[1,2,3][-1]} ${'x'.pad_left(3, '.')} ${'{} and {}'.format(1,'t')}"},
      {"print":"${len('héllo')} ${typeof(1.5)} ${int('42') + 1} ${'abc'.find('c')}"}
    ]"#);
    assert_eq!(ok(&s), "A-B-C\n[1, 2, 3] [3, 2, 1] [2, 4] 6\n[\"a\", \"b\"] 9 {\"a\": 1, \"b\": 2}\nbc 3 ..x 1 and t\n5 float 43 2\n");
}

#[test]
fn null_handling_and_optional_chain() {
    let s = main_of(r#"[
      {"let":"m","value":{"a":null}},
      {"print":"${m.a ?? 'dflt'} ${m.zz ?? 'none'} ${m?.a?.b}"},
      {"try":[{"call":"m['zz']"}],"catch":[{"print":"${e.code}"}],"as":"e"},
      {"try":[{"call":"if_null.x"}],"catch":[{"print":"${e.code}"}],"as":"e"}
    ]"#);
    assert_eq!(ok(&s), "dflt none null\nkeyerror\nnameerror\n");
}

#[test]
fn async_spawn_await_parallel() {
    let s = r#"{"nin":"1","functions":{"sq":{"async":true,"params":["x: int"],"body":[{"return":"${x*x}"}]}},
      "main":[{"let":"t","value":"${sq(7)}"},{"print":"${await t}"},
        {"let":"all","value":"${await [sq(1),sq(2),sq(3)]}"},{"print":"${all}"},
        {"let":"res","value":[]},
        {"parallel":[[{"call":"push(res,1)"}],[{"call":"push(res,2)"}],[{"call":"push(res,3)"}]]},
        {"print":"${res.sort()}"},
        {"spawn":[{"return":5}],"as":"h"},{"await":"h","as":"v"},{"print":"${v}"}]}"#;
    assert_eq!(ok(s), "49\n[1, 4, 9]\n[1, 2, 3]\n5\n");
}

#[test]
fn parallel_error_propagates() {
    let s = main_of(r#"[{"parallel":[[{"throw":"${new Error('x')}"}],[{"call":"range(3)"}]]}]"#);
    assert!(fails(&s).contains("Error: x"));
}

#[test]
fn const_assignment_rejected() {
    let e = fails(&main_of(r#"[{"const":"k","value":1},{"set":"k","value":2}]"#));
    assert!(e.contains("constant"), "{}", e);
}

#[test]
fn undeclared_variable_hint() {
    let e = fails(&main_of(r#"[{"let":"counter","value":1},{"print":"${countr}"}]"#));
    assert!(e.contains("did you mean `counter`"), "{}", e);
}

#[test]
fn stack_overflow_is_error_not_crash() {
    let s = r#"{"nin":"1","functions":{"r":{"body":[{"call":"r()"}]}},"main":[{"try":[{"call":"r()"}],"catch":[{"print":"${e.message}"}],"as":"e"}]}"#;
    let handle = std::thread::Builder::new().stack_size(256 * 1024 * 1024).spawn(move || ok(s)).unwrap();
    assert!(handle.join().unwrap().contains("stack overflow"));
}

#[test]
fn exit_is_not_catchable() {
    let e = fails(&main_of(r#"[{"try":[{"exit":3}],"catch":[{"print":"no"}]}]"#));
    assert_eq!(e, "exit 3");
}

#[test]
fn assert_stmt() {
    let e = fails(&main_of(r#"[{"assert":"1 == 2","message":"math broke"}]"#));
    assert!(e.contains("AssertionError: math broke"), "{}", e);
}

#[test]
fn run_process_capture() {
    #[cfg(unix)]
    let s = main_of(r#"[{"run":"echo","args":["hello"],"into":"r"},{"print":"${r.stdout.trim()} ${r.code} ${r.ok}"}]"#);
    #[cfg(windows)]
    let s = main_of(r#"[{"run":"cmd","args":["/c","echo","hello"],"into":"r"},{"print":"${r.stdout.trim()} ${r.code} ${r.ok}"}]"#);
    assert_eq!(ok(&s), "hello 0 true\n");
}

#[test]
fn run_process_check_fails() {
    #[cfg(unix)]
    let s = main_of(r#"[{"run":"false","check":true}]"#);
    #[cfg(windows)]
    let s = main_of(r#"[{"run":"cmd","args":["/c","exit","3"],"check":true}]"#);
    assert!(fails(&s).contains("ProcessError"));
}

#[test]
fn import_file_module() {
    let dir = std::env::temp_dir().join(format!("nin-rt-imp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("util.nin"), r#"{"nin":"1","const":{"PI":3},"functions":{"dbl":{"params":["x"],"body":[{"return":"${x*2}"}]}},"classes":{"Pt":{"fields":{"x":"int"},"constructors":[{"params":["x: int"],"body":[{"set":"this.x","value":"${x}"}]}]}},"main":[{"print":"NOT RUN"}]}"#).unwrap();
    let src = r#"{"nin":"1","imports":[{"file":"./util.nin"}],"main":[{"print":"${util.dbl(4)} ${util.PI} ${new util.Pt(9).x}"}]}"#;
    let interp = InterpBuilder::new().build();
    let mut ex = Exec::new(interp.clone());
    let cap = Arc::new(Cap(Mutex::new(String::new())));
    ex.sink = Some(cap.clone());
    let prog = ex.parse_source("m.nin", src).unwrap();
    if let Err(Signal::Throw(t)) = ex.run_program(&prog, &dir, "m") {
        panic!("{}", interp.render_throw(&t));
    }
    assert_eq!(*cap.0.lock().unwrap(), "8 3 9\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_std_module_suggests() {
    let e = fails(r#"{"nin":"1","imports":[{"use":"std.fs"}],"main":[]}"#);
    assert!(e.contains("unknown standard module"), "{}", e);
}

#[test]
fn json_roundtrip_helpers() {
    let j: serde_json::Value = serde_json::from_str(r#"{"a":[1,2.5,"x",null,true]}"#).unwrap();
    let v = builtins::json_to_value(&j);
    assert_eq!(builtins::value_to_json(&v).unwrap(), j);
}
