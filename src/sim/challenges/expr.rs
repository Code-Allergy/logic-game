//! The expressions of a challenge's `formula` test: what an output should
//! be, written over the input ports (`"A + B + Cin"`, `"Sel ? B : A"`).
//!
//! Values are 64-bit unsigned integers with wrapping arithmetic; the result
//! is cut to the output's width by the caller. Operators, loosest first:
//! `c ? a : b`; `||`; `&&`; `|`; `^`; `&`; `==` `!=`; `<` `<=` `>` `>=`;
//! `<<` `>>`; `+` `-`; `*` `/` `%`; unary `~` (bitwise not), `!` (1 when
//! zero, else 0) and `-`. Comparisons and `!`, `&&`, `||` give 0 or 1.
//! Numbers are decimal, `0x…` or `0b…` (with `_` separators); names are
//! letters, digits and `_`.

use std::fmt;

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(u64),
    Var(String),
    Unary(char, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExprError(pub String);

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(u64),
    Name(String),
    Op(&'static str),
}

const OPS: [&str; 24] = [
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "&", "|", "^", "~",
    "!", "<", ">", "(", ")", "?", ":",
];

fn tokens(text: &str) -> Result<Vec<Tok>, ExprError> {
    let mut out = Vec::new();
    let mut rest = text.trim_start();
    while let Some(c) = rest.chars().next() {
        if c.is_ascii_digit() {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let value = super::parse_int(word)
                .ok_or_else(|| ExprError(format!("\"{word}\" is not a number")))?;
            out.push(Tok::Num(value));
            rest = &rest[end..];
        } else if c.is_ascii_alphabetic() || c == '_' {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            out.push(Tok::Name(rest[..end].to_string()));
            rest = &rest[end..];
        } else if let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) {
            out.push(Tok::Op(op));
            rest = &rest[op.len()..];
        } else {
            return Err(ExprError(format!("unexpected \"{c}\"")));
        }
        rest = rest.trim_start();
    }
    Ok(out)
}

/// Binary operators by binding strength, loosest first.
const LEVELS: [&[&str]; 10] = [
    &["||"],
    &["&&"],
    &["|"],
    &["^"],
    &["&"],
    &["==", "!="],
    &["<", "<=", ">", ">="],
    &["<<", ">>"],
    &["+", "-"],
    &["*", "/", "%"],
];

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn cond(&mut self) -> Result<Expr, ExprError> {
        let c = self.binary(0)?;
        if self.eat("?") {
            let a = self.cond()?;
            if !self.eat(":") {
                return Err(ExprError("\"?\" without \":\"".into()));
            }
            let b = self.cond()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, level: usize) -> Result<Expr, ExprError> {
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) if LEVELS[level].contains(o) => *o,
                _ => return Ok(lhs),
            };
            self.at += 1;
            let rhs = self.binary(level + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn unary(&mut self) -> Result<Expr, ExprError> {
        for op in ['~', '!', '-'] {
            if self.eat(&op.to_string()) {
                return Ok(Expr::Unary(op, Box::new(self.unary()?)));
            }
        }
        match self.toks.get(self.at).cloned() {
            Some(Tok::Num(v)) => {
                self.at += 1;
                Ok(Expr::Num(v))
            }
            Some(Tok::Name(n)) => {
                self.at += 1;
                Ok(Expr::Var(n))
            }
            Some(Tok::Op("(")) => {
                self.at += 1;
                let e = self.cond()?;
                if !self.eat(")") {
                    return Err(ExprError("missing \")\"".into()));
                }
                Ok(e)
            }
            Some(Tok::Op(o)) => Err(ExprError(format!("unexpected \"{o}\""))),
            None => Err(ExprError("unexpected end".into())),
        }
    }
}

impl Expr {
    pub fn parse(text: &str) -> Result<Expr, ExprError> {
        let mut p = Parser {
            toks: tokens(text)?,
            at: 0,
        };
        let e = p.cond()?;
        match p.peek() {
            None => Ok(e),
            Some(t) => Err(ExprError(format!("unexpected {t:?} after the expression"))),
        }
    }

    /// Every name the expression reads.
    pub fn names(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.visit(&mut |e| {
            if let Expr::Var(n) = e {
                out.push(n.as_str());
            }
        });
        out
    }

