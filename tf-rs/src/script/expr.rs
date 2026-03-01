//! TF expression lexer, AST, parser, and evaluator.
//!
//! TF's expression language (evaluated by `/expr` and `$[...]` in the C source)
//! supports integer/float arithmetic, string comparison, ternary operator,
//! assignment, glob (`=~`) and regex (`=/`) matching, and function calls.
//!
//! Operator precedence (lowest → highest):
//!   comma  →  assign  →  ternary  →  or  →  and  →  relational  →
//!   additive  →  multiplicative  →  unary  →  postfix  →  primary

use crate::pattern::{MatchMode, Pattern};
use super::value::Value;

// ── EvalContext ───────────────────────────────────────────────────────────────

/// Dependency-injection interface used by the expression evaluator.
///
/// An [`Interpreter`](super::interp::Interpreter) implements this trait to give
/// the evaluator access to variables, positional parameters, and built-in or
/// user-defined functions.
pub trait EvalContext {
    /// Look up a variable (local scope first, then global).
    fn get_var(&self, name: &str) -> Option<Value>;

    /// Set a local-scope variable.
    fn set_local(&mut self, name: &str, value: Value);

    /// Set a global variable.
    fn set_global(&mut self, name: &str, value: Value);

    /// Positional parameters (`{1}`, `{2}`, …).
    fn positional_params(&self) -> &[String];

    /// Name of the currently executing command/macro (for `{P}`).
    fn current_cmd_name(&self) -> &str;

    /// Invoke a built-in or user-defined function.
    fn call_fn(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String>;

    /// Evaluate a string as a TF expression (used by `$[...]`).
    fn eval_expr_str(&mut self, s: &str) -> Result<Value, String>;
}

// ── Token ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Tilde,
    Ampersand,
    Pipe,
    Caret,
    ShiftLeft,
    ShiftRight,

    // Comparison
    Eq, // ==
    Ne, // !=
    Lt,
    Le,
    Gt,
    Ge,
    GlobMatch,    // =~
    RegexMatch,   // =/
    NotGlobMatch,  // !~
    NotRegexMatch, // !/

    // Logical
    And, // &&
    Or,  // ||

    // Assignment
    Assign,        // =
    PlusAssign,    // +=
    MinusAssign,   // -=
    StarAssign,    // *=
    SlashAssign,   // /=
    PercentAssign, // %=

    // Misc
    Question,
    Colon,
    Comma,
    LParen,
    RParen,
    /// Unrecognised input byte — reported as a diagnostic instead of masking as EOF.
    Unknown(char),
    Eof,
}

