//! The bounded expression language (`ache.pattern/v1` §1.2): a closed
//! grammar ache parses and evaluates itself. There is no eval, no FFI,
//! and no string execution anywhere: an expression is parsed once at
//! load time into a bounded tree, and evaluation is a total tree walk.
//!
//! Closed set:
//! - literals (decimal numbers) and names from a caller-supplied table
//!   (resolved to slots at parse time; an unknown name is a load error);
//! - `+ - * / %`, unary `-`; division and modulo by zero yield 0;
//! - `< <= == != >= >` yielding 1.0 / 0.0; `and or not` over numeric
//!   truth (0 is false);
//! - functions `min max clamp abs floor ceil round sin cos sqrt pow
//!   select`, plus `hash01` only when the caller enables a seed.
//!
//! Totality: every operation maps finite inputs to a finite output; a
//! non-finite intermediate (an overflowing `pow`, say) is defined as 0.
//! Bounds: source ≤ 4096 bytes, AST depth ≤ 32, nodes ≤ 256 — checked at
//! parse time, so the worst-case cost of one evaluation is known.

use crate::error::{AcheError, ErrorCode, Result};

pub const EXPR_SOURCE_BYTES_MAX: usize = 4096;
pub const EXPR_DEPTH_MAX: usize = 32;
pub const EXPR_NODES_MAX: usize = 256;
/// `pow` exponents are clamped into this range (§1.2).
pub const POW_EXPONENT_LIMIT: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Func {
    Min,
    Max,
    Clamp,
    Abs,
    Floor,
    Ceil,
    Round,
    Sin,
    Cos,
    Sqrt,
    Pow,
    Select,
    Hash01,
}

impl Func {
    fn from_name(name: &str) -> Option<(Func, usize)> {
        Some(match name {
            "min" => (Func::Min, 2),
            "max" => (Func::Max, 2),
            "clamp" => (Func::Clamp, 3),
            "abs" => (Func::Abs, 1),
            "floor" => (Func::Floor, 1),
            "ceil" => (Func::Ceil, 1),
            "round" => (Func::Round, 1),
            "sin" => (Func::Sin, 1),
            "cos" => (Func::Cos, 1),
            "sqrt" => (Func::Sqrt, 1),
            "pow" => (Func::Pow, 2),
            "select" => (Func::Select, 3),
            "hash01" => (Func::Hash01, 1),
            _ => return None,
        })
    }
}

/// Names reserved by the grammar; a variable may not take one.
pub const RESERVED_NAMES: &[&str] = &[
    "and", "or", "not", "min", "max", "clamp", "abs", "floor", "ceil", "round", "sin", "cos",
    "sqrt", "pow", "select", "hash01",
];

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Num(f64),
    Name(usize),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

/// A parsed, bounds-checked expression. Immutable after parse.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    root: Node,
}

/// What an evaluation may read: the slot values (same order as the
/// name table given at parse) and, when the spec declared one, the seed
/// for `hash01`.
pub struct Env<'a> {
    pub slots: &'a [f64],
    pub seed: Option<u64>,
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(AcheError::new(ErrorCode::RequestInvalid, message))
}

/// Zero for anything non-finite: evaluation is total.
fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn truth(value: f64) -> bool {
    value != 0.0
}

