//! Interactive shell (SPEC §9).
use crate::config::Config;
use crate::core::*;
use nin_diag::Span;
use nin_runtime::*;
use std::io::{BufRead, Write};
use std::sync::Arc;

fn balance(s: &str) -> i32 {
    let (mut depth, mut in_str, mut esc, mut q) = (0i32, false, false, '"');
    for c in s.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                in_str = true;
                q = c;
            }
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            _ => {}
        }
    }
    depth
}

const HELP: &str = "NIN shell commands:
  <expr>             evaluate an expression and print the value      e.g. 1 + 2,  [1,2,3].map(x => x * 2)
  {\"print\":\"hi\"}     run a JSON statement ({\"let\":..}, {\"for\":..}, ...)
  {\"functions\":{..}} define functions/classes/enums/imports (any top-level script section)
  !command args      run an external program
  :help              this text            :vars     list variables
  :funcs             list functions       :classes  list classes
  :load FILE         load a .nin file     :reset    clear all state
  :cd DIR            change directory     :history  show history
  :quit              leave (also Ctrl+Z / Ctrl+D)";

struct State {
    interp: Arc<Interp>,
    ex: Exec,
    env: Env,
    cfg: Config,
}

fn fresh(cfg: &Config) -> State {
    let interp = make_interp(cfg, vec![]);
    let ex = Exec::new(interp.clone());
    let env = Scope::new(Some(interp.root.clone()));
    State { interp, ex, env, cfg: cfg.clone() }
}

pub fn run(cfg: &Config) -> i32 {
    let mut st = fresh(cfg);
    watch_cancel(vec![]);
    let color = color_on(cfg);
    println!("NIN Shell {} — type :help for help, :quit to leave", env!("CARGO_PKG_VERSION"));
    nin_win::set_title("NIN Shell");
    let stdin = std::io::stdin();
    let mut history: Vec<String> = Vec::new();
    let mut buf = String::new();
    loop {
        let prompt = if buf.is_empty() { cfg.prompt.clone() } else { "...> ".to_string() };
        if color && buf.is_empty() {
            print!("\x1b[1;36m{}\x1b[0m", prompt);
        } else {
            print!("{}", prompt);
        }
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                println!();
                break;
            }
            Ok(_) => {}
        }
        CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        let line = line.trim_end_matches(['\r', '\n']);
        if buf.is_empty() && line.trim().is_empty() {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
        if balance(&buf) > 0 {
            continue;
        }
        let input = std::mem::take(&mut buf);
        let input = input.trim().to_string();
        history.push(input.clone());
        if history.len() > cfg.history_size.max(1) {
            history.remove(0);
        }
        if let Some(cmd) = input.strip_prefix(':') {
            if !command(&mut st, cmd.trim(), &history) {
                break;
            }
            continue;
        }
        if let Some(cmd) = input.strip_prefix('!') {
            let spec = nin_win::ProcSpec::shell(cmd.trim());
            match nin_win::run(&spec, None, None) {
                Ok(r) if r.code != 0 => println!("(exit {})", r.code),
                Err(e) => eprintln!("error: {}", e),
                _ => {}
            }
            continue;
        }
        eval_input(&mut st, &input, color);
    }
    EXIT_OK
}

fn command(st: &mut State, cmd: &str, history: &[String]) -> bool {
    let (name, arg) = cmd.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
    match name {
        "q" | "quit" | "exit" => return false,
        "h" | "help" | "?" => println!("{}", HELP),
        "vars" => {
            for n in st.env.local_names() {
                if let Some(v) = st.env.get_local(&n) {
                    if !matches!(v, Value::Func(_) | Value::Native(_) | Value::Class(_)) {
                        println!("{} = {}", n, repr(&v));
                    }
                }
            }
        }
        "funcs" => {
            for n in st.env.local_names() {
                if matches!(st.env.get_local(&n), Some(Value::Func(_))) {
                    println!("{}", n);
                }
            }
        }
        "classes" => {
            let mut v: Vec<String> = st.interp.classes.lock().unwrap().keys().cloned().collect();
            v.sort();
            for n in v {
                println!("{}", n);
            }
        }
        "reset" => {
            *st = fresh(&st.cfg.clone());
            println!("state cleared");
        }
        "cd" => {
            if arg.is_empty() {
                println!("{}", std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
            } else if let Err(e) = std::env::set_current_dir(arg) {
                eprintln!("error: {}", e);
            }
        }
        "history" => {
            for (i, h) in history.iter().enumerate() {
                println!("{:>4}  {}", i + 1, h.replace('\n', " "));
            }
        }
        "load" => {
            if arg.is_empty() {
                eprintln!("usage: :load FILE");
            } else {
                match read_script(std::path::Path::new(arg)) {
                    Ok(text) => load_into(st, arg, &text),
                    Err(e) => eprintln!("error: {}", e),
                }
            }
        }
        _ => eprintln!("unknown command `:{}` (try :help)", name),
    }
    true
}

fn load_into(st: &mut State, name: &str, text: &str) {
    let prog = match st.ex.parse_source(name, text) {
        Ok(p) => p,
        Err(r) => {
            eprint!("{}", r);
            return;
        }
    };
    let base = std::env::current_dir().unwrap_or_default();
    let res = (|| -> Result<(), Signal> {
        st.ex.import_all(&prog, &st.env, &base)?;
        st.ex.define_program(&prog, &st.env, "repl")?;
        if let Some(m) = &prog.main {
            let fr = Frame::new(st.env.clone(), "repl");
            st.ex.exec_stmts(m, &fr)?;
        }
        Ok(())
    })();
    if let Err(s) = res {
        report_signal(&st.interp, s);
    }
}

fn eval_input(st: &mut State, input: &str, color: bool) {
    if input.starts_with('{') {
        let is_program = ["\"functions\"", "\"classes\"", "\"enums\"", "\"interfaces\"", "\"imports\"", "\"main\"", "\"workflow\"", "\"nin\""].iter().any(|k| input.contains(k));
        let text = if is_program {
            if input.contains("\"nin\"") {
                input.to_string()
            } else {
                format!("{{\"nin\":\"1\",{}", &input[1..])
            }
        } else {
            format!("{{\"nin\":\"1\",\"main\":[{}]}}", input)
        };
        let prog = match st.ex.parse_source("<repl>", &text) {
            Ok(p) => p,
            Err(r) => {
                eprint!("{}", r);
                return;
            }
        };
        let base = std::env::current_dir().unwrap_or_default();
        let res = (|| -> Result<(), Signal> {
            st.ex.import_all(&prog, &st.env, &base)?;
            st.ex.define_program(&prog, &st.env, "repl")?;
            if let Some(m) = &prog.main {
                let fr = Frame::new(st.env.clone(), "repl");
                st.ex.exec_stmts(m, &fr)?;
            }
            Ok(())
        })();
        if let Err(s) = res {
            report_signal(&st.interp, s);
        }
        return;
    }
    let file = st.interp.add_source("<repl>", input);
    let expr = match nin_ast::expr::parse_expression(input, Span { file, start: 0, end: input.len() as u32 }) {
        Ok(e) => e,
        Err(d) => {
            eprint!("{}", st.interp.render_diagnostics(&[d], color));
            return;
        }
    };
    let fr = Frame::new(st.env.clone(), "repl");
    match st.ex.eval(&expr, &fr) {
        Ok(Value::Null) => {}
        Ok(v) => {
            st.env.define("_", v.clone(), false);
            println!("{}", repr(&v));
        }
        Err(s) => {
            report_signal(&st.interp, s);
        }
    }
}
