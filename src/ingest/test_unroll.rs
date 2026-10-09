//! Tests a test class body declares other than one literal `test "…"` or
//! `def test_…` statement, and how each is either modeled or named.
//!
//! A statement that declares tests ingest cannot model used to refuse its
//! whole class, so one `[a, b].each do |x| test "#{x}" … end` took every
//! literal test beside it out of the run. Now only that statement is
//! refused, by kind and line (`declare_hidden`): the survey records it
//! and the class's other tests still ingest. Strict ingest still fails.
//!
//! What is modeled:
//!
//! - a test name known before the code runs (`static_string`): adjacent
//!   literals (`test "a " \ "b" do`, an `InterpolatedStringNode` of
//!   string parts) and interpolations of literals;
//! - a class-body loop over a collection known before the code runs (a
//!   literal Array, Hash, integer Range or `%w`/`%i` list, `n.times`, or
//!   a same-class `CONST = <literal>` / `<literal>.freeze` written before
//!   the loop), iterated by `each`, `each_with_index`, `each_pair` or
//!   `times`, whose block does nothing but declare tests (or nest such a
//!   loop). Each iteration's tests are unrolled under the name Ruby gives
//!   them (`to_s`/`inspect` of the element, as `"#{…}"` would), with the
//!   block parameters bound by Ruby's block rules (auto-splat of an Array
//!   element, `nil` padding) and set at the start of each test's body
//!   (`x = <element>`), so the body sees the value its closure did.
//!
//! The element is re-evaluated in each test from its literal instead of
//! being the one object the loop yielded; a test that mutates a shared
//! element, or compares its identity, is the one difference.

use std::rc::Rc;

use ruby_prism::Node;

use crate::dialect::Test;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::VarId;
use crate::span::Span;
use crate::{ClassId, Symbol};

use super::super::expr::ingest_expr;
use super::super::util::{constant_id_str, flatten_statements};
use super::super::{IngestError, IngestResult};
use super::{dsl_test, TagConstants, TestTags};

/// Most tests one loop statement may unroll to (a `(1..n)` range).
const MAX_ITERATIONS: usize = 1000;

/// A Ruby value known before the code runs.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Nil,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Sym(String),
    Array(Vec<Value>),
    /// Entries in insertion order; a repeated key keeps its first place.
    Hash(Vec<(Value, Value)>),
    /// A class or module, by the full name it was declared under.
    Namespace(String),
    /// A value that exists when the code runs but is not known here: an
    /// element that is only bound (re-evaluated in each test), or a part
    /// of another file's constant nothing reads. Anything that needs it
    /// (a name, a comparison, a hash key) is not known either.
    Opaque,
}

impl Value {
    fn truthy(&self) -> Option<bool> {
        match self {
            Value::Opaque => None,
            other => Some(!matches!(other, Value::Nil | Value::Bool(false))),
        }
    }

    fn has_opaque(&self) -> bool {
        match self {
            Value::Opaque => true,
            Value::Array(items) => items.iter().any(Value::has_opaque),
            Value::Hash(entries) => entries.iter().any(|(k, v)| k.has_opaque() || v.has_opaque()),
            _ => false,
        }
    }

    /// A value a literal spells (`nil`, a number, String, Symbol, or an
    /// Array or Hash of them): one a test body can rebuild by itself.
    fn is_literal(&self) -> bool {
        match self {
            Value::Opaque | Value::Namespace(_) => false,
            Value::Array(items) => items.iter().all(Value::is_literal),
            Value::Hash(entries) => entries.iter().all(|(k, v)| k.is_literal() && v.is_literal()),
            _ => true,
        }
    }
}

/// A statement ingest does not model, as the kind the gap names and
/// what in it is the reason.
struct NotModeled {
    kind: String,
    detail: String,
}

fn not_modeled(kind: impl Into<String>, detail: impl Into<String>) -> NotModeled {
    NotModeled { kind: kind.into(), detail: detail.into() }
}

/// The first line of `node`'s source, for a gap's detail.
fn first_line(node: &Node<'_>) -> String {
    let text = String::from_utf8_lossy(node.location().as_slice()).into_owned();
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    if line.chars().count() > 100 {
        format!("{}…", line.chars().take(100).collect::<String>())
    } else {
        line
    }
}

/// A test name whose text is known before the code runs: a string
/// literal, adjacent literals (`"a " \ "b"`), or an interpolation of
/// literals (`"x #{:y}"`). `None` for anything computed.
pub(super) fn static_string(node: &Node<'_>) -> Option<String> {
    if node.as_string_node().is_none() && node.as_interpolated_string_node().is_none() {
        return None;
    }
    match eval(node, &Env::default())? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

/// Block-parameter values in scope while a loop's tests are named, and
/// where constants the expression reads are looked up.
#[derive(Default, Clone)]
struct Env {
    vars: Vec<(String, Value)>,
    /// `None`: a constant is not known.
    cx: Option<Rc<consts::ConstCx>>,
    mode: Mode,
}

/// What an Array element or Hash value that is not known becomes.
#[derive(Default, Clone, Copy, PartialEq)]
enum Mode {
    /// Nothing: the whole value is not known (a test name).
    #[default]
    Exact,
    /// `Value::Opaque` when re-evaluating it in a test body gives the
    /// same value (`consts::rebuildable`): a loop element of the test
    /// file, which each test binds from its own expression.
    Rebuild,
    /// `Value::Opaque` whatever it is: another file's constant, which
    /// Ruby evaluated once at load and no test re-evaluates.
    Loaded,
}

impl Env {
    fn get(&self, name: &str) -> Option<&Value> {
        self.vars.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

fn utf8(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

/// The value of `node` as Ruby computes it, for the expressions a test
/// name interpolates: literals, block parameters, `inspect`, `to_s`,
/// `[]`, `fetch`, `join`, `first`, `last`, `size`, `freeze` and `?:`.
/// `None` for anything else.
fn eval(node: &Node<'_>, env: &Env) -> Option<Value> {
    if let Some(s) = node.as_string_node() {
        return utf8(s.unescaped()).map(Value::Str);
    }
    if let Some(s) = node.as_interpolated_string_node() {
        let mut out = String::new();
        for part in s.parts().iter() {
            out.push_str(&interpolated_part(&part, env)?);
        }
        return Some(Value::Str(out));
    }
    if let Some(s) = node.as_symbol_node() {
        return utf8(s.unescaped()).map(Value::Sym);
    }
    if let Some(i) = node.as_integer_node() {
        return super::super::util::integer_decimal(&i.value()).parse().ok().map(Value::Int);
    }
    if let Some(f) = node.as_float_node() {
        return Some(Value::Float(f.value()));
    }
    if node.as_nil_node().is_some() {
        return Some(Value::Nil);
    }
    if node.as_true_node().is_some() {
        return Some(Value::Bool(true));
    }
    if node.as_false_node().is_some() {
        return Some(Value::Bool(false));
    }
    if let Some(a) = node.as_array_node() {
        let values: Option<Vec<Value>> = a
            .elements()
            .iter()
            .map(|e| if e.as_splat_node().is_some() { None } else { elem_value(&e, env) })
            .collect();
        return values.map(Value::Array);
    }
    if let Some(h) = node.as_hash_node() {
        let mut entries: Vec<(Value, Value)> = Vec::new();
        for element in h.elements().iter() {
            let assoc = element.as_assoc_node()?;
            let key = eval(&assoc.key(), env).filter(|k| !k.has_opaque())?;
            let value = elem_value(&assoc.value(), env)?;
            hash_store(&mut entries, key, value);
        }
        return Some(Value::Hash(entries));
    }
    if let Some(p) = node.as_parentheses_node() {
        let body = p.body()?;
        let stmts = flatten_statements(body);
        let [only] = stmts.as_slice() else { return None };
        return eval(only, env);
    }
    if let Some(read) = node.as_local_variable_read_node() {
        return env.get(constant_id_str(&read.name())).cloned();
    }
    if node.as_constant_read_node().is_some() || node.as_constant_path_node().is_some() {
        return consts::value(node, env);
    }
    if let Some(cond) = node.as_if_node() {
        let branch = if eval(&cond.predicate(), env)?.truthy()? {
            cond.statements().map(|s| s.as_node())
        } else {
            match cond.subsequent() {
                None => None,
                Some(next) => match next.as_else_node() {
                    Some(else_node) => else_node.statements().map(|s| s.as_node()),
                    // `elsif`: another IfNode.
                    None => return eval(&next, env),
                },
            }
        };
        return match branch {
            None => Some(Value::Nil),
            Some(stmts) => {
                let stmts = flatten_statements(stmts);
                let [only] = stmts.as_slice() else { return None };
                eval(only, env)
            }
        };
    }
    if let Some(call) = node.as_call_node() {
        let receiver = call.receiver()?;
        // `map(&:to_s)`: a Symbol block argument; any other block is
        // code this does not run.
        let block_sym = match call.block() {
            None => None,
            Some(block) => {
                let arg = block.as_block_argument_node()?;
                let sym = arg.expression()?.as_symbol_node()?;
                Some(utf8(sym.unescaped())?)
            }
        };
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if args.iter().any(|a| a.as_splat_node().is_some() || a.as_block_argument_node().is_some()) {
            return None;
        }
        let recv = eval(&receiver, env)?;
        let method = constant_id_str(&call.name());
        if let Some(sym) = block_sym {
            return match (method, args.as_slice(), recv) {
                ("map" | "collect", [], Value::Array(items)) => items
                    .iter()
                    .map(|item| send0(item, &sym))
                    .collect::<Option<Vec<_>>>()
                    .map(Value::Array),
                _ => None,
            };
        }
        if let Value::Namespace(module) = &recv {
            return match (method, args.as_slice()) {
                ("constants", []) => consts::module_constants(module).map(Value::Array),
                _ => None,
            };
        }
        let arg = |at: usize| eval(&args[at], env);
        return match (method, args.as_slice()) {
            ("[]", [_]) => index(&recv, &arg(0)?, false),
            ("fetch", [_]) => index(&recv, &arg(0)?, true),
            ("join", []) => join(&recv, "").map(Value::Str),
            ("join", [_]) => match arg(0)? {
                Value::Str(sep) => join(&recv, &sep).map(Value::Str),
                Value::Nil => join(&recv, "").map(Value::Str),
                _ => None,
            },
            ("==", [_]) => equal(&recv, &arg(0)?).map(Value::Bool),
            ("!=", [_]) => equal(&recv, &arg(0)?).map(|eq| Value::Bool(!eq)),
            ("+", [_]) => match (recv, arg(0)?) {
                (Value::Array(mut a), Value::Array(b)) => {
                    a.extend(b);
                    Some(Value::Array(a))
                }
                (Value::Str(a), Value::Str(b)) => Some(Value::Str(a + &b)),
                (Value::Int(a), Value::Int(b)) => a.checked_add(b).map(Value::Int),
                _ => None,
            },
            ("-", [_]) => match (recv, arg(0)?) {
                (Value::Array(a), Value::Array(b)) => difference(a, &b).map(Value::Array),
                (Value::Int(a), Value::Int(b)) => a.checked_sub(b).map(Value::Int),
                _ => None,
            },
            // ActiveSupport's `Array#excluding(*elements)`: `self - elements`.
            ("excluding" | "without", _) => {
                let Value::Array(a) = recv else { return None };
                let b: Option<Vec<Value>> = (0..args.len()).map(arg).collect();
                difference(a, &b?).map(Value::Array)
            }
            ("to_sentence", []) => to_sentence(&recv).map(Value::Str),
            (name, []) => send0(&recv, name),
            _ => None,
        };
    }
    None
}

/// An Array element or Hash value: its value, or, where `env.mode`
/// allows, `Value::Opaque` when it is not known.
fn elem_value(node: &Node<'_>, env: &Env) -> Option<Value> {
    if let Some(value) = eval(node, env) {
        return Some(value);
    }
    match env.mode {
        Mode::Exact => None,
        Mode::Rebuild => consts::rebuildable(node, env).then_some(Value::Opaque),
        Mode::Loaded => Some(Value::Opaque),
    }
}

/// `recv.<name>` with no arguments, for the methods modeled.
fn send0(recv: &Value, name: &str) -> Option<Value> {
    match (name, recv) {
        ("inspect", v) => inspect(v).map(Value::Str),
        ("to_s", v) => to_s(v).map(Value::Str),
        ("to_sym", Value::Str(s) | Value::Sym(s)) => Some(Value::Sym(s.clone())),
        ("freeze" | "dup", v) if !matches!(v, Value::Opaque) => Some(v.clone()),
        ("first", Value::Array(items)) => Some(items.first().cloned().unwrap_or(Value::Nil)),
        ("last", Value::Array(items)) => Some(items.last().cloned().unwrap_or(Value::Nil)),
        ("size" | "length" | "count", Value::Array(items)) => Some(Value::Int(items.len() as i128)),
        ("size" | "length" | "count", Value::Hash(entries)) => Some(Value::Int(entries.len() as i128)),
        ("size" | "length", Value::Str(s)) => Some(Value::Int(s.chars().count() as i128)),
        ("keys", Value::Hash(entries)) => Some(Value::Array(entries.iter().map(|(k, _)| k.clone()).collect())),
        ("values", Value::Hash(entries)) => Some(Value::Array(entries.iter().map(|(_, v)| v.clone()).collect())),
        ("to_a", Value::Array(items)) => Some(Value::Array(items.clone())),
        ("to_a", Value::Hash(entries)) => {
            Some(Value::Array(entries.iter().map(|(k, v)| Value::Array(vec![k.clone(), v.clone()])).collect()))
        }
        _ => None,
    }
}

/// `a == b`, where it is known: two values a literal spells, or two
/// modules.
fn equal(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Opaque, _) | (_, Value::Opaque) => None,
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => Some((*x as f64) == *y),
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(false);
            }
            for (x, y) in x.iter().zip(y) {
                if !equal(x, y)? {
                    return Some(false);
                }
            }
            Some(true)
        }
        (Value::Hash(_), _) | (_, Value::Hash(_)) => (!a.has_opaque() && !b.has_opaque() && a == b).then_some(true),
        _ => Some(a == b),
    }
}

