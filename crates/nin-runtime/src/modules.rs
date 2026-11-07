//! Prelude, program loading and module imports (std / file / package / plugin).

use crate::builtins::{json_to_value, value_to_json};
use crate::interp::*;
use crate::value::*;
use nin_ast::*;
use nin_diag::{suggest, Diagnostic};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const PRELUDE: &str = r##"{
  "nin": "1",
  "classes": {
    "Error": {
      "doc": "Base class of all errors.",
      "fields": {
        "message": {"type": "string", "default": ""},
        "code": {"type": "string", "default": "error"},
        "cause": "any",
        "stack": {"type": "list", "default": []}
      },
      "constructors": [
        {"params": ["message: any = ''", "cause: any = null"], "body": [
          {"set": "this.message", "value": "${str(message)}"},
          {"set": "this.cause", "value": "${cause}"},
          {"set": "this.code", "value": "${reflect.class_name(this).lower()}"}
        ]}
      ],
      "methods": {
        "to_string": {"returns": "string", "body": [{"return": "${reflect.class_name(this) + ': ' + this.message}"}]}
      }
    },
    "TypeError": {"extends": "Error"},
    "ValueError": {"extends": "Error"},
    "KeyError": {"extends": "Error"},
    "IndexError": {"extends": "Error"},
    "ArithmeticError": {"extends": "Error"},
    "AssertionError": {"extends": "Error"},
    "IOError": {"extends": "Error"},
    "ProcessError": {"extends": "Error", "fields": {"result": "any"}},
    "TimeoutError": {"extends": "Error"},
    "CancelledError": {"extends": "Error"},
    "StateError": {"extends": "Error"},
    "AccessError": {"extends": "Error"},
    "ImportError": {"extends": "Error"},
    "NameError": {"extends": "Error"}
  }
}"##;

pub fn load_prelude(ex: &mut Exec) -> R<()> {
    let file = ex.interp.add_source("<prelude>", PRELUDE);
    let prog = match nin_ast::parse_program(PRELUDE, file) {
        Ok(p) => p,
        Err(ds) => {
            let sm = lk(&ex.interp.sources);
            panic!("internal error: prelude does not parse:\n{}", sm.render_all(&ds.items, false));
        }
    };
    let root = ex.interp.root.clone();
    ex.define_program(&prog, &root, "<prelude>")
}

impl Interp {
    pub fn render_diagnostics(&self, ds: &[Diagnostic], color: bool) -> String {
        lk(&self.sources).render_all(ds, color)
    }
}

impl Exec {
    /// Parse a `.nin` source; on failure returns the rendered diagnostics.
    pub fn parse_source(&mut self, name: &str, text: &str) -> Result<Program, String> {
        let file = self.interp.add_source(name, text);
        let (p, ds) = nin_ast::lower::parse_program_diag(text, file);
        match p {
            Some(p) if !ds.has_errors() => Ok(p),
            _ => Err(self.interp.render_diagnostics(&ds.items, false)),
        }
    }

    /// Create the module scope for `prog`: imports + declarations (does not run `main`).
    pub fn load_program(&mut self, prog: &Program, base_dir: &Path, module: &str) -> R<Env> {
        let env = Scope::new(Some(self.interp.root.clone()));
        self.import_all(prog, &env, base_dir)?;
        self.define_program(prog, &env, module)?;
        Ok(env)
    }

    /// Load and run a program's `main` block.
    pub fn run_program(&mut self, prog: &Program, base_dir: &Path, module: &str) -> R<Env> {
        let env = self.load_program(prog, base_dir, module)?;
        if let Some(main) = &prog.main {
            let fr = Frame::new(env.clone(), module);
            self.exec_stmts(main, &fr)?;
        }
        Ok(env)
    }

