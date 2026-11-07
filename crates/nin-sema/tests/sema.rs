use nin_sema::*;

fn codes(src: &str) -> Vec<String> {
    let (p, d) = nin_ast::lower::parse_program_diag(src, 0);
    assert!(!d.has_errors(), "parse errors: {:?}", d.items.iter().map(|x| x.message.clone()).collect::<Vec<_>>());
    analyze(&p.unwrap(), &Options::default()).items.iter().map(|d| d.code.to_string()).collect()
}
fn main_prog(body: &str) -> String {
    format!(r#"{{"nin":"1","main":{}}}"#, body)
}
fn has(src: &str, code: &str) {
    let c = codes(src);
    assert!(c.iter().any(|x| x == code), "expected {} in {:?}", code, c);
}

#[test]
fn clean_program_has_no_diagnostics() {
    let c = codes(&main_prog(r#"[{"let":"x","value":1},{"print":"${x+1}"}]"#));
    assert!(c.is_empty(), "{:?}", c);
}
#[test]
fn undefined_name() {
    has(&main_prog(r#"[{"print":"${nope}"}]"#), "E0301");
}
#[test]
fn unused_variable_warns() {
    has(&main_prog(r#"[{"let":"x","value":1}]"#), "W0301");
}
#[test]
fn break_outside_loop() {
    has(&main_prog(r#"[{"break":true}]"#), "E0312");
}
#[test]
fn const_assign() {
    has(&main_prog(r#"[{"const":"c","value":1},{"set":"c","value":2},{"print":"${c}"}]"#), "E0310");
}
#[test]
fn type_mismatch_let() {
    has(&main_prog(r#"[{"let":"x","type":"int","value":"hi"},{"print":"${x}"}]"#), "E0401");
}
#[test]
fn call_arity() {
    let s = r#"{"nin":"1","functions":{"f":{"params":["a: int"],"body":[{"return":"${a}"}]}},"main":[{"print":"${f(1,2)}"}]}"#;
    has(s, "E0308");
}
#[test]
fn abstract_instantiation() {
    let s = r#"{"nin":"1","classes":{"A":{"abstract":true,"methods":{"m":{"abstract":true}}}},"main":[{"let":"a","value":"${new A()}"},{"print":"${a}"}]}"#;
    has(s, "E0319");
}