/// `Array#-`: the elements of `a` not `eql?` to any of `b`'s.
fn difference(a: Vec<Value>, b: &[Value]) -> Option<Vec<Value>> {
    if a.iter().chain(b).any(|v| v.has_opaque() || matches!(v, Value::Hash(_))) {
        return None;
    }
    Some(a.into_iter().filter(|x| !b.iter().any(|y| hash_key_eq(x, y))).collect())
}

/// ActiveSupport's `Array#to_sentence` in the default (English) locale.
fn to_sentence(recv: &Value) -> Option<String> {
    let Value::Array(items) = recv else { return None };
    Some(match items.as_slice() {
        [] => String::new(),
        [one] => to_s(one)?,
        [a, b] => format!("{} and {}", to_s(a)?, to_s(b)?),
        [init @ .., last] => format!("{}, and {}", join(&Value::Array(init.to_vec()), ", ")?, to_s(last)?),
    })
}

/// `Hash#[]=` on a literal: a repeated key keeps its place, takes the
/// new value.
fn hash_store(entries: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    match entries.iter_mut().find(|(k, _)| hash_key_eq(k, &key)) {
        Some(entry) => entry.1 = value,
        None => entries.push((key, value)),
    }
}

/// `eql?` for the keys a literal can spell: `1` and `1.0` differ, as do
/// `"a"` and `:a`.
fn hash_key_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits() || (x == y && *x != 0.0),
        _ => a == b,
    }
}

/// `recv[key]` (`fetch` when `fetch`: a missing key raises, so `None`).
fn index(recv: &Value, key: &Value, fetch: bool) -> Option<Value> {
    match (recv, key) {
        (Value::Array(items), Value::Int(i)) => {
            let len = items.len() as i128;
            let at = if *i < 0 { len + i } else { *i };
            match (0..len).contains(&at) {
                true => Some(items[at as usize].clone()),
                false if fetch => None,
                false => Some(Value::Nil),
            }
        }
        (Value::Hash(entries), key) => match entries.iter().find(|(k, _)| hash_key_eq(k, key)) {
            Some((_, v)) => Some(v.clone()),
            None if fetch => None,
            None => Some(Value::Nil),
        },
        _ => None,
    }
}

/// `Array#join(sep)`: each element's `to_s`, a nested Array joined
/// with the same separator.
fn join(recv: &Value, sep: &str) -> Option<String> {
    let Value::Array(items) = recv else { return None };
    let mut pieces = Vec::with_capacity(items.len());
    for item in items {
        pieces.push(match item {
            Value::Array(_) => join(item, sep)?,
            other => to_s(other)?,
        });
    }
    Some(pieces.join(sep))
}

/// One part of an interpolated string: its text, or `#{expr}`'s `to_s`.
fn interpolated_part(part: &Node<'_>, env: &Env) -> Option<String> {
    if let Some(s) = part.as_string_node() {
        return utf8(s.unescaped());
    }
    if part.as_interpolated_string_node().is_some() {
        return match eval(part, env)? {
            Value::Str(s) => Some(s),
            _ => None,
        };
    }
    if let Some(embedded) = part.as_embedded_statements_node() {
        let Some(stmts) = embedded.statements() else { return Some(String::new()) };
        let stmts: Vec<Node<'_>> = stmts.body().iter().collect();
        let [only] = stmts.as_slice() else { return None };
        return to_s(&eval(only, env)?);
    }
    None
}

/// `to_s`, as `"#{value}"` writes it.
fn to_s(value: &Value) -> Option<String> {
    match value {
        Value::Nil => Some(String::new()),
        Value::Str(s) | Value::Sym(s) => Some(s.clone()),
        Value::Array(_) | Value::Hash(_) => inspect(value),
        other => inspect(other),
    }
}

/// `inspect` (Ruby 3.4+'s Hash form, `{name: 1, "k" => 2}`). `None`
/// where the text depends on more than is modeled (non-ASCII symbols,
/// non-alphanumeric non-ASCII characters).
fn inspect(value: &Value) -> Option<String> {
    Some(match value {
        Value::Nil => "nil".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => float_to_s(*f),
        Value::Str(s) => str_inspect(s)?,
        Value::Sym(s) => sym_inspect(s)?,
        Value::Array(items) => {
            let parts: Option<Vec<String>> = items.iter().map(inspect).collect();
            format!("[{}]", parts?.join(", "))
        }
        Value::Hash(entries) => {
            if entries.is_empty() {
                return Some("{}".into());
            }
            let mut parts = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let value = inspect(value)?;
                parts.push(match key {
                    Value::Sym(name) if !name.is_ascii() => return None,
                    Value::Sym(name) if is_label(name) => format!("{name}: {value}"),
                    Value::Sym(name) => format!("{}: {value}", str_inspect(name)?),
                    other => format!("{} => {value}", inspect(other)?),
                });
            }
            format!("{{{}}}", parts.join(", "))
        }
        // A module's `to_s`/`inspect` is its name unless it defines its
        // own; that is not looked for.
        Value::Namespace(_) | Value::Opaque => return None,
    })
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A symbol a Hash's inspect writes as `name:`.
fn is_label(name: &str) -> bool {
    let stem = name.strip_suffix(['?', '!']).unwrap_or(name);
    is_ident(stem)
}

const OPERATOR_SYMBOLS: &[&str] = &[
    "[]", "[]=", "+", "-", "*", "/", "%", "**", "==", "===", "!=", "=~", "!~", "!", "<", ">", "<=", ">=", "<=>",
    "<<", ">>", "&", "|", "^", "~", "+@", "-@", "`",
];

fn sym_inspect(name: &str) -> Option<String> {
    if !name.is_ascii() {
        return None;
    }
    // `:$1`, `:$!`: special globals print bare; not modeled.
    if name.starts_with('$') && !name[1..].chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    let plain = {
        let stem = name.strip_suffix(['?', '!', '=']).unwrap_or(name);
        is_ident(stem)
            || name.strip_prefix("@@").is_some_and(is_ident)
            || name.strip_prefix('@').is_some_and(is_ident)
            || name.strip_prefix('$').is_some_and(is_ident)
            || OPERATOR_SYMBOLS.contains(&name)
    };
    if plain {
        Some(format!(":{name}"))
    } else {
        Some(format!(":{}", str_inspect(name)?))
    }
}

/// `String#inspect` of a UTF-8 string.
fn str_inspect(s: &str) -> Option<String> {
    let mut out = String::from("\"");
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '#' if matches!(chars.peek(), Some('{' | '$' | '@')) => out.push_str("\\#"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0b}' => out.push_str("\\v"),
            '\u{08}' => out.push_str("\\b"),
            '\u{07}' => out.push_str("\\a"),
            '\u{1b}' => out.push_str("\\e"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\u{:04X}", c as u32)),
            c if c.is_ascii() => out.push(c),
            c if c.is_alphanumeric() => out.push(c),
            _ => return None,
        }
    }
    out.push('"');
    Some(out)
}

