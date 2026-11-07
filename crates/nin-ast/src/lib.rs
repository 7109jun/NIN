//! NIN AST — the typed tree every later stage (semantic analysis, runtime,
//! workflow engine, compiler) works on. It is produced from the validated JSON
//! document by the lowering pass (see docs/SPEC.md for the JSON surface syntax).
//!
//! All nodes are `Send + Sync` (function bodies are shared through `Arc`) so the
//! parallel runtime can execute them from several threads.

pub mod expr;
pub mod lower;
pub mod types;

pub use expr::{parse_expression, parse_template};
pub use lower::{lower, parse_program};
pub use types::parse_type;

use nin_diag::Span;
use std::sync::Arc;

pub type Block = Vec<Stmt>;

// ───────────────────────────── types (syntactic) ─────────────────────────────

/// A type as written in the source, e.g. `int`, `list<string>`, `Box<T>?`,
/// `int|string`, `fn(int,int)->int`, `mod.Class<int>`.
#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeExprKind {
    /// Possibly qualified name with type arguments. Builtins are plain names:
    /// `any int float string bool null void list<T> map<K,V> set<T> task<T> fn error`.
    Named { path: Vec<String>, args: Vec<TypeExpr> },
    /// `T?` — T or null.
    Optional(Box<TypeExpr>),
    /// `A|B|C`
    Union(Vec<TypeExpr>),
    /// `fn(A,B)->R`
    Func { params: Vec<TypeExpr>, ret: Box<TypeExpr> },
}

impl TypeExpr {
    pub fn new(kind: TypeExprKind, span: Span) -> TypeExpr {
        TypeExpr { kind, span }
    }
    pub fn named(name: &str, span: Span) -> TypeExpr {
        TypeExpr { kind: TypeExprKind::Named { path: vec![name.to_string()], args: vec![] }, span }
    }
    /// Canonical source form (used in messages and reflection).
    pub fn display(&self) -> String {
        match &self.kind {
            TypeExprKind::Named { path, args } => {
                let mut s = path.join(".");
                if !args.is_empty() {
                    s.push('<');
                    s.push_str(&args.iter().map(|a| a.display()).collect::<Vec<_>>().join(","));
                    s.push('>');
                }
                s
            }
            TypeExprKind::Optional(t) => format!("{}?", t.display()),
            TypeExprKind::Union(ts) => ts.iter().map(|t| t.display()).collect::<Vec<_>>().join("|"),
            TypeExprKind::Func { params, ret } => {
                format!("fn({})->{}", params.iter().map(|p| p.display()).collect::<Vec<_>>().join(","), ret.display())
            }
        }
    }
}

// ───────────────────────────── expressions ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// short-circuit `&&` / `and`
    And,
    /// short-circuit `||` / `or`
    Or,
    /// `??` — left unless null
    Coalesce,
    /// `x in container` (list element, map key, substring)
    In,
}

impl BinOp {
    pub fn symbol(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Coalesce => "??",
            BinOp::In => "in",
        }
    }
}

#[derive(Clone, Debug)]
pub enum TemplatePart {
    Text(String),
    /// `${ expr }`
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A string containing `${...}` parts. If the template consists of exactly one
    /// `Expr` part and no text, evaluation yields that expression's raw value
    /// (not stringified).
    Template(Vec<TemplatePart>),
    List(Vec<Expr>),
    /// Map literal; keys are evaluated and must yield strings.
    Map(Vec<(Expr, Expr)>),
    Ident(String),
    This,
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `obj.name` / `obj?.name` (optional: null-propagating)
    Member { obj: Box<Expr>, name: String, optional: bool },
    /// `obj[index]`
    Index { obj: Box<Expr>, index: Box<Expr> },
    /// `obj[from:to]` (either bound optional)
    Slice { obj: Box<Expr>, from: Option<Box<Expr>>, to: Option<Box<Expr>> },
    /// `callee(args)`; method calls are `Call` of a `Member`.
    Call { callee: Box<Expr>, args: Vec<Expr> },
    /// `new Class<T>(args)`
    New { class: TypeExpr, args: Vec<Expr> },
    /// `super.method(args)` (method = Some) inside a method.
    SuperMethod { method: String, args: Vec<Expr> },
    /// `fn(a, b) => expr` — body is a single `Return`.
    Lambda(Arc<FuncDecl>),
    /// `x is Type`
    Is(Box<Expr>, TypeExpr),
    /// `x as Type` — checked conversion/cast (throws on failure)
    As(Box<Expr>, TypeExpr),
    /// `await task`
    Await(Box<Expr>),
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }
    pub fn null(span: Span) -> Expr {
        Expr { kind: ExprKind::Null, span }
    }
    pub fn str(s: impl Into<String>, span: Span) -> Expr {
        Expr { kind: ExprKind::Str(s.into()), span }
    }
    /// True for expressions that may appear as an assignment target.
    pub fn is_place(&self) -> bool {
        matches!(self.kind, ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. })
    }
}

