//! Lexer + Pratt parser for the NIN expression language (docs/SPEC.md §2) and
//! `${...}` string templates. Errors: E0201 unexpected token, E0202 unterminated
//! string / `${`, E0203 unexpected end, E0204 invalid number, E0205 invalid
//! escape / assignment target.

use crate::types::{SpanMap, TypeOpts, TypeParser};
use crate::*;
use nin_diag::{Diagnostic, Span};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

const PUNCTS: &[&str] = &[
    "...", "**", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "=>", "+", "-", "*", "/", "%", "<", ">", "!", "?", ":", ",", ".", "(", ")", "[", "]", "{", "}",
];

const KEYWORDS: &[&str] = &["and", "or", "not", "in", "is", "as"];

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}
fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    end: usize,
    map: SpanMap,
    peeked: Option<Token>,
    last_end: usize,
}

type PResult<T> = Result<T, Diagnostic>;

impl<'a> Parser<'a> {
    fn new(src: &'a str, pos: usize, end: usize, map: SpanMap) -> Parser<'a> {
        Parser { src, pos, end, map, peeked: None, last_end: pos }
    }

    fn sp(&self, a: usize, b: usize) -> Span {
        self.map.span(a, b)
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..self.end]
    }

    // ───────── lexer ─────────
    fn lex(&mut self) -> PResult<Token> {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        let start = self.pos;
        let rest = self.rest();
        let c = match rest.chars().next() {
            None => return Ok(Token { tok: Tok::Eof, start, end: start }),
            Some(c) => c,
        };
        if c.is_ascii_digit() {
            return self.lex_number();
        }
        if c == '"' || c == '\'' {
            return self.lex_string(c);
        }
        if is_ident_start(c) {
            let mut e = start;
            for ch in rest.chars() {
                if is_ident_char(ch) {
                    e += ch.len_utf8();
                } else {
                    break;
                }
            }
            self.pos = e;
            return Ok(Token { tok: Tok::Ident(self.src[start..e].to_string()), start, end: e });
        }
        for p in PUNCTS {
            if rest.starts_with(p) {
                // `?.` must be followed by an identifier start, otherwise it is `?` then `.5`-like
                if *p == "?." && !rest[2..].chars().next().map_or(false, is_ident_start) {
                    continue;
                }
                self.pos += p.len();
                return Ok(Token { tok: Tok::Punct(p), start, end: self.pos });
            }
        }
        Err(Diagnostic::error("E0201", format!("unexpected character `{}`", c)).at(self.sp(start, start + c.len_utf8()), "not valid in an expression").help(match c {
            '=' => "use `==` to compare; assignments are statements ({\"set\": ...})",
            '&' | '|' => "use `&&` / `||` (or `and` / `or`)",
            '@' | '#' | '`' | '\\' | ';' => "this character has no meaning in expressions",
            _ => "check the expression syntax",
        }))
    }