// ── Lexer ─────────────────────────────────────────────────────────────────────

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            src: src.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let ch = self.src.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn eat(&mut self, ch: u8) -> bool {
        if self.peek() == Some(ch) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn read_number(&mut self, first: u8) -> Token {
        let mut s = String::new();
        s.push(first as char);
        let mut is_float = false;

        // Hex literal
        if first == b'0' && matches!(self.peek(), Some(b'x' | b'X')) {
            s.push(self.advance().unwrap() as char);
            while matches!(self.peek(), Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F')) {
                s.push(self.advance().unwrap() as char);
            }
            let hex = &s[2..];
            return Token::Int(i64::from_str_radix(hex, 16).unwrap_or(0));
        }

        while matches!(self.peek(), Some(b'0'..=b'9')) {
            s.push(self.advance().unwrap() as char);
        }
        if self.peek() == Some(b'.') && matches!(self.peek2(), Some(b'0'..=b'9')) {
            is_float = true;
            s.push(self.advance().unwrap() as char);
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                s.push(self.advance().unwrap() as char);
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            s.push(self.advance().unwrap() as char);
            if matches!(self.peek(), Some(b'+' | b'-')) {
                s.push(self.advance().unwrap() as char);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                s.push(self.advance().unwrap() as char);
            }
        }

        if is_float {
            Token::Float(s.parse().unwrap_or(0.0))
        } else {
            Token::Int(s.parse().unwrap_or(0))
        }
    }

    fn read_string(&mut self, quote: u8) -> Token {
        let mut s = String::new();
        loop {
            match self.advance() {
                None | Some(b'\n') => break,
                Some(b'\\') => match self.advance() {
                    Some(b'n') => s.push('\n'),
                    Some(b't') => s.push('\t'),
                    Some(c) => s.push(c as char),
                    None => break,
                },
                Some(c) if c == quote => break,
                Some(c) => s.push(c as char),
            }
        }
        Token::Str(s)
    }

    fn read_ident(&mut self, first: u8) -> Token {
        let mut s = String::new();
        s.push(first as char);
        while matches!(
            self.peek(),
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
        ) {
            s.push(self.advance().unwrap() as char);
        }
        Token::Ident(s)
    }

    fn next_token(&mut self) -> Token {
        self.skip_ws();
        let ch = match self.advance() {
            None => return Token::Eof,
            Some(c) => c,
        };

        match ch {
            b'0'..=b'9' => self.read_number(ch),
            b'"' => self.read_string(b'"'),
            b'\'' => self.read_string(b'\''),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.read_ident(ch),
            b'+' => {
                if self.eat(b'=') {
                    Token::PlusAssign
                } else {
                    Token::Plus
                }
            }
            b'-' => {
                if self.eat(b'=') {
                    Token::MinusAssign
                } else {
                    Token::Minus
                }
            }
            b'*' => {
                if self.eat(b'=') {
                    Token::StarAssign
                } else {
                    Token::Star
                }
            }
            b'/' => {
                if self.eat(b'=') {
                    Token::SlashAssign
                } else {
                    Token::Slash
                }
            }
            b'%' => {
                if self.eat(b'=') {
                    Token::PercentAssign
                } else {
                    Token::Percent
                }
            }
            b'!' => {
                if self.eat(b'=') {
                    Token::Ne
                } else if self.eat(b'~') {
                    Token::NotGlobMatch
                } else if self.eat(b'/') {
                    Token::NotRegexMatch
                } else {
                    Token::Bang
                }
            }
            b'~' => Token::Tilde,
            b'^' => Token::Caret,
            b'&' => {
                if self.eat(b'&') {
                    Token::And
                } else {
                    Token::Ampersand
                }
            }
            b'|' => {
                if self.eat(b'|') {
                    Token::Or
                } else {
                    Token::Pipe
                }
            }
            b'<' => {
                if self.eat(b'<') {
                    Token::ShiftLeft
                } else if self.eat(b'=') {
                    Token::Le
                } else {
                    Token::Lt
                }
            }
            b'>' => {
                if self.eat(b'>') {
                    Token::ShiftRight
                } else if self.eat(b'=') {
                    Token::Ge
                } else {
                    Token::Gt
                }
            }
            b'=' => {
                if self.eat(b'=') {
                    Token::Eq
                } else if self.eat(b'~') {
                    Token::GlobMatch
                } else if self.eat(b'/') {
                    Token::RegexMatch
                } else {
                    Token::Assign
                }
            }
            b'?' => Token::Question,
            b':' => Token::Colon,
            b',' => Token::Comma,
            b'(' => Token::LParen,
            b')' => Token::RParen,
            c => Token::Unknown(c as char),
        }
    }

    fn tokenize(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let t = self.next_token();
            let done = matches!(t, Token::Eof);
            tokens.push(t);
            if done {
                break;
            }
        }
        tokens
    }
}

// ── AST ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    GlobMatch,
    RegexMatch,
    NotGlobMatch,
    NotRegexMatch,
}

#[derive(Debug, Clone)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Var(String),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Assign(String, AssignOp, Box<Expr>),
    Call(String, Vec<Expr>),
    Comma(Vec<Expr>),
}