    fn visit<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        f(self);
        match self {
            Expr::Num(_) | Expr::Var(_) => {}
            Expr::Unary(_, a) => a.visit(f),
            Expr::Binary(_, a, b) => {
                a.visit(f);
                b.visit(f);
            }
            Expr::Cond(c, a, b) => {
                c.visit(f);
                a.visit(f);
                b.visit(f);
            }
        }
    }

    /// The value with `var` giving each name's value (0 for unknown ones;
    /// names are checked when a challenge loads).
    pub fn eval(&self, var: &impl Fn(&str) -> u64) -> u64 {
        let b = |v: bool| u64::from(v);
        match self {
            Expr::Num(v) => *v,
            Expr::Var(n) => var(n),
            Expr::Unary(op, a) => {
                let a = a.eval(var);
                match op {
                    '~' => !a,
                    '!' => b(a == 0),
                    _ => a.wrapping_neg(),
                }
            }
            Expr::Binary(op, x, y) => {
                let (x, y) = (x.eval(var), y.eval(var));
                match *op {
                    "||" => b(x != 0 || y != 0),
                    "&&" => b(x != 0 && y != 0),
                    "|" => x | y,
                    "^" => x ^ y,
                    "&" => x & y,
                    "==" => b(x == y),
                    "!=" => b(x != y),
                    "<" => b(x < y),
                    "<=" => b(x <= y),
                    ">" => b(x > y),
                    ">=" => b(x >= y),
                    "<<" => x.checked_shl(y.min(64) as u32).unwrap_or(0),
                    ">>" => x.checked_shr(y.min(64) as u32).unwrap_or(0),
                    "+" => x.wrapping_add(y),
                    "-" => x.wrapping_sub(y),
                    "*" => x.wrapping_mul(y),
                    "/" => x.checked_div(y).unwrap_or(0),
                    _ => x.checked_rem(y).unwrap_or(0),
                }
            }
            Expr::Cond(c, a, b) => {
                if c.eval(var) != 0 {
                    a.eval(var)
                } else {
                    b.eval(var)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(text: &str, vars: &[(&str, u64)]) -> u64 {
        let e = Expr::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        e.eval(&|n| vars.iter().find(|(k, _)| *k == n).map_or(0, |(_, v)| *v))
    }

    #[test]
    fn precedence_and_operators() {
        assert_eq!(eval("1 + 2 * 3", &[]), 7);
        assert_eq!(eval("(1 + 2) * 3", &[]), 9);
        assert_eq!(
            eval("A + B + Cin", &[("A", 200), ("B", 100), ("Cin", 1)]),
            301
        );
        assert_eq!(eval("(A + B) >> 8", &[("A", 200), ("B", 100)]), 1);
        assert_eq!(eval("A & B | C", &[("A", 1), ("B", 0), ("C", 1)]), 1);
        assert_eq!(eval("A ^ B", &[("A", 0b1100), ("B", 0b1010)]), 0b0110);
        assert_eq!(eval("~A & 0xF", &[("A", 0b0101)]), 0b1010);
        assert_eq!(eval("!A", &[("A", 3)]), 0);
        assert_eq!(eval("S == 2", &[("S", 2)]), 1);
        assert_eq!(eval("S != 2 && S < 3", &[("S", 1)]), 1);
        assert_eq!(eval("Sel ? B : A", &[("Sel", 1), ("A", 5), ("B", 9)]), 9);
        assert_eq!(eval("Sel ? B : A", &[("Sel", 0), ("A", 5), ("B", 9)]), 5);
        assert_eq!(eval("0b1_0000 - 1", &[]), 15);
        assert_eq!(eval("-1", &[]), u64::MAX);
        assert_eq!(eval("1 << 70", &[]), 0);
        assert_eq!(eval("7 / 0", &[]), 0);
        assert_eq!(eval("A >= 2 || A <= 0", &[("A", 1)]), 0);
    }

    #[test]
    fn names_and_errors() {
        let e = Expr::parse("Sel ? B : A + 1").unwrap();
        assert_eq!(e.names(), ["Sel", "B", "A"]);
        for bad in ["", "A +", "(A", "A B", "A ? B", "3x", "A $ B", ")"] {
            assert!(Expr::parse(bad).is_err(), "{bad}");
        }
    }
}
