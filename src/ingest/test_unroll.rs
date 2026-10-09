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

use ruby_prism::Node;

use crate::dialect::Test;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::VarId;
use crate::span::Span;
use crate::{ClassId, Symbol};

use super::super::expr::ingest_expr;
use super::super::util::{constant_id_str, flatten_statements};
use super::super::{IngestError, IngestResult};
use super::{dsl_test, TestTags};

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
}

impl Value {
    fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
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

/// Block-parameter values in scope while a loop's tests are named.
#[derive(Default, Clone)]
struct Env {
    vars: Vec<(String, Value)>,
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
            .map(|e| if e.as_splat_node().is_some() { None } else { eval(&e, env) })
            .collect();
        return values.map(Value::Array);
    }
    if let Some(h) = node.as_hash_node() {
        let mut entries: Vec<(Value, Value)> = Vec::new();
        for element in h.elements().iter() {
            let assoc = element.as_assoc_node()?;
            let key = eval(&assoc.key(), env)?;
            let value = eval(&assoc.value(), env)?;
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
    if let Some(cond) = node.as_if_node() {
        let branch = if eval(&cond.predicate(), env)?.truthy() {
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
        if call.block().is_some() {
            return None;
        }
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let recv = eval(&receiver, env)?;
        let method = constant_id_str(&call.name());
        return match (method, args.as_slice()) {
            ("inspect", []) => inspect(&recv).map(Value::Str),
            ("to_s", []) => to_s(&recv).map(Value::Str),
            ("freeze", []) => Some(recv),
            ("[]", [key]) => index(&recv, &eval(key, env)?, false),
            ("fetch", [key]) => index(&recv, &eval(key, env)?, true),
            ("join", []) => join(&recv, "").map(Value::Str),
            ("join", [sep]) => match eval(sep, env)? {
                Value::Str(sep) => join(&recv, &sep).map(Value::Str),
                Value::Nil => join(&recv, "").map(Value::Str),
                _ => None,
            },
            ("first", []) => match recv {
                Value::Array(items) => Some(items.into_iter().next().unwrap_or(Value::Nil)),
                _ => None,
            },
            ("last", []) => match recv {
                Value::Array(items) => Some(items.into_iter().last().unwrap_or(Value::Nil)),
                _ => None,
            },
            ("size" | "length" | "count", []) => match recv {
                Value::Array(items) => Some(Value::Int(items.len() as i128)),
                Value::Hash(entries) => Some(Value::Int(entries.len() as i128)),
                Value::Str(s) => Some(Value::Int(s.chars().count() as i128)),
                _ => None,
            },
            _ => None,
        };
    }
    None
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

/// `node` as a loop element: its value, and its own expression.
fn elem_of(node: &Node<'_>, file: &str) -> IngestResult<Option<Elem>> {
    let Some(value) = eval(node, &Env::default()) else { return Ok(None) };
    let parts = match node.as_array_node() {
        Some(array) => {
            let mut parts = Vec::new();
            for element in array.elements().iter() {
                let Some(part) = elem_of(&element, file)? else { return Ok(None) };
                parts.push(part);
            }
            Some(parts)
        }
        None => None,
    };
    Ok(Some(Elem { value, expr: ingest_expr(node, file)?, parts }))
}

/// A collection a class-body loop iterates, known before the code runs.
enum Collection {
    Array(Vec<Elem>),
    Hash(Vec<(Elem, Elem)>),
}

/// The literal `receiver` iterates: an Array, Hash or integer Range
/// literal (`.freeze`d or not), or a constant the class body set to one
/// before `before`. `None` for anything else.
fn collection(
    receiver: &Node<'_>,
    class_body: &[Node<'_>],
    before: usize,
    file: &str,
) -> IngestResult<Option<Collection>> {
    if let Some(call) = receiver.as_call_node() {
        let freeze = constant_id_str(&call.name()) == "freeze" && call.arguments().is_none() && call.block().is_none();
        return match call.receiver() {
            Some(inner) if freeze => collection(&inner, class_body, before, file),
            _ => Ok(None),
        };
    }
    if let Some(array) = receiver.as_array_node() {
        let mut elems = Vec::new();
        for element in array.elements().iter() {
            let Some(elem) = elem_of(&element, file)? else { return Ok(None) };
            elems.push(elem);
        }
        return Ok(Some(Collection::Array(elems)));
    }
    if let Some(hash) = receiver.as_hash_node() {
        let mut entries: Vec<(Elem, Elem)> = Vec::new();
        for element in hash.elements().iter() {
            let Some(assoc) = element.as_assoc_node() else { return Ok(None) };
            let (Some(key), Some(value)) = (elem_of(&assoc.key(), file)?, elem_of(&assoc.value(), file)?) else {
                return Ok(None);
            };
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
        return collection(only, class_body, before, file);
    }
    if let Some(read) = receiver.as_constant_read_node() {
        let name = read.name();
        let written = class_body
            .iter()
            .filter(|stmt| stmt.location().start_offset() < before)
            .filter_map(|stmt| stmt.as_constant_write_node())
            .filter(|write| write.name().as_slice() == name.as_slice())
            .last();
        return match written {
            Some(write) => collection(&write.value(), class_body, write.location().start_offset(), file),
            None => Ok(None),
        };
    }
    Ok(None)
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

/// What one class-body statement that declares tests becomes.
struct Unroll<'a, 'pr> {
    class_body: &'a [Node<'pr>],
    file: &'a str,
    tags: &'a TestTags,
}

impl Unroll<'_, '_> {
    /// The tests a static loop declares, or why it is not modeled.
    /// `bound` holds the enclosing loops' parameters, outer first.
    fn unroll_loop(
        &self,
        call: &ruby_prism::CallNode<'_>,
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
                let Some(found) = collection(&receiver, self.class_body, before, self.file)? else {
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
                let Some(test_call) = stmt.as_call_node().filter(|c| is_test_call(c)) else {
                    return Ok(Err(not_modeled("a loop whose block does more than declare tests", first_line(stmt))));
                };
                let env = Env { vars: scope.iter().map(|(n, e)| (n.clone(), e.value.clone())).collect() };
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
                match dsl_test(&test_call, name, prelude, self.file, self.tags)? {
                    Ok(test) => out.push(test),
                    Err(why) => return Ok(Err(not_modeled(format!("a loop whose `test` {}", why.0), why.1))),
                }
            }
        }
        Ok(Ok(()))
    }
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
/// first one) and that ingest did not take as one literal test: unroll
/// the tests of a static loop into `tests`, or refuse just this
/// statement by kind and line. Under `--survey` the refusal is recorded
/// and the class's other tests still run; strict ingest fails with it.
pub(super) fn declare_hidden(
    stmt: &Node<'_>,
    offset: usize,
    class_node: &ruby_prism::ClassNode<'_>,
    class: &ClassId,
    file: &str,
    tags: &TestTags,
    tests: &mut Vec<Test>,
) -> IngestResult<()> {
    let class_body: Vec<Node<'_>> = class_node.body().map(flatten_statements).unwrap_or_default();
    let unroll = Unroll { class_body: &class_body, file, tags };
    let outcome: Result<Vec<Test>, NotModeled> = if let Some(call) = stmt.as_call_node().filter(|c| is_test_call(c)) {
        // A `test` ingest_test_declaration did not take: a computed name,
        // or tags or a block it does not model.
        let name_node = call.arguments().and_then(|a| a.arguments().iter().next()).expect("test args");
        match static_string(&name_node) {
            None => Err(not_modeled("a `test` whose name is computed", first_line(&name_node))),
            Some(name) => match dsl_test(&call, name, Vec::new(), file, tags)? {
                Ok(test) => Ok(vec![test]),
                Err(why) => Err(not_modeled(format!("a `test` whose {}", why.0), why.1)),
            },
        }
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
            return Ok(());
        } else {
            Err(not_modeled("a `class << self` method that declares tests when called", macros.join(", ")))
        }
    } else if stmt.as_if_node().is_some() || stmt.as_unless_node().is_some() || stmt.as_case_node().is_some() {
        Err(not_modeled("a conditional", first_line(stmt)))
    } else {
        Err(not_modeled("a class-body statement ingest does not model", first_line(stmt)))
    };
    match outcome {
        Ok(declared) => {
            for test in declared {
                push_declared(tests, test, class, file)?;
            }
            Ok(())
        }
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
            })
        }
    }
}

/// Add a DSL-declared test: ActiveSupport::Testing::Declarative#test
/// raises while the class body loads when the method already exists.
fn push_declared(tests: &mut Vec<Test>, test: Test, class: &ClassId, file: &str) -> IngestResult<()> {
    let collides = tests.iter().any(|t| !t.toggle_flags && !test.toggle_flags && t.method_name == test.method_name);
    if collides {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "{} is already defined in {}: ActiveSupport raises this when the class body loads",
                test.method_name, class.0
            ),
        });
    }
    tests.push(test);
    Ok(())
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
