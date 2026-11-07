//! Parser for type strings: `int`, `list<string>`, `Box<T>?`, `A|B`, `fn(int,int)->int`, `mod.Class<int>`.
//! Error code: E0207.

use crate::{TypeExpr, TypeExprKind};
use nin_diag::{Diagnostic, Span};

#[derive(Clone, Copy)]
pub(crate) struct SpanMap {
    base: Span,
    exact: bool,
}

impl SpanMap {
    pub(crate) fn new(base: Span, src_len: usize) -> SpanMap {
        SpanMap { base, exact: !base.is_dummy() && base.len() == src_len + 2 }
    }
    pub(crate) fn span(&self, a: usize, b: usize) -> Span {
        if self.exact {
            Span { file: self.base.file, start: self.base.start + 1 + a as u32, end: self.base.start + 1 + b.max(a) as u32 }
        } else {
            self.base
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TypeOpts {
    pub union: bool,
    pub optional: bool,
    /// inside an expression: `?` and `<` only bind when adjacent (no whitespace)
    pub expr_ctx: bool,
}

pub(crate) struct TypeParser<'a> {
    pub src: &'a str,
    pub pos: usize,
    pub end: usize,
    pub map: SpanMap,
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}
fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

impl<'a> TypeParser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..self.end]
    }
    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }
    fn err(&self, msg: impl Into<String>, a: usize, b: usize, label: &str) -> Diagnostic {
        Diagnostic::error("E0207", msg).at(self.map.span(a, b), label)
    }
    fn unexpected(&self, what: &str) -> Diagnostic {
        match self.peek() {
            Some(c) => self.err(format!("unexpected `{}` in type, expected {}", c, what), self.pos, self.pos + c.len_utf8(), "unexpected here"),
            None => self.err(format!("unexpected end of type, expected {}", what), self.pos, self.pos, "type ends here"),
        }
    }

    pub(crate) fn parse(&mut self, o: TypeOpts) -> Result<TypeExpr, Diagnostic> {
        self.skip_ws();
        let start = self.pos;
        let mut first = self.parse_optional(o)?;
        if !o.union {
            return Ok(first);
        }
        let mut alts = vec![];
        loop {
            let save = self.pos;
            self.skip_ws();
            if self.rest().starts_with('|') && !self.rest().starts_with("||") {
                self.pos += 1;
                self.skip_ws();
                let next = self.parse_optional(o)?;
                if alts.is_empty() {
                    alts.push(std::mem::replace(&mut first, TypeExpr::named("any", Span::DUMMY)));
                }
                alts.push(next);
            } else {
                self.pos = save;
                break;
            }
        }
        if alts.is_empty() {
            Ok(first)
        } else {
            Ok(TypeExpr::new(TypeExprKind::Union(alts), self.map.span(start, self.pos)))
        }
    }

    fn parse_optional(&mut self, o: TypeOpts) -> Result<TypeExpr, Diagnostic> {
        let start = self.pos;
        let mut t = self.parse_primary(o)?;
        if o.optional {
            loop {
                let save = self.pos;
                if !o.expr_ctx {
                    self.skip_ws();
                }
                if self.rest().starts_with('?') && !self.rest().starts_with("?.") && !self.rest().starts_with("??") {
                    self.pos += 1;
                    t = TypeExpr::new(TypeExprKind::Optional(Box::new(t)), self.map.span(start, self.pos));
                } else {
                    self.pos = save;
                    break;
                }
            }
        }
        Ok(t)
    }

    fn ident(&mut self) -> Result<String, Diagnostic> {
        let start = self.pos;
        match self.peek() {
            Some(c) if is_ident_start(c) => {}
            _ => return Err(self.unexpected("a type name")),
        }
        while let Some(c) = self.peek() {
            if is_ident_char(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        Ok(self.src[start..self.pos].to_string())
    }

    fn parse_primary(&mut self, o: TypeOpts) -> Result<TypeExpr, Diagnostic> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some('(') {
            self.pos += 1;
            let inner = self.parse(TypeOpts { union: true, optional: true, expr_ctx: false })?;
            self.skip_ws();
            if !self.eat(")") {
                return Err(self.unexpected("`)`"));
            }
            return Ok(inner);
        }
        let first = self.ident()?;
        if first == "fn" {
            let save = self.pos;
            self.skip_ws();
            if self.peek() == Some('(') {
                self.pos += 1;
                let mut params = vec![];
                self.skip_ws();
                if !self.eat(")") {
                    loop {
                        params.push(self.parse(TypeOpts { union: true, optional: true, expr_ctx: false })?);
                        self.skip_ws();
                        if self.eat(",") {
                            continue;
                        }
                        if self.eat(")") {
                            break;
                        }
                        return Err(self.unexpected("`,` or `)`"));
                    }
                }
                self.skip_ws();
                if !self.eat("->") {
                    return Err(self.unexpected("`->` and a return type"));
                }
                let ret = self.parse(TypeOpts { union: o.union, optional: o.optional, expr_ctx: o.expr_ctx })?;
                return Ok(TypeExpr::new(TypeExprKind::Func { params, ret: Box::new(ret) }, self.map.span(start, self.pos)));
            }
            self.pos = save;
        }
        let mut path = vec![first];
        while self.peek() == Some('.') {
            let save = self.pos;
            self.pos += 1;
            match self.peek() {
                Some(c) if is_ident_start(c) => path.push(self.ident()?),
                _ => {
                    self.pos = save;
                    break;
                }
            }
        }
        let mut args = vec![];
        let save = self.pos;
        if !o.expr_ctx {
            self.skip_ws();
        }
        if self.peek() == Some('<') {
            self.pos += 1;
            loop {
                args.push(self.parse(TypeOpts { union: true, optional: true, expr_ctx: false })?);
                self.skip_ws();
                if self.eat(",") {
                    continue;
                }
                if self.eat(">") {
                    break;
                }
                return Err(self.unexpected("`,` or `>`"));
            }
        } else {
            self.pos = save;
        }
        Ok(TypeExpr::new(TypeExprKind::Named { path, args }, self.map.span(start, self.pos)))
    }
}