/// `Float#to_s`: the shortest digits that round-trip, fixed notation
/// for a decimal exponent in -4 < e <= 16 (with a fraction at 16),
/// otherwise `d.ddde+XX`.
fn float_to_s(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // `{:e}` writes the shortest round-trip digits: `1.2345e-5`.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').expect("float exponent");
    let exp: i32 = exp.parse().expect("float exponent digits");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digs = digits.len() as i32;
    let decpt = exp + 1;
    let sign = if f < 0.0 { "-" } else { "" };
    let body = if decpt > 0 && (decpt < digs || decpt <= 15) {
        if decpt < digs {
            format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
        } else {
            format!("{}{}.0", digits, "0".repeat((decpt - digs) as usize))
        }
    } else if decpt <= 0 && decpt > -4 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else {
        let rest = if digs > 1 { &digits[1..] } else { "0" };
        let e = decpt - 1;
        format!("{}.{}e{}{:02}", &digits[..1], rest, if e < 0 { '-' } else { '+' }, e.abs())
    };
    format!("{sign}{body}")
}

/// One value a loop yields to its block, with the expression that
/// rebuilds it in a test body and, for an Array, its elements' own.
#[derive(Clone)]
struct Elem {
    value: Value,
    expr: Expr,
    parts: Option<Vec<Elem>>,
}

impl Elem {
    fn nil() -> Self {
        Elem { value: Value::Nil, expr: lit(Literal::Nil), parts: None }
    }

    fn int(i: i128) -> Self {
        let value = match i64::try_from(i) {
            Ok(v) => Literal::Int { value: v },
            Err(_) => Literal::BigInt { value: i.to_string() },
        };
        Elem { value: Value::Int(i), expr: lit(value), parts: None }
    }

    fn pair(key: Elem, value: Elem) -> Self {
        Elem {
            value: Value::Array(vec![key.value.clone(), value.value.clone()]),
            expr: Expr::new(
                Span::synthetic(),
                ExprNode::Array { elements: vec![key.expr.clone(), value.expr.clone()], style: Default::default() },
            ),
            parts: Some(vec![key, value]),
        }
    }

    /// Ruby's multiple assignment of this value to `n` targets: an
    /// Array's elements (padded with `nil`), anything else first.
    fn splat(&self, n: usize) -> Option<Vec<Elem>> {
        let mut out: Vec<Elem> = match (&self.value, &self.parts) {
            (Value::Array(_), Some(parts)) => parts.iter().take(n).cloned().collect(),
            (Value::Array(_), None) => return None,
            _ => vec![self.clone()],
        };
        out.truncate(n);
        while out.len() < n {
            out.push(Elem::nil());
        }
        Some(out)
    }
}

fn lit(value: Literal) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Lit { value })
}

/// `node` as a loop element: its value (`Opaque` parts where
/// re-evaluating them in a test gives the same, `consts::rebuildable`),
/// and its own expression.
fn elem_of(node: &Node<'_>, file: &str, env: &Env) -> IngestResult<Option<Elem>> {
    let element_env = Env { mode: Mode::Rebuild, ..env.clone() };
    let Some(value) = elem_value(node, &element_env) else { return Ok(None) };
    let parts = match node.as_array_node() {
        Some(array) => {
            let mut parts = Vec::new();
            for element in array.elements().iter() {
                let Some(part) = elem_of(&element, file, env)? else { return Ok(None) };
                parts.push(part);
            }
            Some(parts)
        }
        None => None,
    };
    Ok(Some(Elem { value, expr: ingest_expr(node, file)?, parts }))
}

/// The expression a literal `value` is spelled by.
fn value_expr(value: &Value) -> Option<Expr> {
    let node = match value {
        Value::Nil => ExprNode::Lit { value: Literal::Nil },
        Value::Bool(b) => ExprNode::Lit { value: Literal::Bool { value: *b } },
        Value::Int(_) => return Some(Elem::int(match value { Value::Int(i) => *i, _ => 0 }).expr),
        Value::Float(f) => ExprNode::Lit { value: Literal::Float { value: *f } },
        Value::Str(s) => ExprNode::Lit { value: Literal::Str { value: s.clone() } },
        Value::Sym(s) => ExprNode::Lit { value: Literal::Sym { value: Symbol::from(s.as_str()) } },
        Value::Array(items) => ExprNode::Array {
            elements: items.iter().map(value_expr).collect::<Option<Vec<_>>>()?,
            style: Default::default(),
        },
        Value::Hash(entries) => ExprNode::Hash {
            entries: entries.iter().map(|(k, v)| Some((value_expr(k)?, value_expr(v)?))).collect::<Option<Vec<_>>>()?,
            kwargs: false,
        },
        Value::Namespace(_) | Value::Opaque => return None,
    };
    Some(Expr::new(Span::synthetic(), node))
}

/// `recv.method(args)`.
fn send(recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: Some(recv), method: Symbol::from(method), args, block: None, parenthesized: true },
    )
}

/// An element of a collection computed from constants: rebuilt from
/// its literal value, or else read again from the collection (`recv[i]`,
/// which the test body re-evaluates to the same object).
fn computed_elem(value: Value, at_recv: &dyn Fn() -> Expr) -> Elem {
    let parts = match &value {
        Value::Array(items) => Some(
            items
                .iter()
                .enumerate()
                .map(|(j, item)| computed_elem(item.clone(), &|| send(at_recv(), "[]", vec![Elem::int(j as i128).expr])))
                .collect(),
        ),
        _ => None,
    };
    let expr = value_expr(&value).unwrap_or_else(at_recv);
    Elem { value, expr, parts }
}

/// A collection a class-body loop iterates, known before the code runs.
enum Collection {
    Array(Vec<Elem>),
    Hash(Vec<(Elem, Elem)>),
}

/// What `receiver` iterates: an Array, Hash or integer Range literal
/// (`.freeze`d or not), a constant the class body set to one before
/// `before`, or a value computed from constants (`consts`). `None` for
/// anything else.
fn collection(
    receiver: &Node<'_>,
    class_body: &[Node<'_>],
    before: usize,
    file: &str,
    env: &Env,
) -> IngestResult<Option<Collection>> {
    if let Some(call) = receiver.as_call_node() {
        let freeze = constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() && call.block().is_none();
        if let (Some(inner), true) = (call.receiver(), freeze) {
            if let Some(found) = collection(&inner, class_body, before, file, env)? {
                return Ok(Some(found));
            }
        }
        return computed(receiver, file, env);
    }
    if let Some(array) = receiver.as_array_node() {
        let mut elems = Vec::new();
        for element in array.elements().iter() {
            let Some(elem) = elem_of(&element, file, env)? else { return Ok(None) };
            elems.push(elem);
        }
        return Ok(Some(Collection::Array(elems)));
    }
    if let Some(hash) = receiver.as_hash_node() {
        let mut entries: Vec<(Elem, Elem)> = Vec::new();
        for element in hash.elements().iter() {
            let Some(assoc) = element.as_assoc_node() else { return Ok(None) };
            let (Some(key), Some(value)) = (elem_of(&assoc.key(), file, env)?, elem_of(&assoc.value(), file, env)?) else {
                return Ok(None);
            };
            if key.value.has_opaque() {
                return Ok(None);
            }
            match entries.iter_mut().find(|(k, _)| hash_key_eq(&k.value, &key.value)) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
        }
        return Ok(Some(Collection::Hash(entries)));
    }
    if let Some(parens) = receiver.as_parentheses_node() {
        let stmts = parens.body().map(flatten_statements).unwrap_or_default();
        let [only] = stmts.as_slice() else { return Ok(None) };
        if let Some(range) = only.as_range_node() {
            let bound = |n: Option<Node<'_>>| match n.and_then(|n| eval(&n, &Env::default())) {
                Some(Value::Int(i)) => Some(i),
                _ => None,
            };
            let (Some(lo), Some(hi)) = (bound(range.left()), bound(range.right())) else { return Ok(None) };
            let hi = if range.is_exclude_end() { hi - 1 } else { hi };
            if hi - lo >= MAX_ITERATIONS as i128 {
                return Ok(None);
            }
            return Ok(Some(Collection::Array((lo..=hi).map(Elem::int).collect())));
        }
        if let Some(found) = collection(only, class_body, before, file, env)? {
            return Ok(Some(found));
        }
        return computed(receiver, file, env);
    }
    if let Some(read) = receiver.as_constant_read_node() {
        let name = read.name();
        let written = class_body
            .iter()
            .filter(|stmt| stmt.location().start_offset() < before)
            .filter_map(|stmt| stmt.as_constant_write_node())
            .filter(|write| write.name().as_slice() == name.as_slice())
            .last();
        if let Some(write) = written {
            let at = write.location().start_offset();
            let env = Env { cx: env.cx.as_ref().map(|cx| Rc::new(cx.before(at))), ..env.clone() };
            if let Some(found) = collection(&write.value(), class_body, at, file, &env)? {
                return Ok(Some(found));
            }
        }
    }
    computed(receiver, file, env)
}

/// The collection `receiver` computes from constants, each element bound
/// by `computed_elem`.
fn computed(receiver: &Node<'_>, file: &str, env: &Env) -> IngestResult<Option<Collection>> {
    let exact = Env { mode: Mode::Exact, ..env.clone() };
    let Some(value) = eval(receiver, &exact) else { return Ok(None) };
    let recv = ingest_expr(receiver, file)?;
    Ok(match value {
        Value::Array(items) => Some(Collection::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| computed_elem(item, &|| send(recv.clone(), "[]", vec![Elem::int(i as i128).expr])))
                .collect(),
        )),
        Value::Hash(entries) => Some(Collection::Hash(
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (k, v))| {
                    let at = || vec![Elem::int(i as i128).expr];
                    (
                        computed_elem(k, &|| send(send(recv.clone(), "keys", vec![]), "[]", at())),
                        computed_elem(v, &|| send(send(recv.clone(), "values", vec![]), "[]", at())),
                    )
                })
                .collect(),
        )),
        _ => None,
    })
}

/// The receiver of a loop `CONST = <receiver>.each do … end` assigns:
/// `each` and its kin return it.
pub(super) fn loop_receiver<'pr>(node: &Node<'pr>) -> Option<Node<'pr>> {
    let call = node.as_call_node()?;
    call.block()?.as_block_node()?;
    matches!(constant_id_str(&call.name()), "each" | "each_with_index" | "each_pair")
        .then(|| call.receiver())
        .flatten()
}