// ───────────────────────────── statements ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Redirect {
    Inherit,
    Null,
    Capture,
    /// stderr only: merge into stdout
    Merge,
    File { path: Expr, append: bool },
}

/// External process execution (`{"run": ...}` statement and workflow `run` steps).
#[derive(Clone, Debug)]
pub struct RunStmt {
    pub program: Expr,
    pub args: Vec<Expr>,
    /// run `program` as a full command line through `cmd.exe /c`
    pub shell: bool,
    pub cwd: Option<Expr>,
    pub env: Vec<(String, Expr)>,
    pub stdin: Option<Expr>,
    pub stdin_file: Option<Expr>,
    pub stdout: Option<Redirect>,
    pub stderr: Option<Redirect>,
    /// variable receiving the result map `{code, ok, stdout, stderr, duration_ms, timed_out, pid}`
    pub into: Option<String>,
    /// throw `ProcessError` on non-zero exit
    pub check: bool,
    pub timeout: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MatchCase {
    pub when: Vec<Expr>,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub struct CatchClause {
    pub ty: Option<TypeExpr>,
    pub var: Option<String>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// `let` (mutable) / `const` (immutable) declaration
    Let { name: String, ty: Option<TypeExpr>, value: Option<Expr>, is_const: bool },
    /// `target = value` or compound `target op= value`
    Assign { target: Expr, op: Option<BinOp>, value: Expr },
    /// expression statement (`call`)
    Expr(Expr),
    Print { value: Expr, newline: bool, stderr: bool },
    If { cond: Expr, then: Block, otherwise: Block },
    While { cond: Expr, body: Block },
    /// `for x in e` (one var) / `for k, v in e` (two vars)
    For { vars: Vec<String>, iter: Expr, body: Block },
    Match { subject: Expr, cases: Vec<MatchCase>, default: Block },
    Break,
    Continue,
    Return(Option<Expr>),
    Throw(Expr),
    Try { body: Block, catches: Vec<CatchClause>, finally: Block },
    Run(Box<RunStmt>),
    /// run branches concurrently, join all; optional max concurrency
    Parallel { branches: Vec<Block>, max: Option<Expr> },
    /// start `body` asynchronously; the task handle is bound to `name` (if any)
    Spawn { body: Block, name: Option<String> },
    /// wait for a task (or list of tasks); result bound to `into`
    Await { task: Expr, into: Option<String> },
    /// scoped block
    Block(Block),
    Assert { cond: Expr, message: Option<Expr> },
    /// workflow step output: `{"output":"name","value":..}`
    Output { name: String, value: Expr },
    /// constructor-only: call the parent constructor
    SuperCall(Vec<Expr>),
    /// terminate the process with an exit code
    Exit(Option<Expr>),
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Stmt {
        Stmt { kind, span }
    }
}

// ───────────────────────────── declarations ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    Public,
    Protected,
    Private,
}

impl Access {
    pub fn as_str(&self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Protected => "protected",
            Access::Private => "private",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub variadic: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: String,
    pub bound: Option<TypeExpr>,
}

/// Function, method, constructor or lambda.
#[derive(Clone, Debug)]
pub struct FuncDecl {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    /// `None` for abstract / interface methods
    pub body: Option<Block>,
    pub is_async: bool,
    pub is_static: bool,
    pub is_abstract: bool,
    pub is_override: bool,
    pub is_final: bool,
    pub access: Access,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub access: Access,
    pub is_static: bool,
    pub readonly: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub extends: Option<TypeExpr>,
    pub implements: Vec<TypeExpr>,
    pub is_abstract: bool,
    pub is_final: bool,
    pub fields: Vec<FieldDecl>,
    pub constructors: Vec<Arc<FuncDecl>>,
    /// Overloads are separate entries sharing a name (document order).
    pub methods: Vec<Arc<FuncDecl>>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub extends: Vec<TypeExpr>,
    /// signatures only (`body == None`)
    pub methods: Vec<Arc<FuncDecl>>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: String,
    /// `(variant name, explicit value)`; without value the ordinal is used.
    pub variants: Vec<(String, Option<Expr>)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct GlobalDecl {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    pub is_const: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ImportKind {
    /// `std.fs`, `std.json`, ...
    Std(String),
    /// relative `.nin` file path
    File(String),
    /// installed `.nnpkg` package name
    Package(String),
}

#[derive(Clone, Debug)]
pub struct Import {
    pub kind: ImportKind,
    pub alias: String,
    pub span: Span,
}

// ───────────────────────────── workflow ─────────────────────────────

#[derive(Clone, Debug)]
pub struct InputDecl {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// bytes, or a string such as "512MB"
    pub memory: Option<Expr>,
    pub processes: Option<Expr>,
    /// duration
    pub cpu_time: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct Matrix {
    pub dims: Vec<(String, Expr)>,
    pub include: Vec<Expr>,
    pub exclude: Vec<Expr>,
    pub fail_fast: bool,
    pub max_parallel: Option<Expr>,
}

#[derive(Clone, Debug)]
pub enum StepAction {
    Run(Box<RunStmt>),
    /// NIN statements executed in an isolated child scope
    Script(Block),
    /// call a function declared in the program (or `module.function`) with named arguments
    Uses { function: String, with: Vec<(String, Expr)> },
}

#[derive(Clone, Debug)]
pub struct Step {
    pub id: Option<String>,
    pub name: Option<Expr>,
    pub cond: Option<Expr>,
    pub action: StepAction,
    pub env: Vec<(String, Expr)>,
    pub workdir: Option<Expr>,
    pub timeout: Option<Expr>,
    pub retries: Option<Expr>,
    pub retry_delay: Option<Expr>,
    pub continue_on_error: Option<Expr>,
    pub outputs: Vec<(String, Expr)>,
    /// statements run when the step fails (`error` is bound)
    pub on_error: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub name: Option<Expr>,
    pub needs: Vec<String>,
    pub cond: Option<Expr>,
    pub env: Vec<(String, Expr)>,
    pub workdir: Option<Expr>,
    pub timeout: Option<Expr>,
    pub retries: Option<Expr>,
    pub retry_delay: Option<Expr>,
    pub continue_on_error: Option<Expr>,
    pub matrix: Option<Matrix>,
    pub limits: Option<Limits>,
    pub steps: Vec<Step>,
    pub outputs: Vec<(String, Expr)>,
    /// steps that always run after the job body (cleanup)
    pub finally: Vec<Step>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Workflow {
    pub name: Option<String>,
    pub env: Vec<(String, Expr)>,
    pub inputs: Vec<InputDecl>,
    pub jobs: Vec<Job>,
    /// max concurrently running jobs (default: number of CPUs)
    pub max_parallel: Option<Expr>,
    pub default_timeout: Option<Expr>,
    pub default_retries: Option<Expr>,
    pub span: Span,
}

// ───────────────────────────── program ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Program {
    pub file: u32,
    /// `"nin"` format version string (currently "1")
    pub version: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub imports: Vec<Import>,
    /// document order: `const` entries first, then `vars`
    pub globals: Vec<GlobalDecl>,
    pub enums: Vec<EnumDecl>,
    pub interfaces: Vec<InterfaceDecl>,
    pub classes: Vec<ClassDecl>,
    pub functions: Vec<Arc<FuncDecl>>,
    pub main: Option<Block>,
    pub workflow: Option<Workflow>,
}

impl Program {
    pub fn empty(file: u32) -> Program {
        Program {
            file,
            version: "1".to_string(),
            name: None,
            description: None,
            imports: vec![],
            globals: vec![],
            enums: vec![],
            interfaces: vec![],
            classes: vec![],
            functions: vec![],
            main: None,
            workflow: None,
        }
    }
    pub fn find_function(&self, name: &str) -> Option<&Arc<FuncDecl>> {
        self.functions.iter().find(|f| f.name == name)
    }
    pub fn find_class(&self, name: &str) -> Option<&ClassDecl> {
        self.classes.iter().find(|c| c.name == name)
    }
    pub fn find_interface(&self, name: &str) -> Option<&InterfaceDecl> {
        self.interfaces.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn ast_is_thread_safe() {
        assert_send_sync::<Program>();
        assert_send_sync::<Arc<FuncDecl>>();
        assert_send_sync::<Workflow>();
    }

    #[test]
    fn type_display() {
        let sp = Span::DUMMY;
        let list = TypeExpr::new(TypeExprKind::Named { path: vec!["list".into()], args: vec![TypeExpr::named("int", sp)] }, sp);
        assert_eq!(list.display(), "list<int>");
        let opt = TypeExpr::new(TypeExprKind::Optional(Box::new(list)), sp);
        assert_eq!(opt.display(), "list<int>?");
        let f = TypeExpr::new(TypeExprKind::Func { params: vec![TypeExpr::named("int", sp)], ret: Box::new(TypeExpr::named("string", sp)) }, sp);
        assert_eq!(f.display(), "fn(int)->string");
    }

    #[test]
    fn places() {
        let sp = Span::DUMMY;
        assert!(Expr::new(ExprKind::Ident("x".into()), sp).is_place());
        assert!(!Expr::null(sp).is_place());
    }
}