// ── Parser ────────────────────────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        t
    }

    fn eat(&mut self, expected: &Token) -> bool {
        if self.peek() == expected {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    // ── Grammar ───────────────────────────────────────────────────────────────

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_comma()
    }

    fn parse_comma(&mut self) -> Result<Expr, String> {
        let first = self.parse_assign()?;
        if self.peek() == &Token::Comma {
            let mut exprs = vec![first];
            while self.eat(&Token::Comma) {
                exprs.push(self.parse_assign()?);
            }
            Ok(Expr::Comma(exprs))
        } else {
            Ok(first)
        }
    }

    fn parse_assign(&mut self) -> Result<Expr, String> {
        // Look-ahead: if next token is Ident followed by an assign op, parse as assignment.
        if let Token::Ident(name) = self.peek().clone() {
            let op = match self.tokens.get(self.pos + 1) {
                Some(Token::Assign) => Some(AssignOp::Set),
                Some(Token::PlusAssign) => Some(AssignOp::Add),
                Some(Token::MinusAssign) => Some(AssignOp::Sub),
                Some(Token::StarAssign) => Some(AssignOp::Mul),
                Some(Token::SlashAssign) => Some(AssignOp::Div),
                Some(Token::PercentAssign) => Some(AssignOp::Rem),
                _ => None,
            };
            if let Some(op) = op {
                self.pos += 2; // consume ident + assign-op
                let rhs = self.parse_assign()?;
                return Ok(Expr::Assign(name, op, Box::new(rhs)));
            }
        }
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_or()?;
        if self.eat(&Token::Question) {
            let then = self.parse_or()?;
            if !self.eat(&Token::Colon) {
                return Err("expected ':' in ternary".into());
            }
            let else_ = self.parse_ternary()?;
            Ok(Expr::Ternary(
                Box::new(cond),
                Box::new(then),
                Box::new(else_),
            ))
        } else {
            Ok(cond)
        }
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_and()?;
        while self.eat(&Token::Or) {
            let rhs = self.parse_and()?;
            lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_relational()?;
        while self.eat(&Token::And) {
            let rhs = self.parse_relational()?;
            lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_relational(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_bitor()?;
        loop {
            let op = match self.peek() {
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Lt => BinOp::Lt,
                Token::Le => BinOp::Le,
                Token::Gt => BinOp::Gt,
                Token::Ge => BinOp::Ge,
                Token::GlobMatch => BinOp::GlobMatch,
                Token::RegexMatch => BinOp::RegexMatch,
                Token::NotGlobMatch => BinOp::NotGlobMatch,
                Token::NotRegexMatch => BinOp::NotRegexMatch,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_bitor()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_bitor(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_bitxor()?;
        while self.eat(&Token::Pipe) {
            let rhs = self.parse_bitxor()?;
            lhs = Expr::Binary(BinOp::BitOr, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_bitxor(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_bitand()?;
        while self.eat(&Token::Caret) {
            let rhs = self.parse_bitand()?;
            lhs = Expr::Binary(BinOp::BitXor, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_bitand(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_shift()?;
        while self.eat(&Token::Ampersand) {
            let rhs = self.parse_shift()?;
            lhs = Expr::Binary(BinOp::BitAnd, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_shift(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Token::ShiftLeft => BinOp::Shl,
                Token::ShiftRight => BinOp::Shr,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Rem,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Token::Minus => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Neg, Box::new(self.parse_unary()?)))
            }
            Token::Bang => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Not, Box::new(self.parse_unary()?)))
            }
            Token::Tilde => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::BitNot, Box::new(self.parse_unary()?)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        let tok = self.advance();
        match tok {
            Token::Int(n) => Ok(Expr::Literal(Value::Int(n))),
            Token::Float(x) => Ok(Expr::Literal(Value::Float(x))),
            Token::Str(s) => Ok(Expr::Literal(Value::Str(s))),
            Token::Ident(name) => {
                if self.eat(&Token::LParen) {
                    // Function call
                    let mut args = Vec::new();
                    if self.peek() != &Token::RParen {
                        args.push(self.parse_assign()?);
                        while self.eat(&Token::Comma) {
                            args.push(self.parse_assign()?);
                        }
                    }
                    if !self.eat(&Token::RParen) {
                        return Err(format!("expected ')' after args to {name}"));
                    }
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Token::LParen => {
                let inner = self.parse_expr()?;
                if !self.eat(&Token::RParen) {
                    return Err("expected ')'".into());
                }
                Ok(inner)
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }
}

/// Parse a TF expression string into an AST.
pub fn parse_expr(src: &str) -> Result<Expr, String> {
    let tokens = Lexer::new(src).tokenize();
    let mut parser = Parser::new(tokens);
    let expr = parser.parse_expr()?;
    Ok(expr)
}

// ── Evaluator ─────────────────────────────────────────────────────────────────

/// Evaluate an [`Expr`] AST node against the given context.
pub fn eval_expr(expr: &Expr, ctx: &mut dyn EvalContext) -> Result<Value, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),

        Expr::Var(name) => Ok(ctx.get_var(name).unwrap_or_default()),

        Expr::Unary(op, inner) => {
            let v = eval_expr(inner, ctx)?;
            Ok(match op {
                UnaryOp::Neg => v.arith_neg(),
                UnaryOp::Not => Value::Int(if v.as_bool() { 0 } else { 1 }),
                UnaryOp::BitNot => Value::Int(!v.as_int()),
            })
        }

        Expr::Binary(op, lhs, rhs) => {
            // Short-circuit for && and ||
            match op {
                BinOp::And => {
                    let l = eval_expr(lhs, ctx)?;
                    if !l.as_bool() {
                        return Ok(Value::Int(0));
                    }
                    let r = eval_expr(rhs, ctx)?;
                    return Ok(Value::Int(if r.as_bool() { 1 } else { 0 }));
                }
                BinOp::Or => {
                    let l = eval_expr(lhs, ctx)?;
                    if l.as_bool() {
                        return Ok(Value::Int(1));
                    }
                    let r = eval_expr(rhs, ctx)?;
                    return Ok(Value::Int(if r.as_bool() { 1 } else { 0 }));
                }
                _ => {}
            }
            let l = eval_expr(lhs, ctx)?;
            let r = eval_expr(rhs, ctx)?;
            eval_binop(op, l, r, ctx)
        }

        Expr::Ternary(cond, then, else_) => {
            let c = eval_expr(cond, ctx)?;
            if c.as_bool() {
                eval_expr(then, ctx)
            } else {
                eval_expr(else_, ctx)
            }
        }

        Expr::Assign(name, op, rhs) => {
            let rval = eval_expr(rhs, ctx)?;
            let new_val = if let AssignOp::Set = op {
                rval.clone()
            } else {
                let cur = ctx.get_var(name).unwrap_or_default();
                match op {
                    AssignOp::Add => cur.arith_add(&rval),
                    AssignOp::Sub => cur.arith_sub(&rval),
                    AssignOp::Mul => cur.arith_mul(&rval),
                    AssignOp::Div => cur.arith_div(&rval)?,
                    AssignOp::Rem => cur.arith_rem(&rval)?,
                    AssignOp::Set => unreachable!(),
                }
            };
            ctx.set_local(name, new_val.clone());
            Ok(new_val)
        }

        Expr::Call(name, arg_exprs) => {
            let mut args = Vec::with_capacity(arg_exprs.len());
            for ae in arg_exprs {
                args.push(eval_expr(ae, ctx)?);
            }
            ctx.call_fn(name, args)
        }

        Expr::Comma(exprs) => {
            let mut last = Value::default();
            for e in exprs {
                last = eval_expr(e, ctx)?;
            }
            Ok(last)
        }
    }
}

fn eval_binop(op: &BinOp, l: Value, r: Value, _ctx: &mut dyn EvalContext) -> Result<Value, String> {
    use std::cmp::Ordering;
    match op {
        BinOp::Add => Ok(l.arith_add(&r)),
        BinOp::Sub => Ok(l.arith_sub(&r)),
        BinOp::Mul => Ok(l.arith_mul(&r)),
        BinOp::Div => l.arith_div(&r),
        BinOp::Rem => l.arith_rem(&r),

        BinOp::Eq => Ok(Value::Int(if l.cmp_value(&r) == Ordering::Equal {
            1
        } else {
            0
        })),
        BinOp::Ne => Ok(Value::Int(if l.cmp_value(&r) != Ordering::Equal {
            1
        } else {
            0
        })),
        BinOp::Lt => Ok(Value::Int(if l.cmp_value(&r) == Ordering::Less {
            1
        } else {
            0
        })),
        BinOp::Le => Ok(Value::Int(
            if matches!(l.cmp_value(&r), Ordering::Less | Ordering::Equal) {
                1
            } else {
                0
            },
        )),
        BinOp::Gt => Ok(Value::Int(if l.cmp_value(&r) == Ordering::Greater {
            1
        } else {
            0
        })),
        BinOp::Ge => Ok(Value::Int(
            if matches!(l.cmp_value(&r), Ordering::Greater | Ordering::Equal) {
                1
            } else {
                0
            },
        )),

        BinOp::BitAnd => Ok(Value::Int(l.as_int() & r.as_int())),
        BinOp::BitOr => Ok(Value::Int(l.as_int() | r.as_int())),
        BinOp::BitXor => Ok(Value::Int(l.as_int() ^ r.as_int())),
        BinOp::Shl => Ok(Value::Int(l.as_int() << (r.as_int() & 63))),
        BinOp::Shr => Ok(Value::Int(l.as_int() >> (r.as_int() & 63))),

        BinOp::GlobMatch => {
            let text = l.as_str();
            let pattern = r.as_str();
            Ok(Value::Int(if glob_match(&pattern, &text) { 1 } else { 0 }))
        }
        BinOp::RegexMatch => {
            let text = l.as_str();
            let pattern = r.as_str();
            Ok(Value::Int(if regex_match(&pattern, &text) { 1 } else { 0 }))
        }
        BinOp::NotGlobMatch => {
            let text = l.as_str();
            let pattern = r.as_str();
            Ok(Value::Int(if glob_match(&pattern, &text) { 0 } else { 1 }))
        }
        BinOp::NotRegexMatch => {
            let text = l.as_str();
            let pattern = r.as_str();
            Ok(Value::Int(if regex_match(&pattern, &text) { 0 } else { 1 }))
        }

        BinOp::And | BinOp::Or => unreachable!("handled above"),
    }
}

// ── Simple glob matcher ───────────────────────────────────────────────────────

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_inner(&p, &t)
}

fn glob_match_inner(p: &[char], t: &[char]) -> bool {
    match (p.first(), t.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            // Skip consecutive stars
            let rest_p = p
                .iter()
                .position(|&c| c != '*')
                .map(|i| &p[i..])
                .unwrap_or(&[]);
            // Try matching rest_p against every suffix of t
            for i in 0..=t.len() {
                if glob_match_inner(rest_p, &t[i..]) {
                    return true;
                }
            }
            false
        }
        (Some('?'), Some(_)) => glob_match_inner(&p[1..], &t[1..]),
        (Some(pc), Some(tc)) if pc == tc => glob_match_inner(&p[1..], &t[1..]),
        _ => false,
    }
}

fn regex_match(pattern: &str, text: &str) -> bool {
    Pattern::new(pattern, MatchMode::Regexp)
        .map(|p| p.matches(text))
        .unwrap_or(false)
}

/// Convenience: parse and evaluate a TF expression string.
pub fn eval_str(src: &str, ctx: &mut dyn EvalContext) -> Result<Value, String> {
    let expr = parse_expr(src)?;
    eval_expr(&expr, ctx)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── Minimal EvalContext for tests ─────────────────────────────────────────

    struct TestCtx {
        vars: HashMap<String, Value>,
    }

    impl TestCtx {
        fn new() -> Self {
            TestCtx {
                vars: HashMap::new(),
            }
        }
        fn with(mut self, k: &str, v: Value) -> Self {
            self.vars.insert(k.into(), v);
            self
        }
    }

    impl EvalContext for TestCtx {
        fn get_var(&self, name: &str) -> Option<Value> {
            self.vars.get(name).cloned()
        }
        fn set_local(&mut self, name: &str, value: Value) {
            self.vars.insert(name.into(), value);
        }
        fn set_global(&mut self, name: &str, value: Value) {
            self.vars.insert(name.into(), value);
        }
        fn positional_params(&self) -> &[String] {
            &[]
        }
        fn current_cmd_name(&self) -> &str {
            ""
        }
        fn call_fn(&mut self, _name: &str, _args: Vec<Value>) -> Result<Value, String> {
            Err("no functions in test ctx".into())
        }
        fn eval_expr_str(&mut self, s: &str) -> Result<Value, String> {
            eval_str(s, self)
        }
    }

    fn eval(src: &str) -> Value {
        eval_str(src, &mut TestCtx::new()).expect("eval failed")
    }

    fn eval_ctx(src: &str, ctx: &mut TestCtx) -> Value {
        eval_str(src, ctx).expect("eval failed")
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn literals() {
        assert_eq!(eval("42"), Value::Int(42));
        assert_eq!(eval("3.14"), Value::Float(3.14));
        assert_eq!(eval("\"hello\""), Value::Str("hello".into()));
    }

    #[test]
    fn arithmetic() {
        assert_eq!(eval("2 + 3"), Value::Int(5));
        assert_eq!(eval("10 - 4"), Value::Int(6));
        assert_eq!(eval("3 * 4"), Value::Int(12));
        assert_eq!(eval("10 / 3"), Value::Int(3));
        assert_eq!(eval("10 % 3"), Value::Int(1));
    }

    #[test]
    fn unary_neg() {
        assert_eq!(eval("-5"), Value::Int(-5));
        assert_eq!(eval("-(3 + 2)"), Value::Int(-5));
    }

    #[test]
    fn logical_not() {
        assert_eq!(eval("!0"), Value::Int(1));
        assert_eq!(eval("!1"), Value::Int(0));
    }

    #[test]
    fn comparison() {
        assert_eq!(eval("3 == 3"), Value::Int(1));
        assert_eq!(eval("3 != 4"), Value::Int(1));
        assert_eq!(eval("2 < 3"), Value::Int(1));
        assert_eq!(eval("3 >= 3"), Value::Int(1));
    }

    #[test]
    fn ternary() {
        assert_eq!(eval("1 ? 10 : 20"), Value::Int(10));
        assert_eq!(eval("0 ? 10 : 20"), Value::Int(20));
    }

    #[test]
    fn logical_and_or() {
        assert_eq!(eval("1 && 1"), Value::Int(1));
        assert_eq!(eval("1 && 0"), Value::Int(0));
        assert_eq!(eval("0 || 1"), Value::Int(1));
        assert_eq!(eval("0 || 0"), Value::Int(0));
    }

    #[test]
    fn variable_lookup() {
        let mut ctx = TestCtx::new().with("x", Value::Int(7));
        assert_eq!(eval_ctx("x + 1", &mut ctx), Value::Int(8));
    }

    #[test]
    fn assignment() {
        let mut ctx = TestCtx::new();
        eval_ctx("x = 5", &mut ctx);
        assert_eq!(ctx.vars.get("x"), Some(&Value::Int(5)));
    }

    #[test]
    fn compound_assignment() {
        let mut ctx = TestCtx::new().with("x", Value::Int(10));
        eval_ctx("x += 5", &mut ctx);
        assert_eq!(ctx.vars.get("x"), Some(&Value::Int(15)));
    }

    #[test]
    fn glob_match_op() {
        assert_eq!(eval("\"hello\" =~ \"hel*\""), Value::Int(1));
        assert_eq!(eval("\"hello\" =~ \"xyz*\""), Value::Int(0));
        assert_eq!(eval("\"hello\" =~ \"h?llo\""), Value::Int(1));
    }

    #[test]
    fn not_glob_match_op() {
        assert_eq!(eval("\"hello\" !~ \"hel*\""), Value::Int(0));
        assert_eq!(eval("\"hello\" !~ \"xyz*\""), Value::Int(1));
    }

    #[test]
    fn not_regex_match_op() {
        assert_eq!(eval("\"hello world\" !/ \"world\""), Value::Int(0));
        assert_eq!(eval("\"hello\" !/ \"xyz\""), Value::Int(1));
        // Real regex matching — dot matches any char
        assert_eq!(eval("\"hello\" =/ \"hel.o\""), Value::Int(1));
        assert_eq!(eval("\"hello\" =/ \"^hell$\""), Value::Int(0)); // anchored, no full match
        assert_eq!(eval("\"hello\" =/ \"^hello$\""), Value::Int(1));
    }

    #[test]
    fn hex_literal() {
        assert_eq!(eval("0xff"), Value::Int(255));
        assert_eq!(eval("0x10"), Value::Int(16));
    }

    #[test]
    fn bitwise() {
        assert_eq!(eval("5 & 3"), Value::Int(1));
        assert_eq!(eval("5 | 2"), Value::Int(7));
        assert_eq!(eval("5 ^ 3"), Value::Int(6));
        assert_eq!(eval("1 << 3"), Value::Int(8));
        assert_eq!(eval("8 >> 2"), Value::Int(2));
    }

    #[test]
    fn precedence() {
        assert_eq!(eval("2 + 3 * 4"), Value::Int(14));
        assert_eq!(eval("(2 + 3) * 4"), Value::Int(20));
    }

    #[test]
    fn glob_star() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("foo*", "foobar"));
        assert!(!glob_match("foo*", "barfoo"));
        assert!(glob_match("*bar", "foobar"));
        assert!(glob_match("f*r", "foobar"));
    }
}