/// A block parameter: a name, or a `(a, b)` destructuring.
enum Param {
    Name(String),
    Multi(Vec<Param>),
}

fn param_of(node: &Node<'_>) -> Option<Param> {
    if let Some(required) = node.as_required_parameter_node() {
        return Some(Param::Name(constant_id_str(&required.name()).to_string()));
    }
    let multi = node.as_multi_target_node()?;
    if multi.rest().is_some() || multi.rights().iter().next().is_some() {
        return None;
    }
    multi.lefts().iter().map(|n| param_of(&n)).collect::<Option<Vec<_>>>().map(Param::Multi)
}

/// The block's parameters, when each is a plain name or `(a, b)`.
fn block_params(block: &ruby_prism::BlockNode<'_>) -> Option<Vec<Param>> {
    let Some(params) = block.parameters() else { return Some(Vec::new()) };
    let params = params.as_block_parameters_node()?;
    if params.locals().iter().next().is_some() {
        return None;
    }
    let Some(list) = params.parameters() else { return Some(Vec::new()) };
    let plain = list.optionals().iter().next().is_none()
        && list.rest().is_none()
        && list.posts().iter().next().is_none()
        && list.keywords().iter().next().is_none()
        && list.keyword_rest().is_none()
        && list.block().is_none();
    if !plain {
        return None;
    }
    list.requireds().iter().map(|n| param_of(&n)).collect()
}

/// Bind `params` to the values one iteration yields, by a block's rules:
/// one parameter takes the first value; several take a lone Array's
/// elements (auto-splat) or the values in order, `nil` past the end.
fn bind(params: &[Param], yielded: &[Elem], out: &mut Vec<(String, Elem)>) -> Option<()> {
    match params {
        [] => Some(()),
        [only] => bind_one(only, yielded.first().cloned().unwrap_or_else(Elem::nil), out),
        _ => {
            let values = match yielded {
                [one] => one.splat(params.len())?,
                many => {
                    let mut v: Vec<Elem> = many.iter().take(params.len()).cloned().collect();
                    while v.len() < params.len() {
                        v.push(Elem::nil());
                    }
                    v
                }
            };
            for (param, value) in params.iter().zip(values) {
                bind_one(param, value, out)?;
            }
            Some(())
        }
    }
}

fn bind_one(param: &Param, value: Elem, out: &mut Vec<(String, Elem)>) -> Option<()> {
    match param {
        Param::Name(name) => {
            out.push((name.clone(), value));
            Some(())
        }
        Param::Multi(inner) => {
            for (param, value) in inner.iter().zip(value.splat(inner.len())?) {
                bind_one(param, value, out)?;
            }
            Some(())
        }
    }
}

/// The arguments of a call of a test macro, bound to its parameters by
/// Ruby's rules, each a value its tests' names can use and the
/// expression that binds it at the start of each test's body.
pub(super) struct Bound(Vec<(String, Elem)>);

impl Bound {
    pub(super) fn names(&self) -> Vec<String> {
        self.0.iter().map(|(n, _)| n.clone()).collect()
    }

    /// `param = <argument>`, for each parameter in order.
    pub(super) fn prelude(&self) -> Vec<Expr> {
        self.0
            .iter()
            .map(|(name, elem)| {
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Assign {
                        target: LValue::Var { id: VarId(0), name: Symbol::from(name.as_str()) },
                        value: elem.expr.clone(),
                    },
                )
            })
            .collect()
    }

    /// The String `node` (a test name in the macro's body) evaluates to
    /// with the parameters bound.
    pub(super) fn string(&self, node: &Node<'_>) -> Option<String> {
        let env = Env { vars: env_of(&self.0), cx: None, mode: Mode::Exact };
        match eval(node, &env)? {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Bind `call`'s arguments (written in `call_file`, constants looked up
/// by `cx`) to `def`'s parameters (in `def_file`). `Err` names what is
/// not modeled: a splat, a block, an argument not known before it runs
/// that a test could not re-evaluate, a default that is not a literal,
/// or arguments Ruby would refuse (`ArgumentError`).
pub(super) fn bind_call(
    def: &ruby_prism::DefNode<'_>,
    def_file: &str,
    call: &ruby_prism::CallNode<'_>,
    call_file: &str,
    cx: Option<Rc<consts::ConstCx>>,
) -> IngestResult<Result<Bound, String>> {
    let env = Env { vars: Vec::new(), cx, mode: Mode::Exact };
    if call.block().is_some() {
        return Ok(Err("it is called with a block".into()));
    }
    let mut positional: Vec<Node<'_>> = Vec::new();
    let mut keywords: Vec<(String, Node<'_>)> = Vec::new();
    for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
        if let Some(hash) = arg.as_keyword_hash_node() {
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else { return Ok(Err("its keywords are splatted".into())) };
                let Some(key) = assoc.key().as_symbol_node().and_then(|s| utf8(s.unescaped())) else {
                    return Ok(Err("a keyword is not a Symbol".into()));
                };
                if keywords.iter().any(|(k, _)| *k == key) {
                    return Ok(Err(format!("keyword `{key}` is given twice")));
                }
                keywords.push((key, assoc.value()));
            }
        } else if arg.as_splat_node().is_some() || arg.as_block_argument_node().is_some() || arg.as_forwarding_arguments_node().is_some() {
            return Ok(Err("its arguments are splatted".into()));
        } else {
            positional.push(arg);
        }
    }
    let mut required: Vec<String> = Vec::new();
    let mut optional: Vec<(String, Node<'_>)> = Vec::new();
    let mut required_kw: Vec<String> = Vec::new();
    let mut optional_kw: Vec<(String, Node<'_>)> = Vec::new();
    if let Some(params) = def.parameters() {
        if params.rest().is_some()
            || params.posts().iter().next().is_some()
            || params.keyword_rest().is_some()
            || params.block().is_some()
        {
            return Ok(Err("its parameters are not modeled".into()));
        }
        for p in params.requireds().iter() {
            let Some(r) = p.as_required_parameter_node() else { return Ok(Err("its parameters are not modeled".into())) };
            required.push(constant_id_str(&r.name()).to_string());
        }
        for p in params.optionals().iter() {
            let Some(o) = p.as_optional_parameter_node() else { return Ok(Err("its parameters are not modeled".into())) };
            optional.push((constant_id_str(&o.name()).to_string(), o.value()));
        }
        for p in params.keywords().iter() {
            if let Some(k) = p.as_required_keyword_parameter_node() {
                required_kw.push(constant_id_str(&k.name()).trim_end_matches(':').to_string());
            } else if let Some(k) = p.as_optional_keyword_parameter_node() {
                optional_kw.push((constant_id_str(&k.name()).trim_end_matches(':').to_string(), k.value()));
            } else {
                return Ok(Err("its parameters are not modeled".into()));
            }
        }
    }
    if !keywords.is_empty() && required_kw.is_empty() && optional_kw.is_empty() {
        return Ok(Err("it takes no keywords; a Hash argument is not modeled".into()));
    }
    if positional.len() < required.len() || positional.len() > required.len() + optional.len() {
        return Ok(Err("its arguments do not match its parameters (ArgumentError)".into()));
    }
    let argument = |node: &Node<'_>| -> IngestResult<Result<Elem, String>> {
        Ok(elem_of(node, call_file, &env)?.ok_or_else(|| format!("argument `{}` is not known before it runs", first_line(node))))
    };
    let default = |node: &Node<'_>| -> IngestResult<Result<Elem, String>> {
        match elem_of(node, def_file, &Env::default())? {
            Some(elem) if elem.value.is_literal() => Ok(Ok(elem)),
            _ => Ok(Err(format!("default `{}` is not a literal", first_line(node)))),
        }
    };
    let mut bound: Vec<(String, Elem)> = Vec::new();
    let mut given = positional.iter();
    for name in &required {
        match argument(given.next().expect("counted"))? {
            Ok(elem) => bound.push((name.clone(), elem)),
            Err(why) => return Ok(Err(why)),
        }
    }
    for (name, value) in &optional {
        let elem = match given.next() {
            Some(arg) => argument(arg)?,
            None => default(value)?,
        };
        match elem {
            Ok(elem) => bound.push((name.clone(), elem)),
            Err(why) => return Ok(Err(why)),
        }
    }
    for (key, _) in &keywords {
        if !required_kw.contains(key) && !optional_kw.iter().any(|(k, _)| k == key) {
            return Ok(Err(format!("unknown keyword `{key}` (ArgumentError)")));
        }
    }
    for name in &required_kw {
        let Some((_, value)) = keywords.iter().find(|(k, _)| k == name) else {
            return Ok(Err(format!("missing keyword `{name}` (ArgumentError)")));
        };
        match argument(value)? {
            Ok(elem) => bound.push((name.clone(), elem)),
            Err(why) => return Ok(Err(why)),
        }
    }
    for (name, default_value) in &optional_kw {
        let elem = match keywords.iter().find(|(k, _)| k == name) {
            Some((_, value)) => argument(value)?,
            None => default(default_value)?,
        };
        match elem {
            Ok(elem) => bound.push((name.clone(), elem)),
            Err(why) => return Ok(Err(why)),
        }
    }
    Ok(Ok(Bound(bound)))
}

/// What one class-body statement that declares tests becomes.
struct Unroll<'a, 'pr> {
    class_body: &'a [Node<'pr>],
    file: &'a str,
    tags: &'a TestTags,
    consts: &'a TagConstants<'pr>,
    /// Where the statement's constants are looked up.
    cx: Option<Rc<consts::ConstCx>>,
}