fn flag(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

/// splitmix64: a fixed, well-known 64-bit mixer. Deterministic on every
/// platform (integer arithmetic only).
pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `hash01(n)`: a deterministic PRF of (seed, floor(n)) in `[0, 1)`.
pub fn hash01(seed: u64, n: f64) -> f64 {
    let counter = finite(n).floor().clamp(-9.0e15, 9.0e15) as i64;
    let mixed = splitmix64(seed ^ splitmix64(counter as u64));
    // 53 high bits → an exactly representable f64 in [0, 1).
    (mixed >> 11) as f64 / (1u64 << 53) as f64
}

impl Expr {
    /// Parse `source` against a name table. `seeded` enables `hash01`.
    pub fn parse(source: &str, names: &[&str], seeded: bool) -> Result<Expr> {
        if source.len() > EXPR_SOURCE_BYTES_MAX {
            return invalid(format!("expression exceeds {EXPR_SOURCE_BYTES_MAX} bytes"));
        }
        let tokens = tokenize(source)?;
        let mut parser = Parser {
            tokens,
            at: 0,
            names,
            seeded,
            nodes: 0,
        };
        let root = parser.expr(1)?;
        if parser.at != parser.tokens.len() {
            return invalid(format!("expression {source:?}: unexpected trailing input"));
        }
        // The parser's running checks cut nesting and size early; the
        // exact bounds are measured on the finished tree (a left-
        // associated chain grows depth without nesting).
        let (depth, nodes) = measure(&root);
        if depth > EXPR_DEPTH_MAX {
            return invalid(format!("expression exceeds depth {EXPR_DEPTH_MAX}"));
        }
        if nodes > EXPR_NODES_MAX {
            return invalid(format!("expression exceeds {EXPR_NODES_MAX} nodes"));
        }
        Ok(Expr { root })
    }

    /// Evaluate. Total: always a finite number.
    #[must_use]
    pub fn eval(&self, env: &Env<'_>) -> f64 {
        eval_node(&self.root, env)
    }
}

/// (depth, node count) of a tree. Recursion is bounded by the parser's
/// running depth cut.
fn measure(node: &Node) -> (usize, usize) {
    match node {
        Node::Num(_) | Node::Name(_) => (1, 1),
        Node::Neg(inner) | Node::Not(inner) => {
            let (d, n) = measure(inner);
            (d + 1, n + 1)
        }
        Node::Bin(_, left, right) => {
            let (dl, nl) = measure(left);
            let (dr, nr) = measure(right);
            (dl.max(dr) + 1, nl + nr + 1)
        }
        Node::Call(_, args) => args
            .iter()
            .map(measure)
            .fold((1, 1), |(d, n), (ad, an)| (d.max(ad + 1), n + an)),
    }
}

fn eval_node(node: &Node, env: &Env<'_>) -> f64 {
    match node {
        Node::Num(value) => *value,
        Node::Name(slot) => env.slots.get(*slot).copied().map_or(0.0, finite),
        Node::Neg(inner) => finite(-eval_node(inner, env)),
        Node::Not(inner) => flag(!truth(eval_node(inner, env))),
        Node::Bin(op, left, right) => {
            let a = eval_node(left, env);
            let b = eval_node(right, env);
            finite(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => {
                    if b == 0.0 {
                        0.0
                    } else {
                        a / b
                    }
                }
                Op::Rem => {
                    if b == 0.0 {
                        0.0
                    } else {
                        a % b
                    }
                }
                Op::Lt => flag(a < b),
                Op::Le => flag(a <= b),
                Op::Eq => flag(a == b),
                Op::Ne => flag(a != b),
                Op::Ge => flag(a >= b),
                Op::Gt => flag(a > b),
                Op::And => flag(truth(a) && truth(b)),
                Op::Or => flag(truth(a) || truth(b)),
            })
        }
        Node::Call(func, args) => {
            let arg = |i: usize| eval_node(&args[i], env);
            finite(match func {
                Func::Min => arg(0).min(arg(1)),
                Func::Max => arg(0).max(arg(1)),
                Func::Clamp => {
                    let (x, lo, hi) = (arg(0), arg(1), arg(2));
                    // A reversed range is defined, not an error: the
                    // bounds are taken in order.
                    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                    x.clamp(lo, hi)
                }
                Func::Abs => arg(0).abs(),
                Func::Floor => arg(0).floor(),
                Func::Ceil => arg(0).ceil(),
                Func::Round => arg(0).round(),
                Func::Sin => arg(0).sin(),
                Func::Cos => arg(0).cos(),
                Func::Sqrt => arg(0).abs().sqrt(),
                Func::Pow => arg(0).powf(arg(1).clamp(-POW_EXPONENT_LIMIT, POW_EXPONENT_LIMIT)),
                Func::Select => {
                    if truth(arg(0)) {
                        arg(1)
                    } else {
                        arg(2)
                    }
                }
                Func::Hash01 => match env.seed {
                    Some(seed) => hash01(seed, arg(0)),
                    // Unreachable for a parsed expression (parse refuses
                    // hash01 without a seed); still total.
                    None => 0.0,
                },
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Sym(&'static str),
}

fn tokenize(source: &str) -> Result<Vec<Token>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == b'.' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let text = &source[start..i];
            let value: f64 = text.parse().map_err(|_| {
                AcheError::new(ErrorCode::RequestInvalid, format!("bad number {text:?}"))
            })?;
            if !value.is_finite() {
                return invalid(format!("number {text:?} is not finite"));
            }
            tokens.push(Token::Num(value));
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(Token::Ident(source[start..i].to_string()));
            continue;
        }
        let two = if i + 1 < bytes.len() {
            &source[i..i + 2]
        } else {
            ""
        };
        let sym: &'static str = match two {
            "<=" => "<=",
            ">=" => ">=",
            "==" => "==",
            "!=" => "!=",
            _ => match c {
                b'+' => "+",
                b'-' => "-",
                b'*' => "*",
                b'/' => "/",
                b'%' => "%",
                b'<' => "<",
                b'>' => ">",
                b'(' => "(",
                b')' => ")",
                b',' => ",",
                _ => {
                    return invalid(format!(
                        "unexpected character {:?} in expression",
                        char::from(c)
                    ));
                }
            },
        };
        i += sym.len();
        tokens.push(Token::Sym(sym));
    }
    if tokens.is_empty() {
        return invalid("expression is empty");
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    names: &'a [&'a str],
    seeded: bool,
    nodes: usize,
}

/// Binary operator precedence, loosest first.
fn binary(token: &Token) -> Option<(Op, u8)> {
    match token {
        Token::Ident(name) if name == "or" => Some((Op::Or, 1)),
        Token::Ident(name) if name == "and" => Some((Op::And, 2)),
        Token::Sym("==") => Some((Op::Eq, 3)),
        Token::Sym("!=") => Some((Op::Ne, 3)),
        Token::Sym("<") => Some((Op::Lt, 4)),
        Token::Sym("<=") => Some((Op::Le, 4)),
        Token::Sym(">") => Some((Op::Gt, 4)),
        Token::Sym(">=") => Some((Op::Ge, 4)),
        Token::Sym("+") => Some((Op::Add, 5)),
        Token::Sym("-") => Some((Op::Sub, 5)),
        Token::Sym("*") => Some((Op::Mul, 6)),
        Token::Sym("/") => Some((Op::Div, 6)),
        Token::Sym("%") => Some((Op::Rem, 6)),
        _ => None,
    }
}

impl Parser<'_> {
    fn node(&mut self, depth: usize) -> Result<()> {
        self.nodes += 1;
        if self.nodes > EXPR_NODES_MAX {
            return invalid(format!("expression exceeds {EXPR_NODES_MAX} nodes"));
        }
        if depth > EXPR_DEPTH_MAX {
            return invalid(format!("expression exceeds depth {EXPR_DEPTH_MAX}"));
        }
        Ok(())
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn expect(&mut self, sym: &str) -> Result<()> {
        match self.peek() {
            Some(Token::Sym(s)) if *s == sym => {
                self.at += 1;
                Ok(())
            }
            _ => invalid(format!("expected {sym:?} in expression")),
        }
    }

    /// Precedence climbing. `depth` is the AST depth of the node built.
    fn expr(&mut self, depth: usize) -> Result<Node> {
        self.binary_expr(depth, 1)
    }

    fn binary_expr(&mut self, depth: usize, min_prec: u8) -> Result<Node> {
        let mut left = self.unary(depth + 1)?;
        while let Some((op, prec)) = self.peek().and_then(binary) {
            if prec < min_prec {
                break;
            }
            self.at += 1;
            self.node(depth)?;
            let right = self.binary_expr(depth + 1, prec + 1)?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self, depth: usize) -> Result<Node> {
        self.node(depth)?;
        match self.peek().cloned() {
            Some(Token::Sym("-")) => {
                self.at += 1;
                Ok(Node::Neg(Box::new(self.unary(depth + 1)?)))
            }
            Some(Token::Ident(name)) if name == "not" => {
                self.at += 1;
                Ok(Node::Not(Box::new(self.unary(depth + 1)?)))
            }
            Some(Token::Num(value)) => {
                self.at += 1;
                Ok(Node::Num(value))
            }
            Some(Token::Sym("(")) => {
                self.at += 1;
                let inner = self.expr(depth + 1)?;
                self.expect(")")?;
                Ok(inner)
            }
            Some(Token::Ident(name)) => {
                self.at += 1;
                if let Some((func, arity)) = Func::from_name(&name) {
                    if func == Func::Hash01 && !self.seeded {
                        return invalid(
                            "hash01 needs a session seed; with no seed there is no randomness",
                        );
                    }
                    self.expect("(")?;
                    let mut args = Vec::with_capacity(arity);
                    for index in 0..arity {
                        if index > 0 {
                            self.expect(",")?;
                        }
                        args.push(self.expr(depth + 1)?);
                    }
                    self.expect(")")?;
                    return Ok(Node::Call(func, args));
                }
                match self.names.iter().position(|known| *known == name) {
                    Some(slot) => Ok(Node::Name(slot)),
                    None => invalid(format!("unknown name {name:?} in expression")),
                }
            }
            _ => invalid("expected a value in expression"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: &[&str] = &["time", "intensity", "index", "loop"];

    fn eval(source: &str, slots: &[f64]) -> f64 {
        Expr::parse(source, NAMES, false)
            .unwrap()
            .eval(&Env { slots, seed: None })
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn arithmetic_precedence_and_comparisons() {
        assert_eq!(eval("1 + 2 * 3", &[]), 7.0);
        assert_eq!(eval("(1 + 2) * 3", &[]), 9.0);
        assert_eq!(eval("10 - 4 - 3", &[]), 3.0);
        assert_eq!(eval("7 % 4", &[]), 3.0);
        assert_eq!(eval("-3 + 5", &[]), 2.0);
        assert_eq!(eval("2 < 3 and 3 >= 3", &[]), 1.0);
        assert_eq!(eval("not (1 == 1) or 0", &[]), 0.0);
        assert_eq!(eval("1 != 2", &[]), 1.0);
    }

    #[test]
    fn every_function_evaluates_its_law() {
        assert_eq!(eval("min(3, 4) + max(3, 4)", &[]), 7.0);
        assert_eq!(eval("clamp(150, 0, 100)", &[]), 100.0);
        assert_eq!(eval("clamp(5, 10, 0)", &[]), 5.0);
        assert_eq!(
            eval("abs(-2) + floor(1.7) + ceil(1.2) + round(2.5)", &[]),
            8.0
        );
        assert_eq!(eval("sin(0) + cos(0)", &[]), 1.0);
        assert_eq!(eval("sqrt(-16)", &[]), 4.0);
        assert_eq!(eval("pow(2, 10)", &[]), 16.0, "exponent clamps to 4");
        assert_eq!(eval("select(0, 1, 2) + select(5, 10, 20)", &[]), 12.0);
    }

    #[test]
    fn names_resolve_to_slots() {
        assert_eq!(
            eval(
                "time * 10 + intensity + index + loop",
                &[1.5, 20.0, 2.0, 3.0]
            ),
            40.0
        );
    }

    #[test]
    fn hash01_is_deterministic_and_seed_dependent() {
        let expr = Expr::parse("hash01(time)", NAMES, true).unwrap();
        let a = expr.eval(&Env {
            slots: &[3.0],
            seed: Some(42),
        });
        let b = expr.eval(&Env {
            slots: &[3.0],
            seed: Some(42),
        });
        let c = expr.eval(&Env {
            slots: &[3.0],
            seed: Some(43),
        });
        assert_eq!(a.to_bits(), b.to_bits());
        assert_ne!(a.to_bits(), c.to_bits());
        assert!((0.0..1.0).contains(&a));
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn division_and_modulo_by_zero_are_zero_not_errors() {
        assert_eq!(eval("5 / 0", &[]), 0.0);
        assert_eq!(eval("5 % 0", &[]), 0.0);
        assert_eq!(eval("intensity / (time - time)", &[1.0, 9.0]), 0.0);
    }

    #[test]
    fn overflow_is_defined_as_zero() {
        // 1e300 * 1e300 overflows to inf: defined as 0, evaluation total.
        let big = format!("1{}", "0".repeat(80));
        assert_eq!(eval(&format!("pow({big}, 4)"), &[]), 0.0);
        assert_eq!(eval(&format!("pow({big}, 4) + 1"), &[]), 1.0);
    }

    #[test]
    fn unknown_names_strings_and_code_shapes_are_refused() {
        for bad in [
            "x + 1",
            "Math.random()",
            "\"text\"",
            "eval(1)",
            "time.x",
            "intensity = 5",
            "f(1)",
            "random()",
            "",
            "1 +",
            "(1",
            "min(1)",
            "min(1, 2, 3)",
            "1 2",
            "1..2",
        ] {
            assert!(
                Expr::parse(bad, NAMES, false).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn hash01_without_a_seed_is_refused() {
        let error = Expr::parse("hash01(1)", NAMES, false).unwrap_err();
        assert!(error.message.contains("seed"));
    }

    #[test]
    fn depth_and_node_bounds_are_load_errors() {
        let deep = format!("{}1{}", "(".repeat(40), ")".repeat(40));
        assert!(Expr::parse(&deep, NAMES, false).is_err());
        let nested = format!("{}1{}", "-".repeat(40), "");
        assert!(Expr::parse(&nested, NAMES, false).is_err());
        let wide = vec!["1"; 300].join(" + ");
        assert!(Expr::parse(&wide, NAMES, false).is_err());
        // A left-associated chain is as deep as it is long: 40 terms is
        // over the depth bound even though it parses without nesting.
        let chain = vec!["1"; 40].join(" + ");
        assert!(Expr::parse(&chain, NAMES, false).is_err());
        let ok = vec!["1"; 20].join(" + ");
        assert_eq!(eval(&ok, &[]), 20.0);
        let long = "1".repeat(EXPR_SOURCE_BYTES_MAX + 1);
        assert!(Expr::parse(&long, NAMES, false).is_err());
    }
}