/// Parse a complete type string. `base` is the span of the JSON string token.
pub fn parse_type(src: &str, base: Span) -> Result<TypeExpr, Diagnostic> {
    let map = SpanMap::new(base, src.len());
    let mut p = TypeParser { src, pos: 0, end: src.len(), map };
    let t = p.parse(TypeOpts { union: true, optional: true, expr_ctx: false })?;
    p.skip_ws();
    if p.pos < p.end {
        let c = p.peek().unwrap_or(' ');
        return Err(p.err(format!("unexpected `{}` after the type", c), p.pos, p.pos + c.len_utf8(), "unexpected here").help("types look like `int`, `list<string>`, `Box<T>?`, `int|string`, `fn(int)->int`"));
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<TypeExpr, Diagnostic> {
        parse_type(s, Span::DUMMY)
    }

    #[test]
    fn roundtrips() {
        for s in ["int", "list<int>", "map<string,list<int>>", "Box<T>?", "int|string", "fn(int,int)->int", "mod.Class<int>", "fn()->void", "task<list<string>>?"] {
            assert_eq!(p(s).unwrap().display(), s, "{}", s);
        }
        assert_eq!(p("map< string , int >").unwrap().display(), "map<string,int>");
        assert_eq!(p("(int|string)?").unwrap().display(), "int|string?".replace("int|string?", "int|string?"));
    }

    #[test]
    fn kinds() {
        assert!(matches!(p("int?").unwrap().kind, TypeExprKind::Optional(_)));
        assert!(matches!(p("a|b|c").unwrap().kind, TypeExprKind::Union(v) if v.len() == 3));
        assert!(matches!(p("fn(int)->string").unwrap().kind, TypeExprKind::Func { .. }));
    }

    #[test]
    fn errors() {
        assert_eq!(p("").unwrap_err().code, "E0207");
        assert_eq!(p("list<").unwrap_err().code, "E0207");
        assert_eq!(p("int int").unwrap_err().code, "E0207");
        assert_eq!(p("fn(int)").unwrap_err().code, "E0207");
        assert_eq!(p("1x").unwrap_err().code, "E0207");
    }

    #[test]
    fn spans_inside_json_string() {
        let sm = SpanMap::new(Span::new(0, 10, 18), 6);
        assert_eq!(sm.span(2, 4), Span::new(0, 13, 15));
        let sm2 = SpanMap::new(Span::new(0, 10, 30), 6);
        assert_eq!(sm2.span(2, 4), Span::new(0, 10, 30));
    }
}