impl<'pr> Unroll<'_, 'pr> {
    /// The tests a static loop declares, or why it is not modeled.
    /// `bound` holds the enclosing loops' parameters, outer first.
    fn unroll_loop(
        &self,
        call: &ruby_prism::CallNode<'pr>,
        bound: &[(String, Elem)],
        out: &mut Vec<Test>,
    ) -> IngestResult<Result<(), NotModeled>> {
        let method = constant_id_str(&call.name()).to_string();
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return Ok(Err(not_modeled(format!("a `{method}` call"), first_line(&call.as_node()))));
        };
        let Some(receiver) = call.receiver() else {
            return Ok(Err(not_modeled(format!("the block of `{method}`"), first_line(&call.as_node()))));
        };
        if call.arguments().is_some() {
            return Ok(Err(not_modeled(format!("a `{method}` block"), first_line(&call.as_node()))));
        }
        let before = call.location().start_offset();
        // What each iteration yields to the block.
        let iterations: Vec<Vec<Elem>> = match method.as_str() {
            "times" => {
                let Some(Value::Int(n)) = receiver.as_integer_node().and_then(|_| eval(&receiver, &Env::default())) else {
                    return Ok(Err(not_modeled("a loop over a collection that is not a literal", first_line(&receiver))));
                };
                if n > MAX_ITERATIONS as i128 {
                    return Ok(Err(not_modeled(format!("a loop of more than {MAX_ITERATIONS} iterations"), first_line(&receiver))));
                }
                (0..n.max(0)).map(|i| vec![Elem::int(i)]).collect()
            }
            "each" | "each_with_index" | "each_pair" => {
                let env = Env { vars: env_of(bound), cx: self.cx.clone(), mode: Mode::Exact };
                let Some(found) = collection(&receiver, self.class_body, before, self.file, &env)? else {
                    return Ok(Err(not_modeled("a loop over a collection that is not a literal", first_line(&receiver))));
                };
                let items: Vec<Elem> = match found {
                    Collection::Array(_) if method == "each_pair" => {
                        return Ok(Err(not_modeled("a loop over a collection that is not a literal", first_line(&receiver))));
                    }
                    Collection::Array(elems) => elems,
                    Collection::Hash(entries) => entries.into_iter().map(|(k, v)| Elem::pair(k, v)).collect(),
                };
                if items.len() > MAX_ITERATIONS {
                    return Ok(Err(not_modeled(format!("a loop of more than {MAX_ITERATIONS} iterations"), first_line(&receiver))));
                }
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, item)| if method == "each_with_index" { vec![item, Elem::int(i as i128)] } else { vec![item] })
                    .collect()
            }
            _ => return Ok(Err(not_modeled(format!("a `{method}` block"), first_line(&call.as_node())))),
        };
        let Some(params) = block_params(&block) else {
            return Ok(Err(not_modeled("a loop whose block parameters are not modeled", first_line(&call.as_node()))));
        };
        let stmts = block.body().map(flatten_statements).unwrap_or_default();
        for yielded in iterations {
            let mut scope = bound.to_vec();
            if bind(&params, &yielded, &mut scope).is_none() {
                return Ok(Err(not_modeled("a loop whose block parameters are not modeled", first_line(&call.as_node()))));
            }
            for stmt in &stmts {
                if let Some(inner) = stmt.as_call_node().filter(|c| c.receiver().is_some() && c.block().is_some()) {
                    if let Err(why) = self.unroll_loop(&inner, &scope, out)? {
                        return Ok(Err(why));
                    }
                    continue;
                }
                // `client_name = …` beside the tests: a value each test's
                // closure reads, bound like a block parameter.
                if let Some(write) = stmt.as_local_variable_write_node() {
                    let env = Env { vars: env_of(&scope), cx: self.cx.clone(), mode: Mode::Exact };
                    let Some(elem) = elem_of(&write.value(), self.file, &env)? else {
                        return Ok(Err(not_modeled("a loop whose block does more than declare tests", first_line(stmt))));
                    };
                    scope.push((constant_id_str(&write.name()).to_string(), elem));
                    continue;
                }
                let Some(test_call) = stmt.as_call_node().filter(|c| is_test_call(c)) else {
                    return Ok(Err(not_modeled("a loop whose block does more than declare tests", first_line(stmt))));
                };
                let env = Env { vars: env_of(&scope), cx: self.cx.clone(), mode: Mode::Exact };
                let name_node = test_call.arguments().and_then(|a| a.arguments().iter().next()).expect("test args");
                let name = match eval(&name_node, &env) {
                    Some(Value::Str(name)) => name,
                    _ => return Ok(Err(not_modeled("a loop whose test names are not known before it runs", first_line(&name_node)))),
                };
                let prelude: Vec<Expr> = scope
                    .iter()
                    .map(|(name, elem)| {
                        Expr::new(
                            Span::synthetic(),
                            ExprNode::Assign {
                                target: LValue::Var { id: VarId(0), name: Symbol::from(name.as_str()) },
                                value: elem.expr.clone(),
                            },
                        )
                    })
                    .collect();
                match dsl_test(&test_call, name, prelude, self.file, self.tags, self.consts)? {
                    Ok(test) => out.push(test),
                    Err(why) => return Ok(Err(not_modeled(format!("a loop whose `test` {}", why.0), why.1))),
                }
            }
        }
        Ok(Ok(()))
    }
}

/// The values of bound names, for evaluating with them in scope.
fn env_of(bound: &[(String, Elem)]) -> Vec<(String, Value)> {
    bound.iter().map(|(n, e)| (n.clone(), e.value.clone())).collect()
}

/// A receiverless `test` call with arguments.
fn is_test_call(call: &ruby_prism::CallNode<'_>) -> bool {
    call.receiver().is_none() && call.name().as_slice() == b"test" && call.arguments().is_some()
}

/// Does `def`'s body declare a test when it runs (`test "…"` inside)?
fn def_declares_tests(def: &ruby_prism::DefNode<'_>) -> bool {
    def.body().is_some_and(|body| super::hidden_test_declaration(&body).is_some())
}