    pub fn import_all(&mut self, prog: &Program, env: &Env, base_dir: &Path) -> R<()> {
        for imp in &prog.imports {
            let m = match &imp.kind {
                ImportKind::Std(name) => {
                    if let Some(p) = name.strip_prefix("plugin:") {
                        self.import_plugin(p, base_dir, imp.span)?
                    } else {
                        match self.interp.natives.get(name) {
                            Some(m) => m.clone(),
                            None => {
                                let names: Vec<&str> = self.interp.natives.keys().map(|s| s.as_str()).collect();
                                let mut msg = format!("unknown standard module `{}`", name);
                                if let Some(s) = suggest(name, names.iter().copied()) {
                                    msg.push_str(&format!(" (did you mean `{}`?)", s));
                                }
                                return self.err_at(imp.span, "ImportError", msg);
                            }
                        }
                    }
                }
                ImportKind::File(f) => {
                    let p = base_dir.join(f.replace('\\', "/"));
                    self.import_file(&p, imp.span)?
                }
                ImportKind::Package(name) if lk(&self.interp.vfs_pkgs).contains_key(name) => {
                    let entry = lk(&self.interp.vfs_pkgs).get(name).cloned().unwrap();
                    self.import_file(Path::new(&entry), imp.span)?
                }
                ImportKind::Package(name) if lk(&self.interp.vfs).contains_key(&format!("pkg:{}", name)) => {
                    let entry = lk(&self.interp.vfs).get(&format!("pkg:{}", name)).cloned().unwrap_or_default();
                    self.import_file(Path::new(&entry), imp.span)?
                }
                ImportKind::Package(name) => {
                    let root = self.interp.pkg_root.clone().unwrap_or_else(nin_pkg::PackageManager::default_root);
                    let pm = match nin_pkg::PackageManager::new(&root, self.interp.registries.clone()) {
                        Ok(pm) => pm,
                        Err(e) => return self.err_at(imp.span, "ImportError", format!("package manager unavailable: {}", e)),
                    };
                    match pm.find_installed(name) {
                        Ok(Some(pkg)) => match pkg.entry_path() {
                            Some(p) => self.import_file(&p, imp.span)?,
                            None => return self.err_at(imp.span, "ImportError", format!("package `{}` declares no entry file", name)),
                        },
                        Ok(None) => return self.err_at(imp.span, "ImportError", format!("package `{}` is not installed (run `nin pkg install {}`)", name, name)),
                        Err(e) => return self.err_at(imp.span, "ImportError", format!("package `{}`: {}", name, e)),
                    }
                }
            };
            env.define(&imp.alias, Value::Module(m), true);
        }
        Ok(())
    }

