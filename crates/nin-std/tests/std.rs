use nin_runtime::*;
use std::path::Path;
use std::sync::{Arc, Mutex};

struct Cap(Mutex<String>);
impl Sink for Cap {
    fn write(&self, t: &str, _e: bool) {
        self.0.lock().unwrap().push_str(t);
    }
}

fn run(imports: &str, main: &str) -> Result<String, String> {
    let src = format!(r#"{{"nin":"1","imports":[{}],"main":{}}}"#, imports, main);
    let interp = nin_std::builder().build();
    let mut ex = Exec::new(interp.clone());
    let cap = Arc::new(Cap(Mutex::new(String::new())));
    ex.sink = Some(cap.clone());
    let prog = ex.parse_source("t.nin", &src)?;
    match ex.run_program(&prog, Path::new("."), "t") {
        Ok(_) => Ok(cap.0.lock().unwrap().clone()),
        Err(Signal::Throw(t)) => Err(interp.render_throw(&t)),
        Err(Signal::Exit(c)) => Err(format!("exit {}", c)),
    }
}

fn ok(imports: &str, main: &str) -> String {
    run(imports, main).unwrap_or_else(|e| panic!("{}", e))
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("nin-std-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().replace('\\', "/")
}

#[test]
fn fs_roundtrip() {
    let d = tmp("fs");
    let main = format!(r#"[
      {{"let":"d","value":"{d}"}},
      {{"call":"fs.write_text(d + '/a.txt', 'héllo')"}},
      {{"call":"fs.append_text(d + '/a.txt', ' world')"}},
      {{"print":"${{fs.read_text(d + '/a.txt')}} ${{fs.exists(d + '/a.txt')}} ${{fs.is_dir(d)}}"}},
      {{"call":"fs.mkdir_all(d + '/x/y')"}},
      {{"call":"fs.copy(d + '/a.txt', d + '/x/y/b.nin')"}},
      {{"call":"fs.rename(d + '/a.txt', d + '/c.txt')"}},
      {{"print":"${{fs.list_dir(d)}}"}},
      {{"print":"${{fs.glob(d + '/**/*.nin').map(fn(p) => path.file_name(p))}} ${{fs.metadata(d + '/c.txt').size}}"}},
      {{"try":[{{"call":"fs.read_text(d + '/nope')"}}],"catch":[{{"print":"${{e.code}} ${{reflect.class_name(e)}}"}}],"as":"e"}},
      {{"call":"fs.remove_all(d)"}},{{"print":"${{fs.exists(d)}}"}}
    ]"#);
    let o = ok(r#"{"use":"std.fs"},{"use":"std.path"}"#, &main);
    assert_eq!(o, "héllo world true true\n[\"c.txt\", \"x\"]\n[\"b.nin\"] 12\nnot_found IOError\nfalse\n");
}

#[test]
fn path_functions() {
    let o = ok(r#"{"use":"std.path"}"#, r#"[{"print":"${path.join('a','b','c.txt')}"},{"print":"${path.stem('x/y.tar.gz')} ${path.ext('x/y.gz')} ${path.file_name('a/b.c')} ${path.with_ext('a/b.c','nin')}"},{"print":"${path.normalize('a/./b/../c')} ${path.relative_to('a/b/c','a')} ${path.parent('a/b')}"}]"#);
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(o, format!("a{s}b{s}c.txt\ny.tar gz b.c a{s}b.nin\na{s}c b{s}c a\n", s = sep));
}

#[test]
fn json_yaml_toml() {
    let o = ok(r#"{"use":"std.json"},{"use":"std.yaml"},{"use":"std.toml"}"#, r#"[
      {"let":"v","value":{"a":[1,2.5,"x",null],"b":{"c":true}}},
      {"print":"${json.stringify(v)}"},
      {"print":"${json.parse(json.stringify(v)) == v}"},
      {"print":"${yaml.parse(yaml.stringify(v)) == v}"},
      {"print":"${yaml.parse('a: 1\\nb: [x, y]\\nc:\\n  d: true').b}"},
      {"print":"${toml.parse('x = 1\\n[t]\\ny = \"s\"').t.y}"},
      {"print":"${toml.parse(toml.stringify({n: 1, t: {a: [1,2]}})).t.a}"},
      {"try":[{"call":"json.parse('{bad')"}],"catch":[{"print":"${e.code}"}],"as":"e"},
      {"try":[{"call":"toml.stringify({a: null})"}],"catch":[{"print":"${e.code}"}],"as":"e"}
    ]"#);
    assert_eq!(o, "{\"a\":[1,2.5,\"x\",null],\"b\":{\"c\":true}}\ntrue\ntrue\n[\"x\", \"y\"]\ns\n[1, 2]\nvalueerror\nvalueerror\n");
}

#[test]
fn object_hash_regex_random() {
    let o = ok(r#"{"use":"std.object"},{"use":"std.hash"},{"use":"std.regex"},{"use":"std.random"}"#, r#"[
      {"print":"${object.pick({a:1,b:2,c:3},['a','c'])} ${object.omit({a:1,b:2},['a'])} ${object.entries({x:1})}"},
      {"let":"o","value":{"a":{"b":[10,20]}}},
      {"print":"${object.get_path(o,'a.b.1')} ${object.get_path(o,'a.z','d')}"},
      {"call":"object.set_path(o,'a.n.m',5)"},{"print":"${o}"},
      {"print":"${object.deep_merge({a:{x:1}},{a:{y:2}})}"},
      {"print":"${hash.sha256('abc')}"},
      {"print":"${hash.md5('abc')} ${hash.crc32('123456789')}"},
      {"print":"${hash.base64_encode('hi!')} ${hash.base64_decode('aGkh')} ${hash.hex_encode('AB')}"},
      {"print":"${regex.is_match('^a+b','aaab')} ${regex.find('(\\\\d+)-(\\\\d+)','x 10-20 y').groups} ${regex.replace('\\\\s+','a  b   c',' ')} ${regex.split(',','1,2,3')}"},
      {"call":"random.seed(7)"},{"let":"r","value":"${random.int(1,6)}"},{"assert":"r >= 1 && r <= 6"},
      {"print":"${len(random.uuid())} ${random.shuffle([1,2,3]).len()}"}
    ]"#);
    let want = "{\"a\": 1, \"c\": 3} {\"b\": 2} [[\"x\", 1]]\n20 d\n{\"a\": {\"b\": [10, 20], \"n\": {\"m\": 5}}}\n{\"a\": {\"x\": 1, \"y\": 2}}\nba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n900150983cd24fb0d6963f7d28e17f72 3421780262\naGkh hi! 4142\ntrue [\"10\", \"20\"] a b c [\"1\", \"2\", \"3\"]\n36 3\n";
    assert_eq!(o, want);
}

#[test]
fn time_math_task() {
    let o = ok(r#"{"use":"std.time"},{"use":"std.math"},{"use":"std.task"},{"use":"std.str"},{"use":"std.list"}"#, r#"[
      {"print":"${time.format(0, '%Y-%m-%d %H:%M:%S')} ${time.parse('1970-01-02T00:00:00Z')} ${time.duration('1h30m')}"},
      {"print":"${time.parts(86400000).day} ${time.format(time.add_days(0, 1), '%d')}"},
      {"print":"${math.floor(2.7)} ${math.ceil(2.1)} ${math.round(2.5)} ${math.sqrt(16)} ${math.max(1,5,3)} ${math.min([4,2,9])} ${math.clamp(15,0,10)} ${math.pow(2,3)}"},
      {"let":"t","value":"${task.spawn(fn(x) => x * 2, 21)}"},{"print":"${await t}"},
      {"print":"${task.race([task.spawn(fn() => task.sleep(300)), task.spawn(fn() => 'fast')])}"},
      {"try":[{"call":"task.with_timeout(task.spawn(fn() => task.sleep(2000)), '50ms')"}],"catch":[{"print":"${reflect.class_name(e)}"}],"as":"e"},
      {"print":"${str.upper('ab')} ${list.sum([1,2,3])} ${list.map([1,2], fn(x) => x + 1)}"}
    ]"#);
    assert_eq!(o, "1970-01-01 00:00:00 86400000 5400000\n2 02\n2 3 3 4.0 5 2 10 8.0\n42\nfast\nTimeoutError\nAB 6 [2, 3]\n");
}

#[test]
fn process_sys_env() {
    #[cfg(unix)]
    let run_line = r#"{"print":"${process.run('echo',['a b'],{}).stdout.trim()} ${process.shell('echo $X',{env:{X:'v'}}).stdout.trim()}"}"#;
    #[cfg(windows)]
    let run_line = r#"{"print":"${process.run('cmd',['/c','echo','ab'],{}).stdout.trim()} ${process.shell('echo %X%',{env:{X:'v'}}).stdout.trim()}"}"#;
    let main = format!(r#"[{},
      {{"print":"${{process.pid() > 0}} ${{sys.cpu_count() >= 1}} ${{typeof(sys.info().os)}}"}},
      {{"call":"env.set('NIN_T','7')"}},{{"print":"${{env.get('NIN_T')}} ${{env.get('NIN_NOPE','d')}} ${{env.expand('%NIN_T%+${{NIN_T}}')}}"}},
      {{"try":[{{"call":"process.run('definitely-not-a-program-xyz',[],{{}})"}}],"catch":[{{"print":"${{reflect.class_name(e)}}"}}],"as":"e"}},
      {{"print":"${{process.run('{prog}',{args},{{timeout:'100ms'}}).timed_out}}"}}
    ]"#,
        run_line,
        prog = if cfg!(windows) { "ping" } else { "sleep" },
        args = if cfg!(windows) { r#"['-n','6','127.0.0.1']"# } else { r#"['5']"# });
    let o = ok(r#"{"use":"std.process"},{"use":"std.sys"},{"use":"std.env"}"#, &main);
    let first = if cfg!(windows) { "ab v\n" } else { "a b v\n" };
    assert_eq!(o, format!("{}true true string\n7 d 7+7\nProcessError\ntrue\n", first));
}

#[test]
fn http_loopback() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().take(3) {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = if req.starts_with("GET /redir") {
                "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
            } else if req.starts_with("POST") {
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                format!("HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\necho:{}", body.len() + 5, body)
            } else {
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n".to_string()
            };
            s.write_all(resp.as_bytes()).unwrap();
        }
    });
    let main = format!(r#"[
      {{"let":"r","value":"${{net.http_get('http://127.0.0.1:{p}/redir')}}"}},
      {{"print":"${{r.status}} ${{r.body}} ${{r.ok}}"}},
      {{"let":"q","value":"${{net.http_post('http://127.0.0.1:{p}/x', {{k:1}})}}"}},
      {{"print":"${{q.status}} ${{q.body}}"}},
      {{"print":"${{net.tcp_connect('127.0.0.1', 1, '200ms')}}"}}
    ]"#, p = port);
    let o = ok(r#"{"use":"std.net"}"#, &main);
    assert_eq!(o, "200 hello true\n201 echo:{\"k\":1}\nfalse\n");
}

#[test]
fn log_and_io_basic() {
    let o = ok(r#"{"use":"std.log"},{"use":"std.io"}"#, r#"[{"call":"log.info('x')"},{"call":"io.write('a','b')"},{"print":""}]"#);
    assert!(o.contains("[INFO ] x"), "{}", o);
    assert!(o.contains("ab"));
}