/// `stmt`, a class-body statement that declares tests (`offset` is the
/// first one) and that ingest did not take as one literal test: the
/// tests of a static loop, in the order it declares them (the caller
/// adds each, `super::declare_test`), or none, with just this
/// statement refused by kind and line. Under `--survey` the refusal is
/// recorded and the class's other tests still run; strict ingest fails
/// with it.
pub(super) fn declare_hidden<'pr>(
    stmt: &Node<'pr>,
    offset: usize,
    class_node: &ruby_prism::ClassNode<'pr>,
    class: &ClassId,
    file: &str,
    tags: &TestTags,
    consts: &TagConstants<'pr>,
) -> IngestResult<Vec<Test>> {
    let class_body: Vec<Node<'pr>> = class_node.body().map(flatten_statements).unwrap_or_default();
    let cx = consts::ConstCx::of_class(class_node, &class_body, file, stmt.location().start_offset()).map(Rc::new);
    let unroll = Unroll { class_body: &class_body, file, tags, consts, cx };
    let outcome: Result<Vec<Test>, NotModeled> = if let Some(call) = stmt.as_call_node().filter(|c| is_test_call(c)) {
        // A `test` ingest_test_declaration did not take: a computed name,
        // or tags or a block it does not model.
        let name_node = call.arguments().and_then(|a| a.arguments().iter().next()).expect("test args");
        match static_string(&name_node) {
            None => Err(not_modeled("a `test` whose name is computed", first_line(&name_node))),
            Some(name) => match dsl_test(&call, name, Vec::new(), file, tags, consts)? {
                Ok(test) => Ok(vec![test]),
                Err(why) => Err(not_modeled(format!("a `test` whose {}", why.0), why.1)),
            },
        }
    } else if stmt.as_call_node().is_some_and(|c| super::is_generate_test(&c)) {
        Err(not_modeled("TestGenerator's `generate_test`", first_line(stmt)))
    } else if let Some(call) = stmt.as_call_node() {
        let mut out = Vec::new();
        unroll.unroll_loop(&call, &[], &mut out)?.map(|()| out)
    } else if let Some(singleton) = stmt.as_singleton_class_node() {
        // `def test_x` in `class << self` is a class method, never run as
        // a test. A method there whose body declares tests is a macro: the
        // tests are declared where it is called.
        let body = singleton.body().map(flatten_statements).unwrap_or_default();
        let macros: Vec<String> = body
            .iter()
            .filter_map(|s| s.as_def_node())
            .filter(def_declares_tests)
            .map(|d| format!("`{}`", constant_id_str(&d.name())))
            .collect();
        let direct = body.iter().any(|s| s.as_def_node().is_none() && super::hidden_test_declaration(s).is_some());
        if direct {
            Err(not_modeled("a `class << self` body", first_line(stmt)))
        } else if macros.is_empty() {
            return Ok(Vec::new());
        } else {
            Err(not_modeled("a `class << self` method that declares tests when called", macros.join(", ")))
        }
    } else if stmt.as_if_node().is_some() || stmt.as_unless_node().is_some() || stmt.as_case_node().is_some() {
        Err(not_modeled("a conditional", first_line(stmt)))
    } else {
        Err(not_modeled("a class-body statement ingest does not model", first_line(stmt)))
    };
    match outcome {
        Ok(declared) => Ok(declared),
        Err(why) => {
            let line = |at: usize| super::super::sources::line_at(file, at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
            let stmt_line = line(stmt.location().start_offset());
            let test_line = line(offset);
            let at = if stmt_line == test_line { format!("line {stmt_line}") } else { format!("line {stmt_line}, first test at line {test_line}") };
            super::super::survey::continue_or_fail(IngestError::Unsupported {
                file: file.into(),
                message: format!(
                    "tests declared by {} are not modeled; they will not run (`{}` {at}: {})",
                    why.kind, class.0, why.detail
                ),
            })?;
            Ok(Vec::new())
        }
    }
}

/// Constants a loop reads: those the test class body writes above it,
/// and those another registered source (an app model, a test helper)
/// declares, looked up by Ruby's rules for a written constant.
///
/// `A::B::C` resolves `A` through the lexical scopes, innermost first,
/// then at the top level; each later name must be declared directly in
/// the one before it (one inherited from an ancestor is not looked
/// for). A name is known when exactly one statement declares it, run
/// unconditionally (not in a block, a conditional or a `class << self`);
/// a `module`/`class` reopened anywhere stays a namespace. A constant
/// made `private_constant` is refused where Ruby raises (`A::PRIVATE`).
/// The lookup covers the sources registered when the loop is ingested:
/// app models and libraries, and test files that sort before the loop's.
/// A scope the innermost class includes that declares the name is
/// refused (Ruby would look there before the top level).
pub(super) mod consts {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use ruby_prism::Node;

    use super::super::super::sources;
    use super::super::super::util::constant_id_str;
    use super::{eval, Env, Mode, Value};
    use crate::span::FileId;

    /// Constants defined by constants, at most this deep.
    const MAX_DEPTH: usize = 8;

    /// Where an expression is written: its file and lexical scopes (full
    /// names, outer first), and in a test class body, what that body
    /// writes above the loop.
    pub(in super::super) struct ConstCx {
        pub(in super::super) file: String,
        pub(in super::super) cref: Vec<String>,
        pub(in super::super) local: Option<Local>,
        /// The modules the innermost scope includes, as written.
        pub(in super::super) includes: Vec<Vec<String>>,
        pub(in super::super) depth: usize,
    }

    /// The constants a test class body writes, and where the loop is.
    #[derive(Clone)]
    pub(in super::super) struct Local {
        pub(in super::super) class: String,
        /// (name, statement start, value start, value end).
        pub(in super::super) writes: Vec<(String, usize, usize, usize)>,
        pub(in super::super) before: usize,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Kind {
        Const,
        Module,
        Class,
    }

    /// One statement that declares a constant named what was searched for.
    #[derive(Clone, Debug)]
    struct Decl {
        path: String,
        /// Full names it may declare (two for a compact `module A::B`
        /// inside another scope, whose `A` Ruby resolves when it runs).
        names: Vec<String>,
        certain: bool,
        kind: Kind,
        /// The assigned value (`Const`) or the whole `module`/`class`.
        start: usize,
        end: usize,
        /// Lexical scopes at the statement, outer first.
        cref: Vec<String>,
        private: bool,
    }

    enum Found {
        Missing,
        Unknown,
        Namespace,
        Value(Decl),
    }

    enum Hit {
        Namespace(String),
        Decl(Decl),
        /// A test class body constant: (statement start, value start, end).
        Local(usize, usize, usize),
    }

    impl ConstCx {
        /// This context for a statement at `at` of the same body.
        pub(in super::super) fn before(&self, at: usize) -> ConstCx {
            ConstCx {
                file: self.file.clone(),
                cref: self.cref.clone(),
                local: self.local.clone().map(|l| Local { before: at, ..l }),
                includes: self.includes.clone(),
                depth: self.depth,
            }
        }

        /// The context of a statement at `before` in the body of the test
        /// class `class_node` of `file`. `None` when its lexical scopes
        /// are not certain (a compact `module A::B` inside another).
        pub(in super::super) fn of_class(
            class_node: &ruby_prism::ClassNode<'_>,
            class_body: &[Node<'_>],
            file: &str,
            before: usize,
        ) -> Option<ConstCx> {
            let cref = cref_of(file, class_node.location().start_offset())?;
            let writes = class_body
                .iter()
                .filter_map(|stmt| {
                    let write = stmt.as_constant_write_node()?;
                    let value = write.value().location();
                    Some((
                        constant_id_str(&write.name()).to_string(),
                        stmt.location().start_offset(),
                        value.start_offset(),
                        value.end_offset(),
                    ))
                })
                .collect();
            let includes = class_body
                .iter()
                .filter_map(|stmt| stmt.as_call_node())
                .filter(|call| call.receiver().is_none() && call.name().as_slice() == b"include")
                .flat_map(|call| call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default())
                .filter_map(|arg| segments(&arg).map(|(s, _)| s))
                .collect();
            Some(ConstCx {
                file: file.to_string(),
                local: Some(Local { class: cref.last()?.clone(), writes, before }),
                cref,
                includes,
                depth: 0,
            })
        }
    }

    /// The lexical scopes (full names, outer first) inside the `class`
    /// or `module` that starts at `start` in `file`, itself last.
    pub(in super::super) fn cref_of(file: &str, start: usize) -> Option<Vec<String>> {
        struct Scopes {
            stack: Vec<(String, bool)>,
            depth: Vec<bool>,
            start: usize,
            found: Option<Vec<String>>,
        }
        impl<'pr> ruby_prism::Visit<'pr> for Scopes {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                let path = node
                    .as_module_node()
                    .map(|m| m.constant_path())
                    .or_else(|| node.as_class_node().map(|c| c.constant_path()));
                let Some(path) = path else {
                    self.depth.push(false);
                    return;
                };
                let (name, exact) = match segments(&path) {
                    Some((segs, rooted)) => {
                        let joined = segs.join("::");
                        match self.stack.last() {
                            _ if rooted => (joined, true),
                            None => (joined, true),
                            Some((outer, outer_exact)) => (format!("{outer}::{joined}"), *outer_exact && segs.len() == 1),
                        }
                    }
                    None => (String::from("?"), false),
                };
                self.stack.push((name, exact));
                self.depth.push(true);
                if node.location().start_offset() == self.start && self.found.is_none() {
                    if self.stack.iter().all(|(_, exact)| *exact) {
                        self.found = Some(self.stack.iter().map(|(n, _)| n.clone()).collect());
                    } else {
                        self.found = Some(Vec::new());
                    }
                }
            }
            fn visit_branch_node_leave(&mut self) {
                if self.depth.pop() == Some(true) {
                    self.stack.pop();
                }
            }
        }
        use ruby_prism::Visit;
        sources::with_text(file, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let mut scopes = Scopes { stack: Vec::new(), depth: Vec::new(), start, found: None };
            scopes.visit(&result.node());
            scopes.found.filter(|f| !f.is_empty())
        })
        .flatten()
    }

    /// The value of the constant `node` (`C`, `A::C`, `::A::C`) reads.
    pub(super) fn value(node: &Node<'_>, env: &Env) -> Option<Value> {
        let cx = env.cx.as_ref()?;
        if cx.depth > MAX_DEPTH {
            return None;
        }
        let (segments, rooted) = segments(node)?;
        let (first, rest) = segments.split_first()?;
        let mut hit = if rooted { top(first)? } else { lexical(first, cx)? };
        for seg in rest {
            let Value::Namespace(ns) = hit_value(hit, cx)? else { return None };
            let full = format!("{ns}::{seg}");
            hit = match find(&full) {
                Found::Namespace => Hit::Namespace(full),
                Found::Value(decl) if !decl.private => Hit::Decl(decl),
                _ => return None,
            };
        }
        hit_value(hit, cx)
    }

    /// What a constant's first name resolves to where it is written.
    #[derive(Debug, PartialEq)]
    pub(in super::super) enum Resolution {
        /// The full name it denotes.
        Declared(String),
        /// No registered source declares it in any scope searched: a
        /// gem's or Ruby's top-level constant, the same from anywhere.
        Missing,
        /// Not known (declared more than once, conditionally, …).
        Unknown,
    }

    /// `name` read in a scope whose lexical scopes are `cref` (no class
    /// body constants, no ancestors).
    pub(in super::super) fn first_name(cref: &[String], name: &str) -> Resolution {
        for scope in cref.iter().rev() {
            let full = format!("{scope}::{name}");
            match find(&full) {
                Found::Missing => continue,
                Found::Unknown => return Resolution::Unknown,
                Found::Namespace | Found::Value(_) => return Resolution::Declared(full),
            }
        }
        match find(name) {
            Found::Missing => Resolution::Missing,
            Found::Unknown => Resolution::Unknown,
            Found::Namespace | Found::Value(_) => Resolution::Declared(name.to_string()),
        }
    }

    /// The full name of the module `segments` names where `cref` is.
    pub(in super::super) fn namespace(cref: &[String], segments: &[String]) -> Option<String> {
        let (first, rest) = segments.split_first()?;
        let Resolution::Declared(mut full) = first_name(cref, first) else { return None };
        if !matches!(find(&full), Found::Namespace) {
            return None;
        }
        for seg in rest {
            full = format!("{full}::{seg}");
            if !matches!(find(&full), Found::Namespace) {
                return None;
            }
        }
        Some(full)
    }

    /// `A::B::C` as its names, and whether it starts with `::`.
    pub(in super::super) fn segments(node: &Node<'_>) -> Option<(Vec<String>, bool)> {
        if let Some(read) = node.as_constant_read_node() {
            return Some((vec![constant_id_str(&read.name()).to_string()], false));
        }
        let path = node.as_constant_path_node()?;
        let name = constant_id_str(&path.name()?).to_string();
        match path.parent() {
            None => Some((vec![name], true)),
            Some(parent) => {
                let (mut segs, rooted) = segments(&parent)?;
                segs.push(name);
                Some((segs, rooted))
            }
        }
    }

    fn top(name: &str) -> Option<Hit> {
        match find(name) {
            Found::Namespace => Some(Hit::Namespace(name.to_string())),
            Found::Value(decl) => Some(Hit::Decl(decl)),
            Found::Missing | Found::Unknown => None,
        }
    }

    /// Ruby's lookup of a constant's first name where it is written.
    fn lexical(name: &str, cx: &ConstCx) -> Option<Hit> {
        for scope in cx.cref.iter().rev() {
            if let Some(local) = cx.local.as_ref().filter(|l| &l.class == scope) {
                let mut writes = local.writes.iter().filter(|w| w.0 == name);
                if let Some(w) = local.writes.iter().filter(|w| w.0 == name && w.1 < local.before).last() {
                    return Some(Hit::Local(w.1, w.2, w.3));
                }
                // Written below: the loop runs before it exists.
                if writes.next().is_some() {
                    return None;
                }
            }
            let full = format!("{scope}::{name}");
            match find(&full) {
                Found::Missing => continue,
                Found::Unknown => return None,
                Found::Namespace => return Some(Hit::Namespace(full)),
                Found::Value(decl) => return Some(Hit::Decl(decl)),
            }
        }
        // Ancestors of the innermost scope come before the top level.
        for include in &cx.includes {
            let resolved = match include.split_first() {
                Some((first, rest)) => {
                    let base = ConstCx { file: cx.file.clone(), cref: cx.cref.clone(), local: None, includes: Vec::new(), depth: cx.depth };
                    let mut hit = lexical(first, &base);
                    for seg in rest {
                        hit = match hit {
                            Some(Hit::Namespace(ns)) => match find(&format!("{ns}::{seg}")) {
                                Found::Namespace => Some(Hit::Namespace(format!("{ns}::{seg}"))),
                                _ => None,
                            },
                            _ => None,
                        };
                    }
                    hit
                }
                None => None,
            };
            if let Some(Hit::Namespace(ns)) = resolved {
                if !matches!(find(&format!("{ns}::{name}")), Found::Missing) {
                    return None;
                }
            }
        }
        top(name)
    }

    fn hit_value(hit: Hit, cx: &ConstCx) -> Option<Value> {
        match hit {
            Hit::Namespace(name) => Some(Value::Namespace(name)),
            Hit::Decl(decl) => evaluate_at(&decl.path, decl.start, decl.end, decl.cref.clone(), None, cx.depth + 1),
            Hit::Local(stmt, start, end) => {
                let local = cx.local.as_ref()?;
                let local = Local { before: stmt, ..local.clone() };
                evaluate_at(&cx.file, start, end, cx.cref.clone(), Some(local), cx.depth + 1)
            }
        }
    }

    /// The value of the expression at `start..end` of `path`, as Ruby
    /// computed it when the file loaded: a part not known is `Opaque`.
    fn evaluate_at(path: &str, start: usize, end: usize, cref: Vec<String>, local: Option<Local>, depth: usize) -> Option<Value> {
        sources::with_text(path, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let node = node_at(&root, start, end)?;
            let cx = ConstCx { file: path.to_string(), cref, local, includes: Vec::new(), depth };
            let env = Env { vars: Vec::new(), cx: Some(Rc::new(cx)), mode: Mode::Loaded };
            Some(eval(&node, &env).unwrap_or(Value::Opaque))
        })
        .flatten()
    }

    fn node_at<'pr>(root: &Node<'pr>, start: usize, end: usize) -> Option<Node<'pr>> {
        struct Find<'pr> {
            start: usize,
            end: usize,
            found: Option<Node<'pr>>,
        }
        impl<'pr> ruby_prism::Visit<'pr> for Find<'pr> {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                self.check(node);
            }
            fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
                self.check(node);
            }
        }
        impl<'pr> Find<'pr> {
            fn check(&mut self, node: Node<'pr>) {
                // A file's program and statements nodes span exactly
                // what its one top-level statement spans.
                if node.as_program_node().is_some() || node.as_statements_node().is_some() {
                    return;
                }
                let loc = node.location();
                if self.found.is_none() && loc.start_offset() == self.start && loc.end_offset() == self.end {
                    self.found = Some(node);
                }
            }
        }
        use ruby_prism::Visit;
        let mut find = Find { start, end, found: None };
        find.visit(root);
        find.found
    }

    /// Would `node`, evaluated again in a test body, give what it gave
    /// in the class body? A constant (not one the class body writes
    /// below the loop), a literal, an Array or Hash of these, and
    /// `Const.new(…)`, `….to_json` and `….freeze` of these.
    pub(super) fn rebuildable(node: &Node<'_>, env: &Env) -> bool {
        if let Some(read) = node.as_constant_read_node() {
            let name = constant_id_str(&read.name());
            return env
                .cx
                .as_ref()
                .and_then(|cx| cx.local.as_ref())
                .is_none_or(|l| !l.writes.iter().any(|w| w.0 == name && w.1 >= l.before));
        }
        if node.as_constant_path_node().is_some() {
            return segments(node).is_some();
        }
        let exact = Env { mode: Mode::Exact, ..env.clone() };
        if eval(node, &exact).is_some_and(|v| v.is_literal()) {
            return true;
        }
        if let Some(array) = node.as_array_node() {
            return array.elements().iter().all(|e| e.as_splat_node().is_none() && rebuildable(&e, env));
        }
        if let Some(hash) = node.as_hash_node() {
            return hash.elements().iter().all(|e| match e.as_assoc_node() {
                Some(assoc) => eval(&assoc.key(), &exact).is_some_and(|k| k.is_literal()) && rebuildable(&assoc.value(), env),
                None => false,
            });
        }
        if let Some(kw) = node.as_keyword_hash_node() {
            return kw.elements().iter().all(|e| match e.as_assoc_node() {
                Some(assoc) => assoc.key().as_symbol_node().is_some() && rebuildable(&assoc.value(), env),
                None => false,
            });
        }
        if let Some(call) = node.as_call_node() {
            let Some(receiver) = call.receiver() else { return false };
            if call.block().is_some() {
                return false;
            }
            let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
            return match constant_id_str(&call.name()) {
                "new" => {
                    segments(&receiver).is_some()
                        && rebuildable(&receiver, env)
                        && args.iter().all(|a| rebuildable(a, env))
                }
                "to_json" | "freeze" => args.is_empty() && rebuildable(&receiver, env),
                _ => false,
            };
        }
        false
    }

    /// `Module#constants` of `module`: the names its one `module` body
    /// assigns, in order. `None` unless that body only assigns constants
    /// and defines methods, no other source opens the module or writes
    /// into it, and Zeitwerk has no directory of files for it.
    pub(super) fn module_constants(module: &str) -> Option<Vec<Value>> {
        let last = module.rsplit("::").next()?;
        let decls: Vec<Decl> = decls(last).into_iter().filter(|d| d.names.iter().any(|n| n == module)).collect();
        let [decl] = decls.as_slice() else { return None };
        if !decl.certain || decl.names.len() != 1 || decl.kind != Kind::Module {
            return None;
        }
        let dir = format!("/{}/", underscore(module));
        let mut id = 1;
        while let Some(path) = sources::path_of(FileId(id)) {
            if path.contains(&dir) {
                return None;
            }
            if path != decl.path {
                let written = sources::with_text(&path, |text| {
                    text.contains(&format!("{last}.const_set")) || text.contains(&format!("{last}::const_set"))
                        || assigns_into(text, last)
                });
                if written != Some(false) {
                    return None;
                }
            }
            id += 1;
        }
        sources::with_text(&decl.path, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let node = node_at(&root, decl.start, decl.end)?;
            let module_node = node.as_module_node()?;
            let mut out = Vec::new();
            for stmt in module_node.body().map(super::super::super::util::flatten_statements).unwrap_or_default() {
                if let Some(write) = stmt.as_constant_write_node() {
                    out.push(Value::Sym(constant_id_str(&write.name()).to_string()));
                } else if stmt.as_def_node().is_some() {
                } else if let Some(singleton) = stmt.as_singleton_class_node() {
                    let body = singleton.body().map(super::super::super::util::flatten_statements).unwrap_or_default();
                    if !body.iter().all(|s| s.as_def_node().is_some() || is_declaration_call(s)) {
                        return None;
                    }
                } else if is_declaration_call(&stmt) && !stmt.as_call_node().is_some_and(|c| c.name().as_slice() == b"include") {
                } else {
                    return None;
                }
            }
            Some(out)
        })
        .flatten()
    }

    /// `extend T::Sig`, `private`, `sig { … }`: a call that adds no
    /// constant to the module it is in.
    fn is_declaration_call(node: &Node<'_>) -> bool {
        let Some(call) = node.as_call_node() else { return false };
        call.receiver().is_none()
            && match constant_id_str(&call.name()) {
                "extend" | "private" | "public" | "protected" | "module_function" | "private_class_method" => {
                    call.block().is_none()
                }
                "sig" => call.arguments().is_none(),
                _ => false,
            }
    }

    /// Does `text` write `<module>::NAME = …` (a constant into `module`
    /// from outside its body)?
    fn assigns_into(text: &str, module: &str) -> bool {
        let needle = format!("{module}::");
        text.match_indices(&needle).any(|(at, _)| {
            let rest = &text[at + needle.len()..];
            let name_len = rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
            let after = rest[name_len..].trim_start_matches([' ', '\t']);
            name_len > 0 && after.starts_with('=') && !after.starts_with("==") && !after.starts_with("=~") && !after.starts_with("=>")
        })
    }

    /// `Inventory::AdjustmentReasons` as Zeitwerk's directory for it.
    fn underscore(module: &str) -> String {
        let mut out = String::new();
        let chars: Vec<char> = module.replace("::", "/").chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if c.is_ascii_uppercase() {
                let prev = i.checked_sub(1).map(|p| chars[p]);
                let next = chars.get(i + 1);
                let boundary = match prev {
                    Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit() => true,
                    Some(p) if p.is_ascii_uppercase() => next.is_some_and(|n| n.is_ascii_lowercase()),
                    _ => false,
                };
                if boundary {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(*c);
            }
        }
        out
    }

    fn find(full: &str) -> Found {
        let last = full.rsplit("::").next().unwrap_or(full);
        let found: Vec<Decl> = decls(last).into_iter().filter(|d| d.names.iter().any(|n| n == full)).collect();
        if found.is_empty() {
            return Found::Missing;
        }
        if found.iter().any(|d| !d.certain || d.names.len() > 1) {
            return Found::Unknown;
        }
        let consts: Vec<&Decl> = found.iter().filter(|d| d.kind == Kind::Const).collect();
        match consts.as_slice() {
            [] => Found::Namespace,
            [one] if found.len() == 1 => Found::Value((*one).clone()),
            _ => Found::Unknown,
        }
    }

    thread_local! {
        static DECLS: RefCell<Cache> = RefCell::new(Cache::default());
    }

    /// Declarations by name, and how many registered files each has seen.
    #[derive(Default)]
    struct Cache {
        first: Option<String>,
        by_name: HashMap<String, (u32, Vec<Decl>)>,
    }

    /// Every statement in a registered source that declares a constant
    /// called `name` (a module, a class, or an assignment).
    fn decls(name: &str) -> Vec<Decl> {
        let first = sources::path_of(FileId(1));
        DECLS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.first != first {
                cache.by_name.clear();
                cache.first = first;
            }
            let entry = cache.by_name.entry(name.to_string()).or_insert((0, Vec::new()));
            if entry.0 > 0 && sources::path_of(FileId(entry.0)).is_none() {
                *entry = (0, Vec::new());
            }
            let mut id = entry.0 + 1;
            while let Some(path) = sources::path_of(FileId(id)) {
                let found = sources::with_text(&path, |text| {
                    if may_declare(text, name) { scan(text, &path, name) } else { Vec::new() }
                });
                entry.1.extend(found.unwrap_or_default());
                entry.0 = id;
                id += 1;
            }
            entry.1.clone()
        })
    }

    /// A cheap test before parsing: does a line of `text` open a module
    /// or class called `name`, or assign it?
    fn may_declare(text: &str, name: &str) -> bool {
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let bytes = text.as_bytes();
        text.match_indices(name).any(|(at, _)| {
            let end = at + name.len();
            if (at > 0 && ident(bytes[at - 1])) || (end < bytes.len() && ident(bytes[end])) {
                return false;
            }
            let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
            let head = text[line_start..at].trim_start();
            if head.starts_with("module ") || head.starts_with("class ") {
                return true;
            }
            let after = text[end..].trim_start_matches([' ', '\t']);
            (after.starts_with('=') && !after.starts_with("==") && !after.starts_with("=~") && !after.starts_with("=>"))
                || ["||=", "&&=", "+=", "-=", "*=", "|=", "&=", "<<="].iter().any(|op| after.starts_with(op))
        })
    }

    fn scan(text: &str, path: &str, name: &str) -> Vec<Decl> {
        use ruby_prism::Visit;
        let result = ruby_prism::parse(text.as_bytes());
        let mut walk = Walk { name, path, parents: Vec::new(), scopes: Vec::new(), out: Vec::new(), privates: Vec::new() };
        walk.visit(&result.node());
        let Walk { mut out, privates, .. } = walk;
        for decl in out.iter_mut().filter(|d| d.kind == Kind::Const) {
            if privates.iter().any(|scope| decl.cref.last() == Some(scope)) {
                decl.private = true;
            }
        }
        out
    }

    struct Scope {
        name: String,
        /// Its full name is the one Ruby gives it.
        exact: bool,
        /// Index in `parents` of its `module`/`class` node.
        at: usize,
    }

    struct Walk<'a, 'pr> {
        name: &'a str,
        path: &'a str,
        parents: Vec<Node<'pr>>,
        scopes: Vec<Scope>,
        out: Vec<Decl>,
        /// Scopes whose body calls `private_constant :<name>`.
        privates: Vec<String>,
    }

    impl<'pr> Walk<'_, 'pr> {
        fn cref(&self) -> Vec<String> {
            self.scopes.iter().map(|s| s.name.clone()).collect()
        }

        /// Is the node being entered run whenever its scope's body runs:
        /// only statement lists between it and the scope?
        fn unconditional(&self) -> bool {
            let from = self.scopes.last().map_or(0, |s| s.at + 1);
            self.scopes.iter().all(|s| s.exact)
                && self.parents[from.min(self.parents.len())..]
                    .iter()
                    .all(|n| n.as_statements_node().is_some() || n.as_program_node().is_some())
        }

        fn qualified(&self, name: &str) -> String {
            match self.scopes.last() {
                Some(scope) => format!("{}::{name}", scope.name),
                None => name.to_string(),
            }
        }

        fn push(&mut self, names: Vec<String>, certain: bool, kind: Kind, start: usize, end: usize) {
            self.out.push(Decl {
                path: self.path.to_string(),
                names,
                certain,
                kind,
                start,
                end,
                cref: self.cref(),
                private: false,
            });
        }

        /// A `module`/`class` path: its full names, and whether Ruby
        /// certainly gives it the first.
        fn namespace_names(&self, path: &Node<'_>) -> Option<(Vec<String>, bool)> {
            let (segs, rooted) = segments(path)?;
            let joined = segs.join("::");
            Some(if rooted || self.scopes.is_empty() {
                (vec![joined], true)
            } else if segs.len() == 1 {
                (vec![self.qualified(&joined)], true)
            } else {
                (vec![self.qualified(&joined), joined], false)
            })
        }
    }

    impl<'pr> ruby_prism::Visit<'pr> for Walk<'_, 'pr> {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
            let unconditional = self.unconditional();
            let open = node
                .as_module_node()
                .map(|m| (m.constant_path(), Kind::Module))
                .or_else(|| node.as_class_node().map(|c| (c.constant_path(), Kind::Class)));
            if let Some((path, kind)) = open {
                let (names, exact) = self.namespace_names(&path).unwrap_or((vec![String::from("?")], false));
                let segs = segments(&path).map(|(s, _)| s).unwrap_or_default();
                if segs.last().is_some_and(|n| n == self.name) {
                    let loc = node.location();
                    self.push(names.clone(), unconditional && exact, kind, loc.start_offset(), loc.end_offset());
                }
                self.parents.push(node);
                self.scopes.push(Scope { name: names[0].clone(), exact: exact && unconditional, at: self.parents.len() - 1 });
                return;
            }
            if node.as_singleton_class_node().is_some() {
                // `class << self` has constants of its own.
                self.parents.push(node);
                self.scopes.push(Scope { name: String::from("#<Class>"), exact: false, at: self.parents.len() - 1 });
                return;
            }
            if let Some(write) = node.as_constant_write_node() {
                if constant_id_str(&write.name()) == self.name {
                    let value = write.value().location();
                    let names = vec![self.qualified(self.name)];
                    self.push(names, unconditional, Kind::Const, value.start_offset(), value.end_offset());
                }
            }
            let other_write = node
                .as_constant_or_write_node()
                .map(|w| w.name())
                .or_else(|| node.as_constant_and_write_node().map(|w| w.name()))
                .or_else(|| node.as_constant_operator_write_node().map(|w| w.name()));
            if other_write.is_some_and(|n| constant_id_str(&n) == self.name) {
                let names = vec![self.qualified(self.name)];
                self.push(names, false, Kind::Const, 0, 0);
            }
            let path_target = node
                .as_constant_path_write_node()
                .map(|w| w.target())
                .or_else(|| node.as_constant_path_or_write_node().map(|w| w.target()))
                .or_else(|| node.as_constant_path_and_write_node().map(|w| w.target()))
                .or_else(|| node.as_constant_path_operator_write_node().map(|w| w.target()));
            if let Some(target) = path_target {
                if target.name().is_some_and(|n| constant_id_str(&n) == self.name) {
                    let names = segments(&target.as_node())
                        .map(|(s, _)| vec![self.qualified(&s.join("::")), s.join("::")])
                        .unwrap_or_default();
                    self.push(names, false, Kind::Const, 0, 0);
                }
            }
            if let Some(call) = node.as_call_node() {
                if call.receiver().is_none() && call.name().as_slice() == b"private_constant" {
                    let names: Vec<String> = call
                        .arguments()
                        .map(|a| {
                            a.arguments()
                                .iter()
                                .filter_map(|arg| arg.as_symbol_node().and_then(|s| String::from_utf8(s.unescaped().to_vec()).ok()))
                                .collect()
                        })
                        .unwrap_or_default();
                    if names.iter().any(|n| n == self.name) {
                        if let Some(scope) = self.scopes.last() {
                            self.privates.push(scope.name.clone());
                        }
                    }
                }
            }
            self.parents.push(node);
        }

        fn visit_branch_node_leave(&mut self) {
            self.parents.pop();
            if self.scopes.last().is_some_and(|s| s.at == self.parents.len()) {
                self.scopes.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(src: &str) -> Option<Value> {
        let result = ruby_prism::parse(src.as_bytes());
        let root = result.node();
        let stmts = flatten_statements(root.as_program_node().unwrap().statements().as_node());
        eval(&stmts[0], &Env::default())
    }

    fn shown(src: &str) -> Option<String> {
        value_of(src).and_then(|v| match v {
            Value::Str(s) => Some(s),
            _ => None,
        })
    }

    // Each expected text is what Ruby 4.0.7 prints for the same source.
    #[test]
    fn interpolation_matches_ruby() {
        assert_eq!(shown(r##""a " "b""##).as_deref(), Some("a b"));
        assert_eq!(shown(r##""#{[1, nil, "a", :b]} #{{}} #{nil}|#{:sym}""##).as_deref(), Some(r##"[1, nil, "a", :b] {} |sym"##));
        assert_eq!(
            shown(r##""#{{country: "US", currencies: ["USD"]}}""##).as_deref(),
            Some(r##"{country: "US", currencies: ["USD"]}"##)
        );
        assert_eq!(
            shown(r##""#{{"k" => 1, :"a b" => 2, :+ => 3, :a? => 4, :b= => 5, :C => 6, :if => 7, :@iv => 8, 1 => nil}}""##).as_deref(),
            Some(r##"{"k" => 1, "a b": 2, "+": 3, a?: 4, "b=": 5, C: 6, if: 7, "@iv": 8, 1 => nil}"##)
        );
        assert_eq!(
            shown(r##""#{[:a, :"a b", :+, :a?, :b=, :C, :if, :@iv, :$g, :"9a", :"a-b", :[], :"foo?=", :!, :a!, :_x]}""##).as_deref(),
            Some(r##"[:a, :"a b", :+, :a?, :b=, :C, :if, :@iv, :$g, :"9a", :"a-b", :[], :"foo?=", :!, :a!, :_x]"##)
        );
        assert_eq!(
            shown(r##""#{["a\"b", "a\\b", "\#{x}", "\#$x", "\#@x", "#x", "\n\t\r\e\a\b\f\v", "\x01", "\x7f", "é"]}""##).as_deref(),
            Some(r##"["a\"b", "a\\b", "\#{x}", "\#$x", "\#@x", "#x", "\n\t\r\e\a\b\f\v", "\u0001", "\u007F", "é"]"##)
        );
        assert_eq!(shown(r##""#{[1, [2, [3, nil]], "x"].join(",")}|#{[1, [2]].join}""##).as_deref(), Some("1,2,3,,x|12"));
        assert_eq!(shown(r##""#{{a: 1}[:a]} #{{a: 1}.fetch(:a)} #{[1, 2][-1]} #{[1][5].inspect}""##).as_deref(), Some("1 1 2 nil"));
        assert_eq!(shown(r##""#{true ? "has" : "lacks"} #{nil ? "has" : "lacks"}""##).as_deref(), Some("has lacks"));
        // Not known before the code runs.
        assert_eq!(shown(r##""#{CONST}""##), None);
        assert_eq!(shown(r##""#{x}""##), None);
        assert_eq!(shown(r##""#{{a: 1}.fetch(:b)}""##), None);
        assert_eq!(shown(r##""#{"日本".inspect}""##).as_deref(), Some("\"日本\""));
        assert_eq!(shown(r##""#{"—".inspect}""##), None);
    }

    #[test]
    fn float_to_s_matches_ruby() {
        let cases: &[(f64, &str)] = &[
            (1.0, "1.0"),
            (1.5, "1.5"),
            (100.0, "100.0"),
            (1e14, "100000000000000.0"),
            (1e15, "1.0e+15"),
            (1.5e15, "1.5e+15"),
            (1e16, "1.0e+16"),
            (1.0e20, "1.0e+20"),
            (999999999999999.9, "999999999999999.9"),
            (123456789012345.6, "123456789012345.6"),
            (1234567890123456.7, "1234567890123456.8"),
            (9999999999999998.0, "9.999999999999998e+15"),
            (12345678901234567.0, "1.2345678901234568e+16"),
            (0.0001, "0.0001"),
            (0.00015, "0.00015"),
            (0.00001, "1.0e-05"),
            (2.5e-5, "2.5e-05"),
            (1.23456789012345e-7, "1.23456789012345e-07"),
            (1e100, "1.0e+100"),
            (5e-324, "5.0e-324"),
            (-0.0, "-0.0"),
            (-12.5, "-12.5"),
            (9.27, "9.27"),
            (0.1 + 0.2, "0.30000000000000004"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (f, want) in cases {
            assert_eq!(float_to_s(*f), *want, "{f:e}");
        }
    }
}