    fn import_file(&mut self, path: &Path, span: nin_diag::Span) -> R<Arc<Module>> {
        let key = norm_key(path);
        let embedded = lk(&self.interp.vfs).get(&key).cloned();
        let (canon, text) = match embedded {
            Some(t) => (PathBuf::from(&key), t),
            None => {
                let canon = match std::fs::canonicalize(path) {
                    Ok(c) => c,
                    Err(e) => return self.err_at(span, "ImportError", format!("cannot open module `{}`: {}", path.display(), e)),
                };
                let text = match std::fs::read_to_string(&canon) {
                    Ok(t) => t,
                    Err(e) => return self.err_at(span, "ImportError", format!("cannot read `{}`: {}", canon.display(), e)),
                };
                (canon, text)
            }
        };
        if let Some(m) = lk(&self.interp.modules).get(&canon) {
            return Ok(m.clone());
        }
        {
            let loading = lk(&self.interp.loading);
            if loading.contains(&canon) {
                let chain: Vec<String> = loading.iter().map(|p| p.display().to_string()).chain(std::iter::once(canon.display().to_string())).collect();
                drop(loading);
                return self.err_at(span, "ImportError", format!("circular import: {}", chain.join(" -> ")));
            }
        }
        let text = text.trim_start_matches('\u{feff}').to_string();
        let prog = match self.parse_source(&canon.display().to_string(), &text) {
            Ok(p) => p,
            Err(rendered) => return self.err_at(span, "ImportError", format!("module `{}` has errors:\n{}", canon.display(), rendered.trim_end())),
        };
        lk(&self.interp.loading).push(canon.clone());
        let dir = canon.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let name = canon.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "module".into());
        let res = self.load_program(&prog, &dir, &name);
        lk(&self.interp.loading).pop();
        let env = res?;
        let m = Arc::new(Module { name, kind: ModuleKind::Script { env, program: Arc::new(prog) }, path: Some(canon.clone()) });
        lk(&self.interp.modules).insert(canon, m.clone());
        Ok(m)
    }

    fn import_plugin(&mut self, spec: &str, base_dir: &Path, span: nin_diag::Span) -> R<Arc<Module>> {
        let mut cands: Vec<PathBuf> = vec![];
        let p = Path::new(spec);
        for base in [p.to_path_buf(), base_dir.join(p)] {
            cands.push(base.clone());
            if base.extension().is_none() {
                cands.push(base.with_extension("dll"));
            }
        }
        let path = match cands.iter().find(|c| c.is_file()) {
            Some(p) => p.clone(),
            None => return self.err_at(span, "ImportError", format!("plugin `{}` not found", spec)),
        };
        let key = std::fs::canonicalize(&path).unwrap_or(path.clone()).display().to_string();
        if let Some(m) = lk(&self.interp.plugins).get(&key) {
            return Ok(m.clone());
        }
        let plugin = match nin_plugin::host::LoadedPlugin::load(&path) {
            Ok(p) => Arc::new(p),
            Err(e) => return self.err_at(span, "ImportError", format!("cannot load plugin `{}`: {}", path.display(), e)),
        };
        let mut members = indexmap::IndexMap::new();
        for f in plugin.functions.clone() {
            let (pl, fname) = (plugin.clone(), f.name.clone());
            let min = f.min_args.max(0) as usize;
            let max = if f.max_args < 0 { None } else { Some(f.max_args as usize) };
            members.insert(
                f.name.clone(),
                Native::value(&f.name, min, max, move |ex, a| {
                    let mut js = Vec::with_capacity(a.len());
                    for v in a {
                        match value_to_json(v) {
                            Ok(j) => js.push(j),
                            Err(e) => return ex.err("TypeError", format!("plugin call `{}`: {}", fname, e)),
                        }
                    }
                    match pl.call(&fname, &serde_json::Value::Array(js)) {
                        Ok(r) => Ok(json_to_value(&r)),
                        Err(e) => ex.err("Error", format!("plugin `{}`: {}", fname, e)),
                    }
                }),
            );
        }
        let m = Arc::new(Module { name: plugin.name.clone(), kind: ModuleKind::Native(members), path: Some(path) });
        lk(&self.interp.plugins).insert(key, m.clone());
        Ok(m)
    }
}

/// Lexically normalize a path to a `/`-separated virtual path.
pub fn vnorm(p: &Path) -> String {
    use std::path::Component;
    let mut out: Vec<String> = vec![];
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.last().map_or(false, |l| l != "..") {
                    out.pop();
                } else {
                    out.push("..".into());
                }
            }
            Component::Normal(s) => out.push(s.to_string_lossy().to_string()),
            _ => {}
        }
    }
    out.join("/")
}

/// Lexically normalized, `/`-separated key used for embedded (bundled) files.
pub fn vfs_key(p: &Path) -> String {
    use std::path::Component;
    let mut out: Vec<String> = vec![];
    let mut prefix = String::new();
    for c in p.components() {
        match c {
            Component::Prefix(x) => prefix = x.as_os_str().to_string_lossy().to_string(),
            Component::RootDir => prefix.push('/'),
            Component::CurDir => {}
            Component::ParentDir => {
                if out.last().map_or(false, |l| l != "..") {
                    out.pop();
                } else if prefix.is_empty() {
                    out.push("..".into());
                }
            }
            Component::Normal(s) => out.push(s.to_string_lossy().to_string()),
        }
    }
    format!("{}{}", prefix, out.join("/")).replace('\\', "/")
}

/// Normalized, slash-separated path key used for embedded (bundled) sources.
pub fn norm_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s).to_string();
    let abs = s.starts_with('/');
    let mut out: Vec<&str> = vec![];
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(l) if *l != "..") {
                    out.pop();
                } else if !abs {
                    out.push("..");
                }
            }
            x => out.push(x),
        }
    }
    format!("{}{}", if abs { "/" } else { "" }, out.join("/"))
}