    fn lex_number(&mut self) -> PResult<Token> {
        let start = self.pos;
        let b = self.src.as_bytes();
        let mut i = start;
        if b[i] == b'0' && i + 1 < self.end && (b[i + 1] == b'x' || b[i + 1] == b'X') {
            i += 2;
            let ds = i;
            while i < self.end && (b[i].is_ascii_hexdigit() || b[i] == b'_') {
                i += 1;
            }
            let digits: String = self.src[ds..i].chars().filter(|c| *c != '_').collect();
            self.pos = i;
            return match i64::from_str_radix(&digits, 16) {
                Ok(v) if !digits.is_empty() => Ok(Token { tok: Tok::Int(v), start, end: i }),
                _ => Err(Diagnostic::error("E0204", "invalid hexadecimal number").at(self.sp(start, i), "expected hex digits (and must fit in 64 bits)")),
            };
        }
        while i < self.end && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
        let mut is_float = false;
        if i + 1 < self.end && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            is_float = true;
            i += 1;
            while i < self.end && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
        }
        if i < self.end && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < self.end && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j < self.end && b[j].is_ascii_digit() {
                is_float = true;
                while j < self.end && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let text: String = self.src[start..i].chars().filter(|c| *c != '_').collect();
        self.pos = i;
        if is_float {
            match text.parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(Token { tok: Tok::Float(f), start, end: i }),
                _ => Err(Diagnostic::error("E0204", format!("invalid number `{}`", text)).at(self.sp(start, i), "")),
            }
        } else {
            match text.parse::<i64>() {
                Ok(v) => Ok(Token { tok: Tok::Int(v), start, end: i }),
                Err(_) => Err(Diagnostic::error("E0204", format!("integer `{}` does not fit in 64 bits", text)).at(self.sp(start, i), "too large").help("write it as a float (add `.0`)")),
            }
        }
    }

    fn lex_string(&mut self, quote: char) -> PResult<Token> {
        let start = self.pos;
        let mut out = String::new();
        let mut i = start + 1;
        loop {
            let ch = match self.src[i..self.end].chars().next() {
                None => return Err(Diagnostic::error("E0202", "unterminated string literal").at(self.sp(start, start + 1), "this string is never closed").help(format!("add a closing {}", quote))),
                Some(c) => c,
            };
            i += ch.len_utf8();
            if ch == quote {
                break;
            }
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            let esc_start = i - 1;
            let e = match self.src[i..self.end].chars().next() {
                None => return Err(Diagnostic::error("E0202", "unterminated string literal").at(self.sp(start, start + 1), "")),
                Some(c) => c,
            };
            i += e.len_utf8();
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                '"' => out.push('"'),
                '$' => out.push('$'),
                'u' => {
                    if self.src[i..self.end].starts_with('{') {
                        if let Some(close) = self.src[i..self.end].find('}') {
                            let hex = &self.src[i + 1..i + close];
                            match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                Some(c) => out.push(c),
                                None => return Err(Diagnostic::error("E0205", "invalid unicode escape").at(self.sp(esc_start, i + close + 1), "expected \\u{hex} naming a valid character")),
                            }
                            i += close + 1;
                            continue;
                        }
                    }
                    return Err(Diagnostic::error("E0205", "invalid unicode escape").at(self.sp(esc_start, i), "use \\u{1F600}"));
                }
                other => {
                    return Err(Diagnostic::error("E0205", format!("invalid escape `\\{}`", other)).at(self.sp(esc_start, i), "unknown escape").help("valid escapes: \\n \\t \\r \\0 \\\\ \\' \\\" \\$ \\u{...}"));
                }
            }
        }
        self.pos = i;
        Ok(Token { tok: Tok::Str(out), start, end: i })
    }

    fn peek(&mut self) -> PResult<Token> {
        if self.peeked.is_none() {
            let t = self.lex()?;
            self.peeked = Some(t);
        }
        Ok(self.peeked.clone().unwrap_or(Token { tok: Tok::Eof, start: self.pos, end: self.pos }))
    }

    fn next(&mut self) -> PResult<Token> {
        let t = self.peek()?;
        self.peeked = None;
        self.last_end = t.end;
        Ok(t)
    }

    fn unpeek(&mut self) {
        if let Some(t) = self.peeked.take() {
            self.pos = t.start;
        }
    }

    fn is_punct(&mut self, p: &str) -> PResult<bool> {
        Ok(matches!(self.peek()?.tok, Tok::Punct(q) if q == p))
    }

    fn eat_punct(&mut self, p: &str) -> PResult<bool> {
        if self.is_punct(p)? {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn is_word(&mut self, w: &str) -> PResult<bool> {
        Ok(matches!(self.peek()?.tok, Tok::Ident(ref s) if s == w))
    }

    fn describe(t: &Token, src: &str) -> String {
        match &t.tok {
            Tok::Eof => "end of expression".to_string(),
            _ => format!("`{}`", &src[t.start..t.end]),
        }
    }

    fn unexpected(&self, t: &Token, expected: &str) -> Diagnostic {
        if t.tok == Tok::Eof {
            Diagnostic::error("E0203", format!("unexpected end of expression, expected {}", expected)).at(self.sp(t.start, t.end), "the expression ends here")
        } else {
            Diagnostic::error("E0201", format!("unexpected {}, expected {}", Self::describe(t, self.src), expected)).at(self.sp(t.start, t.end), format!("expected {}", expected))
        }
    }

    fn expect_punct(&mut self, p: &'static str) -> PResult<Token> {
        let t = self.next()?;
        if matches!(t.tok, Tok::Punct(q) if q == p) {
            Ok(t)
        } else {
            Err(self.unexpected(&t, &format!("`{}`", p)))
        }
    }

    fn parse_full(&mut self) -> PResult<Expr> {
        let e = self.ternary()?;
        let t = self.peek()?;
        if t.tok != Tok::Eof {
            return Err(self.unexpected(&t, "an operator or end of expression"));
        }
        Ok(e)
    }

    // ───────── grammar ─────────
    fn ternary(&mut self) -> PResult<Expr> {
        let cond = self.binary(1)?;
        if self.is_punct("?")? {
            self.next()?;
            let a = self.ternary()?;
            self.expect_punct(":")?;
            let b = self.ternary()?;
            let span = cond.span.to(b.span);
            return Ok(Expr::new(ExprKind::Ternary(Box::new(cond), Box::new(a), Box::new(b)), span));
        }
        Ok(cond)
    }

    fn binop_of(t: &Tok) -> Option<(u8, Option<BinOp>, bool)> {
        // (precedence, op (None = `is`), right-assoc)
        Some(match t {
            Tok::Punct("??") => (1, Some(BinOp::Coalesce), false),
            Tok::Punct("||") => (2, Some(BinOp::Or), false),
            Tok::Ident(s) if s == "or" => (2, Some(BinOp::Or), false),
            Tok::Punct("&&") => (3, Some(BinOp::And), false),
            Tok::Ident(s) if s == "and" => (3, Some(BinOp::And), false),
            Tok::Punct("==") => (4, Some(BinOp::Eq), false),
            Tok::Punct("!=") => (4, Some(BinOp::Ne), false),
            Tok::Punct("<") => (5, Some(BinOp::Lt), false),
            Tok::Punct("<=") => (5, Some(BinOp::Le), false),
            Tok::Punct(">") => (5, Some(BinOp::Gt), false),
            Tok::Punct(">=") => (5, Some(BinOp::Ge), false),
            Tok::Ident(s) if s == "in" => (5, Some(BinOp::In), false),
            Tok::Ident(s) if s == "is" => (5, None, false),
            Tok::Punct("+") => (6, Some(BinOp::Add), false),
            Tok::Punct("-") => (6, Some(BinOp::Sub), false),
            Tok::Punct("*") => (7, Some(BinOp::Mul), false),
            Tok::Punct("/") => (7, Some(BinOp::Div), false),
            Tok::Punct("%") => (7, Some(BinOp::Mod), false),
            Tok::Punct("**") => (8, Some(BinOp::Pow), true),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let t = self.peek()?;
            let (prec, op, right) = match Self::binop_of(&t.tok) {
                Some(x) if x.0 >= min => x,
                _ => break,
            };
            self.next()?;
            match op {
                Some(op) => {
                    let rhs = self.binary(if right { prec } else { prec + 1 })?;
                    let span = lhs.span.to(rhs.span);
                    lhs = Expr::new(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span);
                }
                None => {
                    let ty = self.type_here(TypeOpts { union: true, optional: true, expr_ctx: true })?;
                    let span = lhs.span.to(self.sp(self.last_end, self.last_end));
                    lhs = Expr::new(ExprKind::Is(Box::new(lhs), ty), span);
                }
            }
        }
        Ok(lhs)
    }

    fn type_here(&mut self, o: TypeOpts) -> PResult<TypeExpr> {
        self.unpeek();
        let mut tp = TypeParser { src: self.src, pos: self.pos, end: self.end, map: self.map };
        let t = tp.parse(o)?;
        self.pos = tp.pos;
        self.last_end = tp.pos;
        Ok(t)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let t = self.peek()?;
        let op = match &t.tok {
            Tok::Punct("-") => Some(Some(UnOp::Neg)),
            Tok::Punct("!") => Some(Some(UnOp::Not)),
            Tok::Ident(s) if s == "not" => Some(Some(UnOp::Not)),
            Tok::Ident(s) if s == "await" => Some(None),
            _ => None,
        };
        if let Some(op) = op {
            self.next()?;
            let operand = self.unary()?;
            let span = self.sp(t.start, t.start).to(operand.span);
            let span = if span.is_dummy() { operand.span } else { Span { start: self.sp(t.start, t.start).start.min(span.start), ..span } };
            return Ok(match op {
                Some(op) => Expr::new(ExprKind::Unary(op, Box::new(operand)), span),
                None => Expr::new(ExprKind::Await(Box::new(operand)), span),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            let t = self.peek()?;
            match &t.tok {
                Tok::Punct(".") | Tok::Punct("?.") => {
                    self.next()?;
                    let optional = t.tok == Tok::Punct("?.");
                    let n = self.next()?;
                    let name = match n.tok {
                        Tok::Ident(s) => s,
                        _ => return Err(self.unexpected(&n, "a member name")),
                    };
                    let span = e.span.to(self.sp(n.start, n.end));
                    e = Expr::new(ExprKind::Member { obj: Box::new(e), name, optional }, span);
                }
                Tok::Punct("(") => {
                    self.next()?;
                    let args = self.args(")")?;
                    let span = e.span.to(self.sp(self.last_end, self.last_end));
                    e = Expr::new(ExprKind::Call { callee: Box::new(e), args }, span);
                }
                Tok::Punct("[") => {
                    self.next()?;
                    let mut from = None;
                    if !self.is_punct(":")? {
                        from = Some(Box::new(self.ternary()?));
                    }
                    if self.eat_punct(":")? {
                        let mut to = None;
                        if !self.is_punct("]")? {
                            to = Some(Box::new(self.ternary()?));
                        }
                        self.expect_punct("]")?;
                        let span = e.span.to(self.sp(self.last_end, self.last_end));
                        e = Expr::new(ExprKind::Slice { obj: Box::new(e), from, to }, span);
                    } else {
                        self.expect_punct("]")?;
                        let span = e.span.to(self.sp(self.last_end, self.last_end));
                        let index = from.unwrap_or_else(|| Box::new(Expr::null(span)));
                        e = Expr::new(ExprKind::Index { obj: Box::new(e), index }, span);
                    }
                }
                Tok::Ident(s) if s == "as" => {
                    self.next()?;
                    let ty = self.type_here(TypeOpts { union: true, optional: true, expr_ctx: true })?;
                    let span = e.span.to(self.sp(self.last_end, self.last_end));
                    e = Expr::new(ExprKind::As(Box::new(e), ty), span);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn args(&mut self, close: &'static str) -> PResult<Vec<Expr>> {
        let mut out = vec![];
        if self.eat_punct(close)? {
            return Ok(out);
        }
        loop {
            out.push(self.ternary()?);
            if self.eat_punct(",")? {
                if self.eat_punct(close)? {
                    break;
                }
                continue;
            }
            self.expect_punct(close)?;
            break;
        }
        Ok(out)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.next()?;
        let span = self.sp(t.start, t.end);
        match t.tok {
            Tok::Int(i) => Ok(Expr::new(ExprKind::Int(i), span)),
            Tok::Float(f) => Ok(Expr::new(ExprKind::Float(f), span)),
            Tok::Str(s) => Ok(Expr::new(ExprKind::Str(s), span)),
            Tok::Punct("(") => {
                let e = self.ternary()?;
                self.expect_punct(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                let items = self.args("]")?;
                let sp = self.sp(t.start, self.last_end);
                Ok(Expr::new(ExprKind::List(items), sp))
            }
            Tok::Punct("{") => self.map_literal(t.start),
            Tok::Ident(name) => match name.as_str() {
                "true" => Ok(Expr::new(ExprKind::Bool(true), span)),
                "false" => Ok(Expr::new(ExprKind::Bool(false), span)),
                "null" => Ok(Expr::new(ExprKind::Null, span)),
                "this" => Ok(Expr::new(ExprKind::This, span)),
                "new" => {
                    let class = self.type_here(TypeOpts { union: false, optional: false, expr_ctx: true })?;
                    self.expect_punct("(")?;
                    let args = self.args(")")?;
                    let sp = self.sp(t.start, self.last_end);
                    Ok(Expr::new(ExprKind::New { class, args }, sp))
                }
                "super" => {
                    self.expect_punct(".")?;
                    let n = self.next()?;
                    let method = match n.tok {
                        Tok::Ident(s) => s,
                        _ => return Err(self.unexpected(&n, "a method name after `super.`")),
                    };
                    self.expect_punct("(")?;
                    let args = self.args(")")?;
                    let sp = self.sp(t.start, self.last_end);
                    Ok(Expr::new(ExprKind::SuperMethod { method, args }, sp))
                }
                "fn" => {
                    if self.is_punct("(")? {
                        self.lambda(t.start)
                    } else {
                        Ok(Expr::new(ExprKind::Ident(name), span))
                    }
                }
                w if KEYWORDS.contains(&w) => Err(Diagnostic::error("E0201", format!("unexpected keyword `{}`", w)).at(span, "an operand is expected here")),
                _ => Ok(Expr::new(ExprKind::Ident(name), span)),
            },
            _ => Err(self.unexpected(&t, "an expression")),
        }
    }

    fn map_literal(&mut self, start: usize) -> PResult<Expr> {
        let mut entries = vec![];
        if !self.eat_punct("}")? {
            loop {
                let kt = self.next()?;
                let ksp = self.sp(kt.start, kt.end);
                let key = match kt.tok {
                    Tok::Ident(s) => Expr::new(ExprKind::Str(s), ksp),
                    Tok::Str(s) => Expr::new(ExprKind::Str(s), ksp),
                    Tok::Int(i) => Expr::new(ExprKind::Str(i.to_string()), ksp),
                    Tok::Punct("[") => {
                        let e = self.ternary()?;
                        self.expect_punct("]")?;
                        e
                    }
                    _ => return Err(self.unexpected(&kt, "a map key (name, string or [expression])")),
                };
                self.expect_punct(":")?;
                let v = self.ternary()?;
                entries.push((key, v));
                if self.eat_punct(",")? {
                    if self.eat_punct("}")? {
                        break;
                    }
                    continue;
                }
                self.expect_punct("}")?;
                break;
            }
        }
        Ok(Expr::new(ExprKind::Map(entries), self.sp(start, self.last_end)))
    }

    fn lambda(&mut self, start: usize) -> PResult<Expr> {
        self.expect_punct("(")?;
        let mut params = vec![];
        if !self.eat_punct(")")? {
            loop {
                let variadic = self.eat_punct("...")?;
                let n = self.next()?;
                let name = match n.tok {
                    Tok::Ident(s) => s,
                    _ => return Err(self.unexpected(&n, "a parameter name")),
                };
                let mut ty = None;
                if self.is_punct(":")? {
                    self.next()?;
                    ty = Some(self.type_here(TypeOpts { union: true, optional: true, expr_ctx: true })?);
                }
                params.push(Param { name, ty, default: None, variadic, span: self.sp(n.start, n.end) });
                if self.eat_punct(",")? {
                    continue;
                }
                self.expect_punct(")")?;
                break;
            }
        }
        self.expect_punct("=>")?;
        let body = self.ternary()?;
        let span = self.sp(start, self.last_end);
        let ret = Stmt::new(StmtKind::Return(Some(body)), span);
        Ok(Expr::new(
            ExprKind::Lambda(Arc::new(FuncDecl {
                name: "<lambda>".to_string(),
                generics: vec![],
                params,
                ret: None,
                body: Some(vec![ret]),
                is_async: false,
                is_static: false,
                is_abstract: false,
                is_override: false,
                is_final: false,
                access: Access::Public,
                doc: None,
                span,
            })),
            span,
        ))
    }
}

/// Parse an expression string. `base` is the span of the JSON string token.
pub fn parse_expression(src: &str, base: Span) -> Result<Expr, Diagnostic> {
    let map = SpanMap::new(base, src.len());
    if src.trim().is_empty() {
        return Err(Diagnostic::error("E0203", "empty expression").at(base, "an expression is expected here"));
    }
    Parser::new(src, 0, src.len(), map).parse_full()
}

fn find_interp_end(src: &str, from: usize, base_map: SpanMap) -> Result<usize, Diagnostic> {
    let mut depth = 1usize;
    let mut chars = src[from..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\'' | '"' => {
                let q = c;
                let mut closed = false;
                while let Some((_, d)) = chars.next() {
                    if d == '\\' {
                        chars.next();
                    } else if d == q {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return Err(Diagnostic::error("E0202", "unterminated string inside `${...}`").at(base_map.span(from + i, from + i + 1), "this string is never closed"));
                }
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(from + i);
                }
            }
            _ => {}
        }
    }
    Err(Diagnostic::error("E0202", "unterminated `${`").at(base_map.span(from.saturating_sub(2), from), "this interpolation is never closed with `}`").help("add a closing `}` (write `$${` for a literal `${`)"))
}

/// Parse a value-position string: `${expr}` interpolation, `$${` escape.
pub fn parse_template(src: &str, base: Span) -> Result<Expr, Diagnostic> {
    let map = SpanMap::new(base, src.len());
    if !src.contains('$') {
        return Ok(Expr::new(ExprKind::Str(src.to_string()), base));
    }
    let mut parts: Vec<TemplatePart> = vec![];
    let mut text = String::new();
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        if rest.starts_with("$${") {
            text.push_str("${");
            i += 3;
        } else if rest.starts_with("${") {
            let close = find_interp_end(src, i + 2, map)?;
            if !text.is_empty() {
                parts.push(TemplatePart::Text(std::mem::take(&mut text)));
            }
            let inner = &src[i + 2..close];
            if inner.trim().is_empty() {
                return Err(Diagnostic::error("E0203", "empty `${}` interpolation").at(map.span(i, close + 1), "an expression is expected inside"));
            }
            let e = Parser::new(src, i + 2, close, map).parse_full()?;
            parts.push(TemplatePart::Expr(e));
            i = close + 1;
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            text.push(c);
            i += c.len_utf8();
        }
    }
    if parts.is_empty() {
        return Ok(Expr::new(ExprKind::Str(text), base));
    }
    if !text.is_empty() {
        parts.push(TemplatePart::Text(text));
    }
    Ok(Expr::new(ExprKind::Template(parts), base))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str) -> Expr {
        parse_expression(s, Span::DUMMY).unwrap_or_else(|d| panic!("{}: {}", s, d))
    }
    /// Fully parenthesised rendering for precedence tests.
    fn show(x: &Expr) -> String {
        match &x.kind {
            ExprKind::Null => "null".into(),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Int(i) => i.to_string(),
            ExprKind::Float(f) => f.to_string(),
            ExprKind::Str(s) => format!("{:?}", s),
            ExprKind::Ident(s) => s.clone(),
            ExprKind::This => "this".into(),
            ExprKind::Unary(op, a) => format!("({}{})", if *op == UnOp::Neg { "-" } else { "!" }, show(a)),
            ExprKind::Binary(op, a, b) => format!("({} {} {})", show(a), op.symbol(), show(b)),
            ExprKind::Ternary(c, a, b) => format!("({} ? {} : {})", show(c), show(a), show(b)),
            ExprKind::Member { obj, name, optional } => format!("{}{}{}", show(obj), if *optional { "?." } else { "." }, name),
            ExprKind::Index { obj, index } => format!("{}[{}]", show(obj), show(index)),
            ExprKind::Slice { obj, from, to } => format!("{}[{}:{}]", show(obj), from.as_ref().map_or(String::new(), |x| show(x)), to.as_ref().map_or(String::new(), |x| show(x))),
            ExprKind::Call { callee, args } => format!("{}({})", show(callee), args.iter().map(show).collect::<Vec<_>>().join(", ")),
            ExprKind::New { class, args } => format!("new {}({})", class.display(), args.iter().map(show).collect::<Vec<_>>().join(", ")),
            ExprKind::List(v) => format!("[{}]", v.iter().map(show).collect::<Vec<_>>().join(", ")),
            ExprKind::Map(v) => format!("{{{}}}", v.iter().map(|(k, v)| format!("{}: {}", show(k), show(v))).collect::<Vec<_>>().join(", ")),
            ExprKind::Is(a, t) => format!("({} is {})", show(a), t.display()),
            ExprKind::As(a, t) => format!("({} as {})", show(a), t.display()),
            ExprKind::Await(a) => format!("(await {})", show(a)),
            ExprKind::SuperMethod { method, args } => format!("super.{}({})", method, args.iter().map(show).collect::<Vec<_>>().join(", ")),
            ExprKind::Lambda(f) => format!("fn({}) => {}", f.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", "), match &f.body {
                Some(b) => match &b[0].kind {
                    StmtKind::Return(Some(x)) => show(x),
                    _ => "?".into(),
                },
                None => "?".into(),
            }),
            ExprKind::Template(_) => "<template>".into(),
        }
    }
    fn s(src: &str) -> String {
        show(&e(src))
    }

    #[test]
    fn literals() {
        assert_eq!(s("1 + 2"), "(1 + 2)");
        assert_eq!(s("0xFF"), "255");
        assert_eq!(s("1_000"), "1000");
        assert_eq!(s("1.5e2"), "150");
        assert_eq!(s("'a\\n' "), "\"a\\n\"");
        assert_eq!(s("\"x\\u{41}\""), "\"xA\"");
        assert_eq!(s("true && null"), "(true && null)");
    }

    #[test]
    fn precedence() {
        assert_eq!(s("1 + 2 * 3"), "(1 + (2 * 3))");
        assert_eq!(s("1 * 2 + 3"), "((1 * 2) + 3)");
        assert_eq!(s("a || b && c"), "(a || (b && c))");
        assert_eq!(s("a or b and c"), "(a || (b && c))");
        assert_eq!(s("a == b && c != d"), "((a == b) && (c != d))");
        assert_eq!(s("a < b == c < d"), "((a < b) == (c < d))");
        assert_eq!(s("a ?? b || c"), "(a ?? (b || c))");
        assert_eq!(s("a + b in c"), "((a + b) in c)");
        assert_eq!(s("2 ** 3 ** 2"), "(2 ** (3 ** 2))");
        assert_eq!(s("-2 ** 2"), "((-2) ** 2)");
        assert_eq!(s("2 ** -1"), "(2 ** (-1))");
        assert_eq!(s("!a && b"), "((!a) && b)");
        assert_eq!(s("not a == b"), "((!a) == b)");
        assert_eq!(s("a - b - c"), "((a - b) - c)");
        assert_eq!(s("a / b % c"), "((a / b) % c)");
        assert_eq!(s("(a + b) * c"), "((a + b) * c)");
    }

    #[test]
    fn ternary_nesting() {
        assert_eq!(s("a ? b : c ? d : e"), "(a ? b : (c ? d : e))");
        assert_eq!(s("a ?? b ? c : d"), "((a ?? b) ? c : d)");
        assert_eq!(s("x > 1 ? 'a' : 'b'"), "((x > 1) ? \"a\" : \"b\")");
    }

    #[test]
    fn postfix_forms() {
        assert_eq!(s("a.b.c"), "a.b.c");
        assert_eq!(s("a?.b"), "a?.b");
        assert_eq!(s("a[0][1]"), "a[0][1]");
        assert_eq!(s("a[1:2]"), "a[1:2]");
        assert_eq!(s("a[:2]"), "a[:2]");
        assert_eq!(s("a[1:]"), "a[1:]");
        assert_eq!(s("a[c ? 1 : 2]"), "a[(c ? 1 : 2)]");
        assert_eq!(s("f(1, 2,)"), "f(1, 2)");
        assert_eq!(s("a.b(1).c[2]"), "a.b(1).c[2]");
        assert_eq!(s("x as int"), "(x as int)");
        assert_eq!(s("x is list<int>"), "(x is list<int>)");
        assert_eq!(s("x is int ? 1 : 2"), "((x is int) ? 1 : 2)");
        assert_eq!(s("x is int || y"), "((x is int) || y)");
        assert_eq!(s("await f()"), "(await f())");
        assert_eq!(s("new Box<int>(1, 2)"), "new Box<int>(1, 2)");
        assert_eq!(s("new util.Box(1)"), "new util.Box(1)");
        assert_eq!(s("super.speak('x')"), "super.speak(\"x\")");
        assert_eq!(s("this.n + 1"), "(this.n + 1)");
    }

    #[test]
    fn collections_and_lambdas() {
        assert_eq!(s("[1, 2, [3],]"), "[1, 2, [3]]");
        assert_eq!(s("{a: 1, 'b c': 2, [k]: 3,}"), "{\"a\": 1, \"b c\": 2, k: 3}");
        assert_eq!(s("fn(a, b) => a + b"), "fn(a, b) => (a + b)");
        assert_eq!(s("fn(a: int, ...rest) => rest"), "fn(a, rest) => rest");
        assert_eq!(s("xs.map(fn(x) => x * 2)"), "xs.map(fn(x) => (x * 2))");
        assert_eq!(s("fn"), "fn");
    }

    #[test]
    fn errors() {
        let code = |src: &str| parse_expression(src, Span::DUMMY).unwrap_err().code;
        assert_eq!(code(""), "E0203");
        assert_eq!(code("1 +"), "E0203");
        assert_eq!(code("1 + * 2"), "E0201");
        assert_eq!(code("(1"), "E0203");
        assert_eq!(code("a b"), "E0201");
        assert_eq!(code("'abc"), "E0202");
        assert_eq!(code("a = 1"), "E0201");
        assert_eq!(code("99999999999999999999"), "E0204");
        assert_eq!(code("'\\q'"), "E0205");
        assert_eq!(code("a @ b"), "E0201");
        assert_eq!(code("and"), "E0201");
        assert_eq!(code("a.1"), "E0201");
        assert_eq!(code("new (1)"), "E0207");
    }

    #[test]
    fn error_spans_inside_json_string() {
        // JSON token `"1 + * 2"` occupies bytes 10..19 (7 chars + 2 quotes)
        let d = parse_expression("1 + * 2", Span::new(0, 10, 19)).unwrap_err();
        let sp = d.primary_span().unwrap();
        assert_eq!((sp.start, sp.end), (10 + 1 + 4, 10 + 1 + 5));
        // with escapes the whole token is used
        let d = parse_expression("1 + * 2", Span::new(0, 10, 25)).unwrap_err();
        assert_eq!(d.primary_span().unwrap(), Span::new(0, 10, 25));
    }

    #[test]
    fn templates() {
        let t = parse_template("hello", Span::DUMMY).unwrap();
        assert!(matches!(t.kind, ExprKind::Str(ref s) if s == "hello"));
        let t = parse_template("a ${x + 1} b ${f('}')}", Span::DUMMY).unwrap();
        match t.kind {
            ExprKind::Template(parts) => {
                assert_eq!(parts.len(), 4);
                assert!(matches!(&parts[0], TemplatePart::Text(s) if s == "a "));
                assert!(matches!(&parts[1], TemplatePart::Expr(_)));
                assert!(matches!(&parts[2], TemplatePart::Text(s) if s == " b "));
            }
            _ => panic!(),
        }
        let single = parse_template("${x}", Span::DUMMY).unwrap();
        assert!(matches!(single.kind, ExprKind::Template(ref p) if p.len() == 1));
        let esc = parse_template("cost: $${5}", Span::DUMMY).unwrap();
        assert!(matches!(esc.kind, ExprKind::Str(ref s) if s == "cost: ${5}"));
        let dollar = parse_template("$5 and $", Span::DUMMY).unwrap();
        assert!(matches!(dollar.kind, ExprKind::Str(ref s) if s == "$5 and $"));
        assert_eq!(parse_template("${", Span::DUMMY).unwrap_err().code, "E0202");
        assert_eq!(parse_template("${}", Span::DUMMY).unwrap_err().code, "E0203");
        assert_eq!(parse_template("${1 +}", Span::DUMMY).unwrap_err().code, "E0203");
        assert_eq!(parse_template("${'a}", Span::DUMMY).unwrap_err().code, "E0202");
    }

    #[test]
    fn template_error_spans() {
        let src = "ab ${1 + * 2}";
        let d = parse_template(src, Span::new(0, 100, 100 + src.len() + 2)).unwrap_err();
        let sp = d.primary_span().unwrap();
        assert_eq!(&src[(sp.start - 101) as usize..(sp.end - 101) as usize], "*");
    }
}
