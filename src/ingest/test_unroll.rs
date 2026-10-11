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
//! - a loop over an app constant's collection (`consts`), looked up as
//!   Ruby does (lexical scopes, the class's ancestors, the top level),
//!   only when its value is a literal of frozen values and every source
//!   uses it only in the few ways known to leave it as written (an
//!   allow-list: the loop itself, `keys`, `size`, `join`, `include?`, a
//!   block that only reads, and for frozen elements `first`/`[i]`). Any
//!   other value or use, a constant an ancestor without a registered
//!   source may declare, or a `Module#constants` loop whose order matters
//!   (anything but `each`), is a named gap.
//! - what no constant's name tells, across every registered source and
//!   the test sources not registered yet (`Wide`): a `constants` or
//!   `const_get` a module may answer with that its body does not show is
//!   a named gap for every `Module#constants` loop. That is a definition
//!   (`def`, `alias`, `define_method`/`alias_method`/`delegate`/a stub of
//!   a literal name, a name in an Array) on `Module`, `Class` or a module
//!   `EXTENDS` lists, in a block or method whose `self` is not known, on
//!   a receiver not known, or in a module that may be mixed into one not
//!   written there (`base.extend(Patch)`); `class Module` reopened; a
//!   visibility change of one. A module's own singleton (`def
//!   self.constants`) is its own loop's gap; a Symbol code only returns
//!   or passes on (`def category = :constants`) defines nothing. A
//!   constant or module looked up by a name not known before the code
//!   runs (`const_get(name)`, `name.constantize`,
//!   `names.map(&:constantize)`) and changed there is a named gap for
//!   every loop that change may reach: with a literal head
//!   (`"GraphApi::#{x}"`, or a same-class method whose every return is
//!   one) only a constant under that head, an ancestor of a module there,
//!   or one passed on or aliased (`Origin`, `reaches`). A literal String's
//!   `constantize` is a use of the constant it names. What is done to a
//!   value found by a computed name is followed through calls on it (and
//!   `each`/`tap`, which return it), `||`, `&&`, a branch's value, and a
//!   local assigned in the same scope; a value passed on, returned, given
//!   to `case` or stored in an instance variable is not followed, a
//!   computed module's `const_set` of a computed name counts as a change
//!   of that module (not of every constant it may bind again), and a stub
//!   in a test's own code (`stubs`, `expects`) is not counted. Not
//!   followed either: a method defined by a name not literal
//!   (`names.each { |n| define_method(n) }`), and a module reached as
//!   what a call returns or yields (`Mod.module_parent`, a method of its
//!   own that returns or yields `self`).
//!
//! Native tests of one iteration share its closure; each emitted test
//! sets its own copy (`x = <element>`). An element read from an app
//! constant is set to that constant's own object (`CONST[i]`); one built
//! by the loop is rebuilt in each test. So a loop whose tests change an
//! object they share, or whose body assigns a bound variable again, is a
//! named gap rather than a run that differs.

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
    /// An Array whose elements are known but whose order is not:
    /// `Module#constants`, which lists names in the order of the
    /// process's symbol table. Only what does not depend on the order
    /// is computed from it (`each`, `-`, `size`); anything else (`first`,
    /// `join`, `each_with_index`, a name) is not known.
    Unordered(Vec<Value>),
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
            Value::Array(items) | Value::Unordered(items) => items.iter().any(Value::has_opaque),
            Value::Hash(entries) => entries.iter().any(|(k, v)| k.has_opaque() || v.has_opaque()),
            _ => false,
        }
    }

    /// Does any part of it have an order that is not known?
    fn has_unordered(&self) -> bool {
        match self {
            Value::Unordered(_) => true,
            Value::Array(items) => items.iter().any(Value::has_unordered),
            Value::Hash(entries) => entries.iter().any(|(k, v)| k.has_unordered() || v.has_unordered()),
            _ => false,
        }
    }

    /// A value a literal spells (`nil`, a number, String, Symbol, or an
    /// Array or Hash of them): one a test body can rebuild by itself.
    fn is_literal(&self) -> bool {
        match self {
            Value::Opaque | Value::Namespace(_) | Value::Unordered(_) => false,
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
            let key = eval(&assoc.key(), env).filter(|k| !k.has_opaque() && !k.has_unordered())?;
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
                ("map" | "collect", [], Value::Unordered(items)) => items
                    .iter()
                    .map(|item| send0(item, &sym))
                    .collect::<Option<Vec<_>>>()
                    .map(Value::Unordered),
                _ => None,
            };
        }
        if let Value::Namespace(module) = &recv {
            return match (method, args.as_slice()) {
                ("constants", []) => consts::module_constants(module).map(Value::Unordered),
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
                (Value::Unordered(mut a), Value::Array(b) | Value::Unordered(b))
                | (Value::Array(mut a), Value::Unordered(b)) => {
                    a.extend(b);
                    Some(Value::Unordered(a))
                }
                (Value::Str(a), Value::Str(b)) => Some(Value::Str(a + &b)),
                (Value::Int(a), Value::Int(b)) => a.checked_add(b).map(Value::Int),
                _ => None,
            },
            ("-", [_]) => match (recv, arg(0)?) {
                (Value::Array(a), Value::Array(b) | Value::Unordered(b)) => difference(a, &b).map(Value::Array),
                (Value::Unordered(a), Value::Array(b) | Value::Unordered(b)) => difference(a, &b).map(Value::Unordered),
                (Value::Int(a), Value::Int(b)) => a.checked_sub(b).map(Value::Int),
                _ => None,
            },
            // ActiveSupport's `Array#excluding(*elements)` (and its alias
            // `without`): `self - elements.flatten(1)`, so an Array
            // argument's elements are what is removed.
            ("excluding" | "without", _) => {
                let mut removed = Vec::new();
                for at in 0..args.len() {
                    match arg(at)? {
                        Value::Array(items) | Value::Unordered(items) => removed.extend(items),
                        other => removed.push(other),
                    }
                }
                match recv {
                    Value::Array(a) => difference(a, &removed).map(Value::Array),
                    Value::Unordered(a) => difference(a, &removed).map(Value::Unordered),
                    _ => None,
                }
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
        ("size" | "length" | "count", Value::Array(items) | Value::Unordered(items)) => Some(Value::Int(items.len() as i128)),
        ("size" | "length" | "count", Value::Hash(entries)) => Some(Value::Int(entries.len() as i128)),
        ("size" | "length", Value::Str(s)) => Some(Value::Int(s.chars().count() as i128)),
        ("keys", Value::Hash(entries)) => Some(Value::Array(entries.iter().map(|(k, _)| k.clone()).collect())),
        ("values", Value::Hash(entries)) => Some(Value::Array(entries.iter().map(|(_, v)| v.clone()).collect())),
        ("to_a", Value::Array(items)) => Some(Value::Array(items.clone())),
        ("to_a", Value::Unordered(items)) => Some(Value::Unordered(items.clone())),
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
        (Value::Unordered(_), _) | (_, Value::Unordered(_)) => None,
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
    if a.iter().chain(b).any(|v| v.has_opaque() || v.has_unordered() || matches!(v, Value::Hash(_))) {
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
        Value::Namespace(_) | Value::Opaque | Value::Unordered(_) => return None,
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
    /// `expr` gives the one object the loop yielded (it reads it again
    /// from the constant, or the value is an immediate: `nil`, a number,
    /// a Symbol), not a copy that a test changing it would not share.
    shared: bool,
}

/// `nil`, `true`/`false`, a number or a Symbol: the same object however
/// it is spelled.
fn immediate(value: &Value) -> bool {
    matches!(value, Value::Nil | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Sym(_))
}

impl Elem {
    fn nil() -> Self {
        Elem { value: Value::Nil, expr: lit(Literal::Nil), parts: None, shared: true }
    }

    fn int(i: i128) -> Self {
        let value = match i64::try_from(i) {
            Ok(v) => Literal::Int { value: v },
            Err(_) => Literal::BigInt { value: i.to_string() },
        };
        Elem { value: Value::Int(i), expr: lit(value), parts: None, shared: true }
    }

    fn pair(key: Elem, value: Elem) -> Self {
        Elem {
            value: Value::Array(vec![key.value.clone(), value.value.clone()]),
            expr: Expr::new(
                Span::synthetic(),
                ExprNode::Array { elements: vec![key.expr.clone(), value.expr.clone()], style: Default::default() },
            ),
            parts: Some(vec![key, value]),
            shared: false,
        }
    }

    /// Ruby's multiple assignment of this value to `n` targets: an
    /// Array's elements (padded with `nil`), anything else first.
    fn splat(&self, n: usize) -> Option<Vec<Elem>> {
        let mut out: Vec<Elem> = match (&self.value, &self.parts) {
            (Value::Array(_), Some(parts)) => parts.iter().take(n).cloned().collect(),
            (Value::Array(_) | Value::Unordered(_), _) => return None,
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
    let shared = immediate(&value);
    Ok(Some(Elem { value, expr: ingest_expr(node, file)?, parts, shared }))
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
        Value::Namespace(_) | Value::Opaque | Value::Unordered(_) => return None,
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

/// An element of a collection computed from constants. When `same` (the
/// collection is a constant's own object, `recv` reads it again), a value
/// that is not an immediate is read again from it (`recv[i]`): the one
/// object the loop yielded, with its identity and frozen state. Otherwise
/// it is rebuilt from its literal value where it has one (the loop
/// yielded an object made when it ran), else read again.
fn computed_elem(value: Value, same: bool, at_recv: &dyn Fn() -> Expr) -> Elem {
    let parts = match &value {
        Value::Array(items) => Some(
            items
                .iter()
                .enumerate()
                .map(|(j, item)| computed_elem(item.clone(), same, &|| send(at_recv(), "[]", vec![Elem::int(j as i128).expr])))
                .collect(),
        ),
        _ => None,
    };
    let expr = match value_expr(&value) {
        Some(expr) if immediate(&value) || !same => expr,
        _ => at_recv(),
    };
    let shared = same || immediate(&value);
    Elem { value, expr, parts, shared }
}

/// A collection a class-body loop iterates, known before the code runs.
enum Collection {
    Array(Vec<Elem>),
    Hash(Vec<(Elem, Elem)>),
    /// `Module#constants`: its elements, in no order known here.
    Unordered(Vec<Elem>),
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
            // Not a literal of frozen values, or used where it may change
            // (`CASES << :b`): its literal may not be what the loop iterates.
            if !consts::local_unchanged(env, file, constant_id_str(&name), &write.value(), at) {
                return Ok(None);
            }
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
    // A constant (`.freeze`d or not) is the one object the loop iterated;
    // anything computed from it is made again where a test evaluates it.
    let same = constant_object(receiver);
    Ok(match value {
        Value::Array(items) => Some(Collection::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| computed_elem(item, same, &|| send(recv.clone(), "[]", vec![Elem::int(i as i128).expr])))
                .collect(),
        )),
        Value::Hash(entries) => Some(Collection::Hash(
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (k, v))| {
                    let at = || vec![Elem::int(i as i128).expr];
                    (
                        computed_elem(k, same, &|| send(send(recv.clone(), "keys", vec![]), "[]", at())),
                        computed_elem(v, same, &|| send(send(recv.clone(), "values", vec![]), "[]", at())),
                    )
                })
                .collect(),
        )),
        // Not read again by index: the order a test would see is not the
        // one the loop had. Each element is rebuilt from its literal.
        Value::Unordered(items) => {
            let mut elems = Vec::with_capacity(items.len());
            for item in items {
                let Some(expr) = value_expr(&item) else { return Ok(None) };
                let shared = immediate(&item);
                elems.push(Elem { value: item, expr, parts: None, shared });
            }
            Some(Collection::Unordered(elems))
        }
        _ => None,
    })
}

/// Is `node` a constant (`A::B`, or `A::B.freeze`, which returns it)?
fn constant_object(node: &Node<'_>) -> bool {
    if node.as_constant_read_node().is_some() || node.as_constant_path_node().is_some() {
        return true;
    }
    if let Some(call) = node.as_call_node() {
        return call.name().as_slice() == b"freeze"
            && call.arguments().is_none()
            && call.block().is_none()
            && call.receiver().is_some_and(|r| constant_object(&r));
    }
    node.as_parentheses_node()
        .and_then(|p| p.body())
        .map(flatten_statements)
        .is_some_and(|stmts| matches!(stmts.as_slice(), [only] if constant_object(only)))
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

impl Param {
    /// The names it binds.
    fn names(&self, out: &mut Vec<String>) {
        match self {
            Param::Name(n) => out.push(n.clone()),
            Param::Multi(inner) => inner.iter().for_each(|p| p.names(out)),
        }
    }
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

    /// The parameters whose each test binds its own copy of the argument
    /// (not the one object the call passed, nor an immediate).
    pub(super) fn unshared(&self) -> Vec<String> {
        self.0.iter().filter(|(_, e)| !e.shared).map(|(n, _)| n.clone()).collect()
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
                consts::clear_refusal();
                let Some(found) = collection(&receiver, self.class_body, before, self.file, &env)? else {
                    return Ok(Err(refused("a loop over a collection that is not a literal", first_line(&receiver))));
                };
                let items: Vec<Elem> = match found {
                    Collection::Array(_) if method == "each_pair" => {
                        return Ok(Err(not_modeled("a loop over a collection that is not a literal", first_line(&receiver))));
                    }
                    Collection::Array(elems) => elems,
                    Collection::Hash(entries) => entries.into_iter().map(|(k, v)| Elem::pair(k, v)).collect(),
                    // Ruby lists `Module#constants` in the order of its
                    // symbol table: only a loop whose tests do not depend
                    // on the order (`each`: the same set of tests in any
                    // order) is modeled.
                    Collection::Unordered(elems) if method == "each" => elems,
                    Collection::Unordered(_) => {
                        return Ok(Err(not_modeled(
                            "a loop whose order Ruby does not fix (`Module#constants`)",
                            first_line(&call.as_node()),
                        )));
                    }
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
        // A test's closure also sees the class body's locals (`prefix =
        // "x"` before the loop); the tests bind only the loop's names.
        if let Some((name, at)) = outer_local(&block, bound) {
            let line = super::super::sources::line_at(self.file, at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
            return Ok(Err(not_modeled("a loop whose block reads a local from outside it", format!("`{name}` at line {line}"))));
        }
        let stmts = block.body().map(flatten_statements).unwrap_or_default();
        // The names each test's closure shares with the loop: the block
        // parameters and the locals its body binds (first binding only).
        let mut names: Vec<String> = Vec::new();
        params.iter().for_each(|p| p.names(&mut names));
        let mut bindings: Vec<usize> = Vec::new();
        for stmt in &stmts {
            if let Some(write) = stmt.as_local_variable_write_node() {
                let name = constant_id_str(&write.name()).to_string();
                if !names.contains(&name) {
                    names.push(name);
                    bindings.push(stmt.location().start_offset());
                }
            }
        }
        // Each test binds its own copy of them (its prelude); Ruby's
        // closures share one variable per iteration, so a test that
        // assigns one, or the loop assigning it again after a test, is
        // not what the copies see.
        let changes = closure_changes(&block.as_node(), &names, &bindings);
        if let Some((name, at)) = changes.written.first() {
            return Ok(Err(not_modeled(
                "a loop whose tests share a variable that is assigned again",
                format!("`{name}`: `{at}`"),
            )));
        }
        for yielded in iterations {
            let mut scope = bound.to_vec();
            if bind(&params, &yielded, &mut scope).is_none() {
                return Ok(Err(not_modeled("a loop whose block parameters are not modeled", first_line(&call.as_node()))));
            }
            for stmt in &stmts {
                if let Some(inner) = stmt.as_call_node().filter(|c| c.receiver().is_some() && c.block().is_some()) {
                    if let Some(why) = shared_change(&changes, &scope) {
                        return Ok(Err(why));
                    }
                    if let Err(why) = self.unroll_loop(&inner, &scope, out)? {
                        return Ok(Err(why));
                    }
                    continue;
                }
                // `client_name = …` beside the tests: a value each test's
                // closure reads, bound like a block parameter.
                if let Some(write) = stmt.as_local_variable_write_node() {
                    let env = Env { vars: env_of(&scope), cx: self.cx.clone(), mode: Mode::Exact };
                    consts::clear_refusal();
                    let Some(elem) = elem_of(&write.value(), self.file, &env)? else {
                        return Ok(Err(refused("a loop whose block does more than declare tests", first_line(stmt))));
                    };
                    scope.push((constant_id_str(&write.name()).to_string(), elem));
                    continue;
                }
                let Some(test_call) = stmt.as_call_node().filter(|c| is_test_call(c)) else {
                    return Ok(Err(not_modeled("a loop whose block does more than declare tests", first_line(stmt))));
                };
                if let Some(why) = shared_change(&changes, &scope) {
                    return Ok(Err(why));
                }
                let env = Env { vars: env_of(&scope), cx: self.cx.clone(), mode: Mode::Exact };
                let name_node = test_call.arguments().and_then(|a| a.arguments().iter().next()).expect("test args");
                consts::clear_refusal();
                let name = match eval(&name_node, &env) {
                    Some(Value::Str(name)) => name,
                    _ => return Ok(Err(refused("a loop whose test names are not known before it runs", first_line(&name_node)))),
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

/// The first local `block` (a loop's block, inside the loops `bound`
/// binds) reads or writes that lives outside it and those loops (a
/// class-body local), with where it does.
fn outer_local(block: &ruby_prism::BlockNode<'_>, bound: &[(String, Elem)]) -> Option<(String, usize)> {
    use ruby_prism::Visit;
    struct Scan<'a> {
        /// Blocks entered inside `block`.
        level: u32,
        bound: &'a [(String, Elem)],
        found: Option<(String, usize)>,
    }
    impl Scan<'_> {
        fn local(&mut self, name: &ruby_prism::ConstantId<'_>, depth: u32, node: &Node<'_>) {
            let name = constant_id_str(name);
            if self.found.is_none() && depth > self.level && !self.bound.iter().any(|(n, _)| n == name) {
                self.found = Some((name.to_string(), node.location().start_offset()));
            }
        }
    }
    impl<'pr> Visit<'pr> for Scan<'_> {
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.level += 1;
            ruby_prism::visit_block_node(self, node);
            self.level -= 1;
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.level += 1;
            ruby_prism::visit_lambda_node(self, node);
            self.level -= 1;
        }
        // Their own scopes: an outer local is not seen there.
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}
        fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
        fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
        fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
        fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
            self.local(&node.name(), node.depth(), &node.as_node());
        }
        fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
            self.local(&node.name(), node.depth(), &node.as_node());
            ruby_prism::visit_local_variable_write_node(self, node);
        }
        fn visit_local_variable_target_node(&mut self, node: &ruby_prism::LocalVariableTargetNode<'pr>) {
            self.local(&node.name(), node.depth(), &node.as_node());
        }
        fn visit_local_variable_operator_write_node(&mut self, node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>) {
            self.local(&node.name(), node.depth(), &node.as_node());
            ruby_prism::visit_local_variable_operator_write_node(self, node);
        }
        fn visit_local_variable_or_write_node(&mut self, node: &ruby_prism::LocalVariableOrWriteNode<'pr>) {
            self.local(&node.name(), node.depth(), &node.as_node());
            ruby_prism::visit_local_variable_or_write_node(self, node);
        }
        fn visit_local_variable_and_write_node(&mut self, node: &ruby_prism::LocalVariableAndWriteNode<'pr>) {
            self.local(&node.name(), node.depth(), &node.as_node());
            ruby_prism::visit_local_variable_and_write_node(self, node);
        }
    }
    let mut scan = Scan { level: 0, bound, found: None };
    if let Some(params) = block.parameters() {
        scan.visit(&params);
    }
    if let Some(body) = block.body() {
        scan.visit(&body);
    }
    scan.found
}

/// A test that changes a shared object in place (`k << x`): the copies
/// each test binds are the one object only when each reads it again from
/// where the loop got it, or it is an immediate. Checked at each test (or
/// inner loop), once the body's locals before it are bound.
fn shared_change(changes: &Changes, scope: &[(String, Elem)]) -> Option<NotModeled> {
    changes.mutated.iter().find_map(|(name, at)| {
        scope
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .is_some_and(|(_, elem)| !elem.shared)
            .then(|| not_modeled("a loop whose tests change an object they share", format!("`{name}`: `{at}`")))
    })
}

/// Why a value was not known: the reason a constant lookup refused it
/// (`consts::take_refusal`), else `kind`.
fn refused(kind: &str, detail: String) -> NotModeled {
    match consts::take_refusal() {
        Some((why, what)) => not_modeled(why, what),
        None => not_modeled(kind, detail),
    }
}

/// What a block does to names it shares with tests' closures, after
/// binding them.
#[derive(Default)]
struct Changes {
    /// Assigned again anywhere (a test body included), other than at
    /// its binding (`bindings`): (name, the assignment's first line).
    written: Vec<(String, String)>,
    /// Its object changed in place (`k << x`, `k[0] = x`, `k.a = x`,
    /// `k.upcase!`, `k.first.concat(x)`, `k.freeze`).
    mutated: Vec<(String, String)>,
}

/// The changes `node` makes to `names` (`bindings`: the starts of the
/// statements that bind them). An inner block's parameter of the same
/// name is not told apart: it counts too.
fn closure_changes(node: &Node<'_>, names: &[String], bindings: &[usize]) -> Changes {
    use ruby_prism::Visit;
    struct Scan<'a> {
        names: &'a [String],
        bindings: &'a [usize],
        out: Changes,
    }
    impl Scan<'_> {
        fn bound(&self, name: &str) -> bool {
            self.names.iter().any(|n| n == name)
        }
        fn write(&mut self, name: &str, node: &Node<'_>) {
            if self.bound(name) {
                self.out.written.push((name.to_string(), first_line(node)));
            }
        }
        fn mutate(&mut self, receiver: Option<Node<'_>>, node: &Node<'_>) {
            if let Some(name) = receiver.as_ref().and_then(root_local) {
                if self.bound(&name) {
                    self.out.mutated.push((name, first_line(node)));
                }
            }
        }
    }
    impl<'pr> Visit<'pr> for Scan<'_> {
        fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
            if !self.bindings.contains(&node.location().start_offset()) {
                self.write(constant_id_str(&node.name()), &node.as_node());
            }
            ruby_prism::visit_local_variable_write_node(self, node);
        }
        fn visit_local_variable_operator_write_node(&mut self, node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>) {
            self.write(constant_id_str(&node.name()), &node.as_node());
            ruby_prism::visit_local_variable_operator_write_node(self, node);
        }
        fn visit_local_variable_or_write_node(&mut self, node: &ruby_prism::LocalVariableOrWriteNode<'pr>) {
            self.write(constant_id_str(&node.name()), &node.as_node());
            ruby_prism::visit_local_variable_or_write_node(self, node);
        }
        fn visit_local_variable_and_write_node(&mut self, node: &ruby_prism::LocalVariableAndWriteNode<'pr>) {
            self.write(constant_id_str(&node.name()), &node.as_node());
            ruby_prism::visit_local_variable_and_write_node(self, node);
        }
        fn visit_local_variable_target_node(&mut self, node: &ruby_prism::LocalVariableTargetNode<'pr>) {
            self.write(constant_id_str(&node.name()), &node.as_node());
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if mutates(constant_id_str(&node.name())) {
                self.mutate(node.receiver(), &node.as_node());
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_index_operator_write_node(&mut self, node: &ruby_prism::IndexOperatorWriteNode<'pr>) {
            self.mutate(node.receiver(), &node.as_node());
            ruby_prism::visit_index_operator_write_node(self, node);
        }
        fn visit_index_or_write_node(&mut self, node: &ruby_prism::IndexOrWriteNode<'pr>) {
            self.mutate(node.receiver(), &node.as_node());
            ruby_prism::visit_index_or_write_node(self, node);
        }
        fn visit_index_and_write_node(&mut self, node: &ruby_prism::IndexAndWriteNode<'pr>) {
            self.mutate(node.receiver(), &node.as_node());
            ruby_prism::visit_index_and_write_node(self, node);
        }
        fn visit_call_operator_write_node(&mut self, node: &ruby_prism::CallOperatorWriteNode<'pr>) {
            self.mutate(node.receiver(), &node.as_node());
            ruby_prism::visit_call_operator_write_node(self, node);
        }
        fn visit_call_or_write_node(&mut self, node: &ruby_prism::CallOrWriteNode<'pr>) {
            self.mutate(node.receiver(), &node.as_node());
            ruby_prism::visit_call_or_write_node(self, node);
        }
        fn visit_call_and_write_node(&mut self, node: &ruby_prism::CallAndWriteNode<'pr>) {
            self.mutate(node.receiver(), &node.as_node());
            ruby_prism::visit_call_and_write_node(self, node);
        }
    }
    let mut scan = Scan { names, bindings, out: Changes::default() };
    scan.visit(node);
    scan.out
}

/// The local a chain of calls starts from (`k` of `k.first[0]`).
fn root_local(node: &Node<'_>) -> Option<String> {
    if let Some(read) = node.as_local_variable_read_node() {
        return Some(constant_id_str(&read.name()).to_string());
    }
    if let Some(call) = node.as_call_node() {
        return root_local(&call.receiver()?);
    }
    let parens = node.as_parentheses_node()?;
    let stmts = parens.body().map(flatten_statements).unwrap_or_default();
    match stmts.as_slice() {
        [only] => root_local(only),
        _ => None,
    }
}

/// The first name of `names` whose object `node` changes in place, with
/// the change's first line.
pub(super) fn changed_in_place(node: &Node<'_>, names: &[String]) -> Option<(String, String)> {
    closure_changes(node, names, &[]).mutated.into_iter().next()
}

/// Does a call of `name` change its receiver in place (`<<`, `[]=`, a
/// setter, a `!` method, `freeze`, `concat`, `send`)?
pub(super) fn mutates(name: &str) -> bool {
    /// Methods that change their receiver (Array, Hash, String, Set), or
    /// reach code that may.
    const MUTATORS: &[&str] = &[
        "push", "append", "prepend", "unshift", "insert", "concat", "replace", "clear", "delete", "delete_at",
        "delete_if", "keep_if", "fill", "pop", "shift", "store", "update", "add", "subtract", "reset",
        "compare_by_identity", "rehash", "force_encoding", "setbyte", "instance_variable_set", "send", "public_send",
        "__send__", "instance_eval", "instance_exec", "class_eval", "module_eval", "class_exec", "module_exec",
        "define_method", "define_singleton_method", "extend", "include", "const_set", "remove_const", "singleton_class",
    ];
    match name {
        "<<" | "[]=" | "freeze" => true,
        "!" | "!=" | "==" | "===" | "<=" | ">=" | "=~" | "!~" => false,
        _ => {
            name.ends_with('!')
                || name
                    .strip_suffix('=')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                || MUTATORS.contains(&name)
        }
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

/// What a class-body statement that declares tests gives the test class.
pub(super) enum Hidden {
    /// The tests of a static loop, in the order it declares them.
    Tests(Vec<Test>),
    /// Tests that cannot be listed before the statement runs, with the
    /// refusal ingest would name for them: Ruby declares them as the
    /// class loads, so the caller carries the statement to run there
    /// (`TestModule::tests_at_load`), and names this when it cannot.
    AtLoad(IngestError),
}

/// `stmt`, a class-body statement that declares tests (`offset` is the
/// first one) and that ingest did not take as one literal test: the
/// tests of a static loop, in the order it declares them (the caller
/// adds each, `super::declare_test`), or the refusal by kind and line
/// for the statement alone (`Hidden::AtLoad`), which the caller names
/// when it cannot carry the statement to run as the class loads.
pub(super) fn declare_hidden<'pr>(
    stmt: &Node<'pr>,
    offset: usize,
    class_node: &ruby_prism::ClassNode<'pr>,
    class: &ClassId,
    file: &str,
    tags: &TestTags,
    consts: &TagConstants<'pr>,
) -> IngestResult<Hidden> {
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
    } else if stmt.as_call_node().is_some_and(|c| super::defines_test_method(&c)) {
        Err(not_modeled("`define_method` of a test", first_line(stmt)))
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
            return Ok(Hidden::Tests(Vec::new()));
        } else {
            Err(not_modeled("a `class << self` method that declares tests when called", macros.join(", ")))
        }
    } else if stmt.as_if_node().is_some() || stmt.as_unless_node().is_some() || stmt.as_case_node().is_some() {
        Err(not_modeled("a conditional", first_line(stmt)))
    } else {
        Err(not_modeled("a class-body statement ingest does not model", first_line(stmt)))
    };
    match outcome {
        Ok(declared) => Ok(Hidden::Tests(declared)),
        Err(why) => {
            let line = |at: usize| super::super::sources::line_at(file, at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
            let stmt_line = line(stmt.location().start_offset());
            let test_line = line(offset);
            let at = if stmt_line == test_line { format!("line {stmt_line}") } else { format!("line {stmt_line}, first test at line {test_line}") };
            Ok(Hidden::AtLoad(IngestError::Unsupported {
                file: file.into(),
                message: format!(
                    "tests declared by {} are not modeled; they will not run (`{}` {at}: {})",
                    why.kind, class.0, why.detail
                ),
            }))
        }
    }
}

/// Constants a loop reads: those the test class body writes above it,
/// and those another registered source (an app model, a test helper)
/// declares, looked up by Ruby's rules for a written constant.
///
/// `A::B::C` resolves `A` through the lexical scopes, innermost first,
/// then through the ancestors of the innermost scope (`ancestors`: its
/// superclasses and the modules each includes or prepends), then at the
/// top level; each later name must be declared directly in the one
/// before it (one inherited from an ancestor is not looked for). A name
/// an ancestor may declare, or one looked up past an ancestor that is
/// not known (not registered, changed from outside its body), is not
/// known. A name is known when exactly one statement declares it, run
/// unconditionally (not in a block, a conditional or a `class << self`);
/// a `module`/`class` reopened anywhere stays a namespace. A constant
/// made `private_constant` is refused where Ruby raises (`A::PRIVATE`).
///
/// A constant's value is known only when it is a literal of frozen
/// values (`shape`: Symbols, numbers, frozen Strings, `nil`/`true`/
/// `false`, modules, and Arrays and Hashes of these) and every mention
/// in every source, test bodies included, is a use its shape allows
/// (`scan_uses`, `Shape::allows`): mutation is not analyzed, any use not
/// on the list is refused by name. A frozen value may be used in any way
/// but `const_set`/`remove_const`; an unfrozen Array or Hash of frozen
/// values may give out its elements (`first`, `[i]`, a block that does
/// more than read them); one with an unfrozen part may only be read
/// (`size`, `join`, `include?`, a block that only reads, the loop that
/// declares tests).
///
/// The lookup covers the sources registered when the loop is ingested:
/// app models and libraries, and test files that sort before the loop's.
/// Each refusal says why (`take_refusal`), for the gap that names it.
pub(super) mod consts {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    use ruby_prism::Node;

    use super::super::super::sources;
    use super::super::super::util::{constant_id_str, flatten_statements};
    use super::{eval, Env, Mode, Value};
    use crate::span::FileId;

    /// Constants defined by constants, at most this deep.
    const MAX_DEPTH: usize = 8;

    /// Most modules a scope's ancestry may have.
    const MAX_ANCESTORS: usize = 64;

    /// Where an expression is written: its file and lexical scopes (full
    /// names, outer first), and in a test class body, what that body
    /// writes above the loop.
    pub(in super::super) struct ConstCx {
        pub(in super::super) file: String,
        pub(in super::super) cref: Vec<String>,
        pub(in super::super) local: Option<Local>,
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

    #[derive(Clone)]
    enum Found {
        Missing,
        Unknown,
        Namespace,
        Value(Decl),
    }

    enum Hit {
        Namespace(String),
        Decl(Decl),
        /// A test class body constant: (name, statement start, value
        /// start, end).
        Local(String, usize, usize, usize),
    }

    thread_local! {
        /// Why the first constant refused since `clear_refusal` is not
        /// known: (what the gap is, its detail).
        static REFUSAL: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    }

    /// Not known, because of `kind`: the first refusal is the one a gap
    /// names.
    fn refuse<T>(kind: &str, detail: String) -> Option<T> {
        REFUSAL.with(|r| {
            let mut r = r.borrow_mut();
            if r.is_none() {
                *r = Some((kind.to_string(), detail));
            }
        });
        None
    }

    pub(in super::super) fn clear_refusal() {
        REFUSAL.with(|r| *r.borrow_mut() = None);
    }

    /// The first refusal since `clear_refusal`: (what the gap is, its
    /// detail).
    pub(in super::super) fn take_refusal() -> Option<(String, String)> {
        REFUSAL.with(|r| r.borrow_mut().take())
    }

    pub(in super::super) const NOT_LITERAL: &str = "a loop over a constant whose value is not a literal of frozen values";
    pub(in super::super) const USED: &str = "a loop over a constant a source uses in a way not known to leave it unchanged";
    pub(in super::super) const REOPENED: &str = "a loop over the constants of a module changed from outside its body";
    pub(in super::super) const MODULE_BODY: &str =
        "a loop over the constants of a module whose body is not one listed as declaring only the constants it shows";
    pub(in super::super) const ANCESTOR: &str = "a loop over a constant an ancestor of its class may declare";
    pub(in super::super) const UNCERTAIN: &str = "a loop over a constant not assigned once with certainty";
    pub(in super::super) const LATE_SOURCE: &str = "a loop over a constant a test source not registered before it may declare";

    impl ConstCx {
        /// This context for a statement at `at` of the same body.
        pub(in super::super) fn before(&self, at: usize) -> ConstCx {
            ConstCx {
                file: self.file.clone(),
                cref: self.cref.clone(),
                local: self.local.clone().map(|l| Local { before: at, ..l }),
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
            Some(ConstCx {
                file: file.to_string(),
                local: Some(Local { class: cref.last()?.clone(), writes, before }),
                cref,
                depth: 0,
            })
        }
    }

    thread_local! {
        /// `cref_of` answers: (file, its length and fingerprint, start).
        static CREFS: RefCell<HashMap<(String, usize, u64, usize), Option<Vec<String>>>> = RefCell::new(HashMap::new());
    }

    fn fingerprint(text: &str) -> u64 {
        text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
    }

    /// The lexical scopes (full names, outer first) inside the `class`
    /// or `module` that starts at `start` in `file`, itself last.
    pub(in super::super) fn cref_of(file: &str, start: usize) -> Option<Vec<String>> {
        let key = sources::with_text(file, |text| (file.to_string(), text.len(), fingerprint(text), start))?;
        if let Some(found) = CREFS.with(|c| c.borrow().get(&key).cloned()) {
            return found;
        }
        let found = parse_cref(file, start);
        CREFS.with(|c| c.borrow_mut().insert(key, found.clone()));
        found
    }

    fn parse_cref(file: &str, start: usize) -> Option<Vec<String>> {
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
        hit_value(hit(node, cx)?, cx)
    }

    /// What the constant `node` names where `cx` is.
    fn hit(node: &Node<'_>, cx: &ConstCx) -> Option<Hit> {
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
                Found::Unknown => return uncertain(&full),
                _ => return None,
            };
        }
        Some(hit)
    }

    /// What a constant's first name resolves to where it is written.
    #[derive(Debug, PartialEq)]
    pub(in super::super) enum Resolution {
        /// The full name it denotes.
        Declared(String),
        /// No registered source declares it in any scope searched: a
        /// gem's or Ruby's top-level constant, the same from anywhere.
        Missing,
        /// Not known (declared more than once, conditionally, by an
        /// ancestor, …).
        Unknown,
    }

    /// `name` read in a scope whose lexical scopes are `cref`, by its
    /// lexical scopes and the top level only (no class body constants,
    /// no ancestors).
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

    /// `name` read in a block written where the lexical scopes are
    /// `cref`, by Ruby's whole lookup: the lexical scopes, then the
    /// ancestors of the innermost (the one registered ancestor that
    /// declares it; `Unknown` when more may, a framework module may, or
    /// one is not known), then the top level.
    pub(in super::super) fn resolution(cref: &[String], name: &str) -> Resolution {
        for scope in cref.iter().rev() {
            let full = format!("{scope}::{name}");
            match find(&full) {
                Found::Missing => continue,
                Found::Unknown => return Resolution::Unknown,
                Found::Namespace | Found::Value(_) => return Resolution::Declared(full),
            }
        }
        if let Some(inner) = cref.last() {
            let Ok(list) = ancestors(inner) else { return Resolution::Unknown };
            // One ancestor alone (as `lexical`): the listed order is not Ruby's.
            let declaring: Vec<&String> = list.iter().filter(|a| declares(a, name)).collect();
            match declaring.as_slice() {
                [] => {}
                [a] if !known(a) => {
                    let full = format!("{a}::{name}");
                    return match find(&full) {
                        Found::Namespace | Found::Value(_) => Resolution::Declared(full),
                        Found::Missing | Found::Unknown => Resolution::Unknown,
                    };
                }
                _ => return Resolution::Unknown,
            }
        }
        match find(name) {
            Found::Missing => Resolution::Missing,
            Found::Unknown => Resolution::Unknown,
            Found::Namespace | Found::Value(_) => Resolution::Declared(name.to_string()),
        }
    }

    /// Does `name`, read in a block of a method in `class << self` of the
    /// class whose lexical scopes are `cref`, resolve as it does in that
    /// class's own methods? Both look in the lexical scopes first; past
    /// them the singleton class looks in its own ancestors (the modules
    /// the class and its superclasses `extend`, the `ClassMethods` of the
    /// concerns they include, those of the framework's classes) and the
    /// class in its. `Err` says why it may not: an
    /// ancestor of either may declare it, or one is not known.
    pub(in super::super) fn same_in_singleton(cref: &[String], name: &str) -> Result<(), String> {
        for scope in cref.iter().rev() {
            match find(&format!("{scope}::{name}")) {
                Found::Missing => continue,
                Found::Unknown => return Err(format!("`{scope}::{name}` is not declared once with certainty")),
                Found::Namespace | Found::Value(_) => return Ok(()),
            }
        }
        let Some(inner) = cref.last() else { return Ok(()) };
        let class_side = ancestors(inner)?;
        if let Some(a) = class_side.iter().find(|a| declares(a, name)) {
            return Err(format!("`{a}`, an ancestor of `{inner}`, may declare; the singleton class does not look there"));
        }
        if KNOWN_CONSTANTS.contains(&name) {
            return Err("a framework module the singleton class extends may declare".into());
        }
        let mut chain = vec![inner.clone()];
        chain.extend(class_side.into_iter().filter(|a| !known(a)));
        for module in &chain {
            for extended in singleton_adds(module)? {
                let mut list = ancestors(&extended)?;
                list.insert(0, extended);
                if let Some(a) = list.iter().find(|a| declares(a, name)) {
                    return Err(format!("`{a}`, which `{module}` gives the singleton class, may declare"));
                }
            }
        }
        Ok(())
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

    /// Refused: `full` is declared by a test source not registered yet,
    /// or more than once, or not certainly (`X, ALL = …`, `ALL ||= …`,
    /// in a conditional).
    fn uncertain<T>(full: &str) -> Option<T> {
        match late_reason(full) {
            Some(why) => refuse(LATE_SOURCE, why),
            None => refuse(UNCERTAIN, format!("`{full}` is not assigned once, unconditionally, by `{full} = …`")),
        }
    }

    fn top(name: &str) -> Option<Hit> {
        match find(name) {
            Found::Namespace => Some(Hit::Namespace(name.to_string())),
            Found::Value(decl) => Some(Hit::Decl(decl)),
            Found::Unknown => uncertain(name),
            Found::Missing => None,
        }
    }

    /// Ruby's lookup of a constant's first name where it is written: the
    /// lexical scopes' own constants, then the innermost scope's
    /// ancestors (refused unless all are known and at most one, a
    /// registered one, declares it), then the top level.
    fn lexical(name: &str, cx: &ConstCx) -> Option<Hit> {
        for scope in cx.cref.iter().rev() {
            if let Some(local) = cx.local.as_ref().filter(|l| &l.class == scope) {
                let mut writes = local.writes.iter().filter(|w| w.0 == name);
                if let Some(w) = local.writes.iter().filter(|w| w.0 == name && w.1 < local.before).last() {
                    return Some(Hit::Local(name.to_string(), w.1, w.2, w.3));
                }
                // Written below: the loop runs before it exists.
                if writes.next().is_some() {
                    return None;
                }
            }
            let full = format!("{scope}::{name}");
            match find(&full) {
                Found::Missing => continue,
                Found::Unknown => return uncertain(&full),
                Found::Namespace => return Some(Hit::Namespace(full)),
                Found::Value(decl) => return Some(Hit::Decl(decl)),
            }
        }
        if let Some(inner) = cx.cref.last() {
            match ancestors(inner) {
                Err(why) => return refuse(ANCESTOR, format!("`{name}`, looked up past the ancestors of `{inner}`: {why}")),
                Ok(list) => {
                    // Ruby takes the first ancestor that declares it. The
                    // order `ancestors` lists is not Ruby's, so only one
                    // that alone declares it is certain.
                    let declaring: Vec<&String> = list.iter().filter(|a| declares(a, name)).collect();
                    match declaring.as_slice() {
                        [] => {}
                        [a] => {
                            let full = format!("{a}::{name}");
                            return match (find(&full), known(a)) {
                                (Found::Namespace, false) => Some(Hit::Namespace(full)),
                                (Found::Value(decl), false) => Some(Hit::Decl(decl)),
                                _ => refuse(ANCESTOR, format!("`{name}`, which `{a}`, an ancestor of `{inner}`, may declare")),
                            };
                        }
                        [a, b, ..] => {
                            return refuse(ANCESTOR, format!("`{name}`, which both `{a}` and `{b}`, ancestors of `{inner}`, may declare"));
                        }
                    }
                }
            }
        }
        top(name)
    }

    fn hit_value(hit: Hit, cx: &ConstCx) -> Option<Value> {
        match hit {
            Hit::Namespace(name) => Some(Value::Namespace(name)),
            Hit::Decl(decl) => {
                let (value, shape) = evaluate_at(&decl.path, decl.start, decl.end, decl.cref.clone(), None, cx.depth + 1)?;
                let full = decl.names.first()?;
                unchanged(full, &decl.path, decl.start, shape)?;
                Some(value)
            }
            Hit::Local(name, stmt, start, end) => {
                let local = cx.local.as_ref()?;
                let full = format!("{}::{name}", local.class);
                let local = Local { before: stmt, ..local.clone() };
                let (value, shape) = evaluate_at(&cx.file, start, end, cx.cref.clone(), Some(local), cx.depth + 1)?;
                unchanged(&full, &cx.file, start, shape)?;
                Some(value)
            }
        }
    }

    /// The value of the expression at `start..end` of `path`, as Ruby
    /// computed it when the file loaded (a part not known is `Opaque`),
    /// and its `shape`.
    fn evaluate_at(
        path: &str,
        start: usize,
        end: usize,
        cref: Vec<String>,
        local: Option<Local>,
        depth: usize,
    ) -> Option<(Value, Option<Shape>)> {
        sources::with_text(path, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let node = node_at(&root, start, end)?;
            let cx = ConstCx { file: path.to_string(), cref, local, depth };
            let env = Env { vars: Vec::new(), cx: Some(Rc::new(cx)), mode: Mode::Loaded };
            let value = eval(&node, &env).unwrap_or(Value::Opaque);
            Some((value, quietly(|| shape(&node, &env))))
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
    /// assigns or declares (`class`/`module`), less those it makes
    /// `private_constant`; a set, in no order known here (Ruby lists them
    /// in the order of its symbol table). Only for a body of statements
    /// listed as adding no other constant (`body_constants`), a module
    /// Zeitwerk has no directory of files for, and no use of the module
    /// anywhere (its own file included) that may add to it
    /// (`changed_from_outside`): `Mod::X = …`, `class Mod::X`,
    /// `Mod.const_set`, `Mod.autoload`, `Alias = Mod`, any call not
    /// listed as only reading it. Else a named gap.
    pub(super) fn module_constants(module: &str) -> Option<Vec<Value>> {
        let last = module.rsplit("::").next()?;
        let decls: Vec<Decl> = decls(last).iter().filter(|d| d.names.iter().any(|n| n == module)).cloned().collect();
        let [decl] = decls.as_slice() else {
            return refuse(MODULE_BODY, format!("`{module}.constants`: `{module}` is not declared by one `module` body"));
        };
        if !decl.certain || decl.names.len() != 1 || decl.kind != Kind::Module {
            return refuse(MODULE_BODY, format!("`{module}.constants`: `{module}` is not declared by one `module` body"));
        }
        if let Some(why) = late_reason(module) {
            return refuse(LATE_SOURCE, format!("`{module}.constants`: {why}"));
        }
        let dir = format!("/{}/", underscore(module));
        let mut id = 1;
        while let Some(path) = sources::path_of(FileId(id)) {
            if path.contains(&dir) {
                return refuse(
                    REOPENED,
                    format!("`{module}.constants`: Zeitwerk may load {} into it", sources::relative_path(&path)),
                );
            }
            id += 1;
        }
        let body = sources::with_text(&decl.path, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let node = node_at(&root, decl.start, decl.end).ok_or_else(|| String::from("its body is not found"))?;
            let module_node = node.as_module_node().ok_or_else(|| String::from("its body is not found"))?;
            Ok::<_, String>((body_constants(&module_node, text)?, readers(&module_node)))
        });
        let (names, readers) = match body {
            Some(Ok(body)) => body,
            Some(Err(why)) => return refuse(MODULE_BODY, format!("`{module}.constants`: {why}")),
            None => return refuse(MODULE_BODY, format!("`{module}.constants`: its source is not read")),
        };
        // `Mod.public_reasons`, a method of its own body known to add none.
        let own = |s: &Site| matches!(&s.used, Use::Call { block: false, .. } if readers.contains(&s.method));
        let name = module.rsplit("::").next().unwrap_or(module);
        if let Some(s) = sites_of(name).iter().find(|s| changes(s, Reach::Constants) && !own(s) && relevant(s, module)) {
            return refuse(REOPENED, format!("`{module}.constants`: `{module}` may be changed at {}", describe_at(&s.path, s.at)));
        }
        if let Some((path, at)) = redefinition() {
            return refuse(REOPENED, format!("`{module}.constants`: `constants` may be defined again at {}", describe_at(&path, at)));
        }
        if let Some((path, at)) = computed_change(|e| e.module, |_| module.to_string()) {
            return refuse(
                REOPENED,
                format!("`{module}.constants`: a module looked up by a name not known before it runs is changed at {}", describe_at(&path, at)),
            );
        }
        Some(names.into_iter().map(Value::Sym).collect())
    }

    /// The methods of the module `module_node` itself (`def self.x`, or in
    /// `class << self`) whose code adds no constant: none written, no
    /// `self`, `super` or `const_set`/`eval`/`send`, no call without a
    /// receiver but one listed as only reading a module.
    fn readers(module_node: &ruby_prism::ModuleNode<'_>) -> Vec<String> {
        use ruby_prism::Visit;
        struct Scan {
            found: bool,
        }
        impl<'pr> Visit<'pr> for Scan {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                self.found |= node.as_super_node().is_some()
                    || node.as_forwarding_super_node().is_some()
                    || node.as_call_node().is_some_and(|c| {
                        c.receiver().is_none() && !MODULE_READS.contains(&constant_id_str(&c.name()))
                    });
            }
            fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
                self.found |= node.as_self_node().is_some()
                    || node.as_forwarding_super_node().is_some()
                    || node.as_call_node().is_some_and(|c| {
                        c.receiver().is_none() && !MODULE_READS.contains(&constant_id_str(&c.name()))
                    });
            }
        }
        let body = module_node.body().map(flatten_statements).unwrap_or_default();
        let mut defs: Vec<ruby_prism::DefNode<'_>> = Vec::new();
        for stmt in &body {
            if let Some(def) = stmt.as_def_node().filter(|d| d.receiver().is_some_and(|r| r.as_self_node().is_some())) {
                defs.push(def);
            }
            if let Some(singleton) = stmt.as_singleton_class_node().filter(|c| c.expression().as_self_node().is_some()) {
                let inner = singleton.body().map(flatten_statements).unwrap_or_default();
                defs.extend(inner.iter().filter_map(|s| s.as_def_node()).filter(|d| d.receiver().is_none()));
            }
        }
        defs.into_iter()
            .filter(|def| {
                let Some(code) = def.body() else { return true };
                let mut scan = Scan { found: false };
                scan.visit(&code);
                !scan.found && !writes_constants(&code)
            })
            .map(|def| constant_id_str(&def.name()).to_string())
            .collect()
    }

    /// Methods Ruby calls on a module as code runs (a constant or method
    /// added to it, the module included somewhere): one may add
    /// constants.
    const HOOKS: &[&str] = &[
        "included", "extended", "prepended", "inherited", "append_features", "prepend_features", "extend_object",
        "method_added", "singleton_method_added", "const_added", "const_missing",
    ];

    /// Modules a module body may `extend` knowing no constant comes of it.
    const EXTENDS: &[&str] =
        &["self", "ActiveSupport::Concern", "T::Sig", "T::Helpers", "T::Generic", "Enumerable", "Comparable", "Forwardable"];

    /// Methods a module answers what constants it has with: a source
    /// that defines one of them again (for a module, or for every module
    /// in `Module`) may make `Mod.constants` other than its body shows.
    const REFLECTION: &[&str] = &["constants", "const_get", "const_defined?", "const_source_location"];

    /// The `REFLECTION` method `node` defines: `def constants`, `def
    /// self.constants`, or one given to `private`/`module_function`/….
    fn redefined(node: &Node<'_>) -> Option<String> {
        let name = |d: &ruby_prism::DefNode<'_>| Some(constant_id_str(&d.name()).to_string()).filter(|n| REFLECTION.contains(&n.as_str()));
        if let Some(def) = node.as_def_node() {
            return name(&def);
        }
        let call = node.as_call_node().filter(|c| c.receiver().is_none())?;
        call.arguments()?.arguments().iter().find_map(|a| a.as_def_node().and_then(|d| name(&d)))
    }

    /// The constants the body of `module_node` declares, listed by
    /// statement: `NAME = value` (no constant written in `value`),
    /// `class`/`module NAME`, `def` (not a hook), `class << self` of
    /// those, `private_constant :NAME`, `extend` of a listed module,
    /// `private`/`sig`/…. Any other statement: why not.
    fn body_constants(module_node: &ruby_prism::ModuleNode<'_>, text: &str) -> Result<Vec<String>, String> {
        let line = |n: &Node<'_>| {
            let start = n.location().start_offset();
            let line = text[..start].matches('\n').count() + 1;
            let first = String::from_utf8_lossy(n.location().as_slice()).lines().next().unwrap_or("").trim().to_string();
            format!("its body runs `{first}` (line {line}), not listed as adding no constant")
        };
        let def_ok = |n: &Node<'_>| n.as_def_node().is_some_and(|d| !HOOKS.contains(&constant_id_str(&d.name())));
        // `def self.constants`, or `def constants` that `extend self` or
        // `module_function` makes the module's own.
        let again = |n: &Node<'_>| {
            redefined(n).map(|name| {
                let line = text[..n.location().start_offset()].matches('\n').count() + 1;
                format!("its body defines `{name}` (line {line}), which `Module#constants` answers with")
            })
        };
        let mut out: Vec<String> = Vec::new();
        let mut private: Vec<String> = Vec::new();
        for stmt in module_node.body().map(flatten_statements).unwrap_or_default() {
            if let Some(why) = again(&stmt) {
                return Err(why);
            }
            if let Some(write) = stmt.as_constant_write_node() {
                // `A = B = :x` writes `B` too; `A = const_set(…)` more.
                if writes_constants(&write.value()) {
                    return Err(line(&stmt));
                }
                out.push(constant_id_str(&write.name()).to_string());
            } else if let Some(path) = stmt.as_module_node().map(|m| m.constant_path()).or_else(|| stmt.as_class_node().map(|c| c.constant_path())) {
                // `class Foo < Class.new { Z = 1 }`, `class Foo < (Z = Object)`:
                // the superclass is evaluated in this body.
                if stmt.as_class_node().and_then(|c| c.superclass()).is_some_and(|s| writes_constants(&s)) {
                    return Err(line(&stmt));
                }
                match segments(&path) {
                    Some((segs, false)) if segs.len() == 1 => out.push(segs[0].clone()),
                    _ => return Err(line(&stmt)),
                }
            } else if stmt.as_def_node().is_some() {
                if !def_ok(&stmt) {
                    return Err(line(&stmt));
                }
            } else if let Some(singleton) = stmt.as_singleton_class_node() {
                let body = singleton.body().map(flatten_statements).unwrap_or_default();
                if !singleton.expression().as_self_node().is_some() {
                    return Err(line(&stmt));
                }
                if let Some(why) = body.iter().find_map(|s| again(s)) {
                    return Err(why);
                }
                if let Some(bad) = body.iter().find(|s| !(def_ok(s) || declaration_call(s))) {
                    return Err(line(bad));
                }
            } else if let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && c.name().as_slice() == b"private_constant") {
                for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
                    let Some(sym) = arg.as_symbol_node() else { return Err(line(&stmt)) };
                    private.push(String::from_utf8_lossy(sym.unescaped()).into_owned());
                }
            } else if !declaration_call(&stmt) {
                return Err(line(&stmt));
            }
        }
        let mut names: Vec<String> = Vec::new();
        for name in out {
            if !names.contains(&name) && !private.contains(&name) {
                names.push(name);
            }
        }
        Ok(names)
    }

    /// Does `node` write a constant, or call what may (`const_set`,
    /// `class_eval`, `send`, a `Class.new`/`Module.new` block)?
    fn writes_constants(node: &Node<'_>) -> bool {
        use ruby_prism::Visit;
        struct Scan {
            found: bool,
        }
        impl<'pr> Visit<'pr> for Scan {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                self.found |= node.as_constant_write_node().is_some()
                    || node.as_constant_or_write_node().is_some()
                    || node.as_constant_and_write_node().is_some()
                    || node.as_constant_operator_write_node().is_some()
                    || node.as_constant_path_write_node().is_some()
                    || node.as_constant_path_or_write_node().is_some()
                    || node.as_constant_path_and_write_node().is_some()
                    || node.as_constant_path_operator_write_node().is_some()
                    || node.as_multi_write_node().is_some_and(|m| {
                        m.lefts().iter().chain(m.rest()).chain(m.rights().iter()).any(|t| {
                            t.as_constant_target_node().is_some() || t.as_constant_path_target_node().is_some()
                        })
                    })
                    || node.as_call_node().is_some_and(|c| {
                        matches!(
                            c.name().as_slice(),
                            b"const_set" | b"class_eval" | b"module_eval" | b"class_exec" | b"module_exec" | b"instance_eval"
                                | b"instance_exec" | b"send" | b"public_send" | b"__send__" | b"eval"
                        ) || (c.name().as_slice() == b"new" && c.block().is_some())
                    });
            }
        }
        let mut scan = Scan { found: false };
        scan.visit(node);
        scan.found
    }

    /// `extend T::Sig`, `private`, `sig { … }`: a call that adds no
    /// constant to the module it is in.
    fn declaration_call(node: &Node<'_>) -> bool {
        let Some(call) = node.as_call_node() else { return false };
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        call.receiver().is_none()
            && match constant_id_str(&call.name()) {
                "extend" => {
                    call.block().is_none()
                        && !args.is_empty()
                        && args.iter().all(|a| {
                            a.as_self_node().is_some()
                                || segments(a).is_some_and(|(segs, _)| EXTENDS.contains(&segs.join("::").as_str()))
                        })
                }
                "private" | "public" | "protected" | "module_function" | "private_class_method" | "public_class_method" => {
                    call.block().is_none()
                        && args.iter().all(|a| a.as_symbol_node().is_some() || a.as_def_node().is_some_and(|d| !HOOKS.contains(&constant_id_str(&d.name()))))
                }
                "sig" => args.is_empty(),
                _ => false,
            }
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
        if let Some(found) = with_memo(|m| m.found.get(full).cloned()) {
            return found;
        }
        let found = find_uncached(full);
        with_memo(|m| m.found.insert(full.to_string(), found.clone()));
        found
    }

    fn find_uncached(full: &str) -> Found {
        let last = full.rsplit("::").next().unwrap_or(full);
        // A test source not registered yet may declare it again.
        if late_decls(last).iter().any(|d| d.names.iter().any(|n| n == full)) {
            return Found::Unknown;
        }
        let decls = decls(last);
        let found: Vec<&Decl> = decls.iter().filter(|d| d.names.iter().any(|n| n == full)).collect();
        if found.is_empty() {
            return Found::Missing;
        }
        if found.iter().any(|d| !d.certain || d.names.len() > 1) {
            return Found::Unknown;
        }
        let consts: Vec<&Decl> = found.iter().copied().filter(|d| d.kind == Kind::Const).collect();
        match consts.as_slice() {
            [] => Found::Namespace,
            [one] if found.len() == 1 => Found::Value((*one).clone()),
            _ => Found::Unknown,
        }
    }

    // ---- Ancestors -------------------------------------------------

    /// Modules of Minitest, ActiveSupport, Rails and Mocha (and Ruby's
    /// `Comparable`/`Enumerable`, StatsD's assertions) a test class may
    /// have as ancestors without a registered source: the ancestors of
    /// Core's test case classes under its Rails and Minitest 5.27.
    const KNOWN_MODULES: &[&str] = &[
        "AbstractController::Helpers", "AbstractController::Helpers::Resolution", "ActionController::TemplateAssertions",
        "ActionController::TestCase", "ActionController::TestCase::Behavior", "ActionDispatch::Assertions",
        "ActionDispatch::Assertions::ResponseAssertions", "ActionDispatch::Assertions::RoutingAssertions",
        "ActionDispatch::Assertions::RoutingAssertions::WithIntegrationRouting", "ActionDispatch::Integration::Runner",
        "ActionDispatch::IntegrationTest", "ActionDispatch::IntegrationTest::Behavior",
        "ActionDispatch::IntegrationTest::UrlOptions", "ActionDispatch::Routing::PolymorphicRoutes",
        "ActionDispatch::Routing::UrlFor", "ActionDispatch::TestHelpers::PageDumpHelper", "ActionDispatch::TestProcess",
        "ActionDispatch::TestProcess::FixtureFile", "ActionView::Context", "ActionView::Helpers",
        "ActionView::Helpers::ActiveModelHelper", "ActionView::Helpers::AssetTagHelper", "ActionView::Helpers::AssetUrlHelper",
        "ActionView::Helpers::AtomFeedHelper", "ActionView::Helpers::CacheHelper", "ActionView::Helpers::CaptureHelper",
        "ActionView::Helpers::ContentExfiltrationPreventionHelper", "ActionView::Helpers::ControllerHelper",
        "ActionView::Helpers::CspHelper", "ActionView::Helpers::CsrfHelper", "ActionView::Helpers::DateHelper",
        "ActionView::Helpers::DebugHelper", "ActionView::Helpers::FormHelper", "ActionView::Helpers::FormOptionsHelper",
        "ActionView::Helpers::FormTagHelper", "ActionView::Helpers::JavaScriptHelper", "ActionView::Helpers::NavigationHelper",
        "ActionView::Helpers::NumberHelper", "ActionView::Helpers::OutputSafetyHelper", "ActionView::Helpers::RenderingHelper",
        "ActionView::Helpers::SanitizeHelper", "ActionView::Helpers::TagHelper", "ActionView::Helpers::TextHelper",
        "ActionView::Helpers::TranslationHelper", "ActionView::Helpers::UrlHelper", "ActionView::ModelNaming",
        "ActionView::RecordIdentifier", "ActionView::RoutingUrlFor", "ActionView::TestCase", "ActionView::TestCase::Behavior",
        "ActiveJob::TestCase", "ActiveJob::TestHelper", "ActiveRecord::TestDatabases", "ActiveRecord::TestFixtures",
        "ActiveSupport::Benchmarkable", "ActiveSupport::Callbacks", "ActiveSupport::Dependencies::RequireDependency",
        "ActiveSupport::DescendantsTracker", "ActiveSupport::TestCase", "ActiveSupport::Testing::Assertions",
        "ActiveSupport::Testing::ConstantLookup", "ActiveSupport::Testing::ConstantStubbing",
        "ActiveSupport::Testing::Declarative", "ActiveSupport::Testing::Deprecation",
        "ActiveSupport::Testing::ErrorReporterAssertions", "ActiveSupport::Testing::EventReporterAssertions",
        "ActiveSupport::Testing::FileFixtures", "ActiveSupport::Testing::NotificationAssertions",
        "ActiveSupport::Testing::SetupAndTeardown", "ActiveSupport::Testing::Stream", "ActiveSupport::Testing::TaggedLogging",
        "ActiveSupport::Testing::TestsWithoutAssertions", "ActiveSupport::Testing::TimeHelpers", "FileUtils",
        "FileUtils::StreamUtils_", "Minitest::Assertions", "Minitest::Guard", "Minitest::Reportable", "Minitest::Runnable",
        "Minitest::Test", "Minitest::Test::LifecycleHooks", "Mocha::API", "Mocha::Hooks",
        "Mocha::Integration::Minitest::Adapter", "Mocha::ParameterMatchers", "Rails::Dom::Testing::Assertions",
        "Rails::Dom::Testing::Assertions::DomAssertions", "Rails::Dom::Testing::Assertions::SelectorAssertions",
        "Rails::Generators::TestCase", "Rails::Generators::Testing::Assertions", "Rails::Generators::Testing::Behavior",
        "Rails::Generators::Testing::SetupAndTeardown", "Comparable", "Enumerable", "StatsD::Instrument::Assertions",
        "StatsD::Instrument::Helpers",
        // Core's `TestGenerator` (platform/essentials'
        // `lib/platform/testing/test_generator.rb`, whose `generate_test`
        // `test_macros` models): no constants, no ancestors.
        "TestGenerator",
    ];

    /// Every constant any of `KNOWN_MODULES` has (`constants(false)` of
    /// each, under Core's Rails and Minitest 5.27): one of these names is
    /// not looked up past such an ancestor.
    const KNOWN_CONSTANTS: &[&str] = &[
        "APP_SESSIONS", "ARIA_PREFIXES", "ASSET_EXTENSIONS", "ASSET_PUBLIC_DIRECTORIES", "ActiveModelHelper",
        "ActiveModelInstanceTag", "AllOf", "AnyOf", "AnyParameters", "Anything", "AroundCallbackSupport", "Assertion",
        "AssetTagHelper", "AssetUrlHelper", "AtomBuilder", "AtomFeedBuilder", "AtomFeedHelper", "BASE_DIR",
        "BOOLEAN_ATTRIBUTES", "BUTTON_TAG_METHOD_VERBS", "Base", "Behavior", "CALLBACK_FILTER_TYPES", "CLASS_PREFIXES",
        "CLOSE_CDATA_COMMENT", "CLOSE_FORM_TAG", "CLOSE_OPTION_TAG", "CLOSE_QUOTES_COMMENT",
        "CONTENT_EXFILTRATION_PREVENTION_MARKUP", "CONTROLLER_DELEGATES", "CacheHelper", "CachingRegistry", "CallTemplate",
        "Callback", "CallbackChain", "CallbackSequence", "CaptureHelper", "ClassMethods", "Conditionals",
        "ContentExfiltrationPreventionHelper", "ControllerHelper", "CspHelper", "CsrfHelper", "Cycle", "DATA_PREFIXES",
        "DateHelper", "DateTimeSelector", "DebugHelper", "DomAssertions", "DryRun", "E", "Entry_", "Equals",
        "EquivalentUri", "ErrorCollector", "EventCollector", "Filters", "FixtureFile", "FormBuilder", "FormHelper",
        "FormOptionsHelper", "FormTagHelper", "HTMLSelector", "HasEntries", "HasEntry", "HasKey", "HasKeys", "HasValue",
        "HelperMethodBuilder", "HelperMethods", "INTERNAL_IVARS", "Includes", "InstanceMethods", "InstanceOf",
        "InvalidNumberError", "InvalidResponse", "IsA", "JOIN", "JS_ESCAPE_MAP", "JavaScriptHelper", "KindOf",
        "LOW_METHODS", "LifecycleHooks", "Locals", "LowMethods", "MAX_HEADER_SIZE", "METHODS", "MINUTES_IN_QUARTER_YEAR",
        "MINUTES_IN_THREE_QUARTERS_YEAR", "MINUTES_IN_YEAR", "NEW", "NavigationHelper", "NoWrite", "Not", "NumberHelper",
        "OPT_TABLE", "Optionally", "OutputSafetyHelper", "PASSTHROUGH_EXCEPTIONS", "PRE_CONTENT_STRINGS",
        "PositionalOrKeywordHash", "RESPONSE_PREDICATES", "RegexpMatches", "ReloadedClassesFiltering",
        "RenderedViewContent", "RenderedViewsCollection", "RenderingHelper", "Resolution", "RespondsWith",
        "ResponseAssertions", "RoutingAssertions", "SETUP_METHODS", "SIGNALS", "STRINGIFIED_COMMON_METHODS",
        "SanitizeHelper", "SelectorAssertions", "StreamUtils_", "SubstitutionContext", "TAG_TYPES", "TEARDOWN_METHODS",
        "TagBuilder", "TagHelper", "Tags", "TestController", "TestQueueAdapter", "TextHelper", "TranslationHelper",
        "UNDEFINED", "UNTRACKED", "URI_REGEXP", "UncacheableFragmentError", "UrlHelper", "UrlOptions", "VERSION", "Verbose",
        "WeakSet", "WithIntegrationRouting", "YamlEquivalent",
    ];

    /// The known classes' superclasses (their modules' constants are all
    /// in `KNOWN_CONSTANTS`; the chain matters for what registered
    /// sources reopen or include into them).
    const KNOWN_SUPERCLASSES: &[(&str, &str)] = &[
        ("Minitest::Test", "Minitest::Runnable"),
        ("ActiveSupport::TestCase", "Minitest::Test"),
        ("ActionDispatch::IntegrationTest", "ActiveSupport::TestCase"),
        ("ActionController::TestCase", "ActiveSupport::TestCase"),
        ("ActionView::TestCase", "ActiveSupport::TestCase"),
        ("ActiveJob::TestCase", "ActiveSupport::TestCase"),
        ("Rails::Generators::TestCase", "ActiveSupport::TestCase"),
    ];

    /// `ActiveSupport.on_load` hooks that run in a known class.
    const LOAD_HOOKS: &[(&str, &str)] = &[
        ("ActiveSupport::TestCase", "active_support_test_case"),
        ("ActionDispatch::IntegrationTest", "action_dispatch_integration_test"),
        ("ActionController::TestCase", "action_controller_test_case"),
        ("ActionView::TestCase", "action_view_test_case"),
        ("ActiveJob::TestCase", "active_job_test_case"),
    ];

    fn known(module: &str) -> bool {
        KNOWN_MODULES.contains(&module)
    }

    /// Is `module` (a full name) one of the framework's that a test class
    /// may have as an ancestor without a registered source?
    pub(in super::super) fn known_module(module: &str) -> bool {
        known(module)
    }

    /// The `module` statements that declare the module `full`, as (file,
    /// start, end, the lexical scopes inside it). `Err` says why they
    /// are not all known: it is not declared in a registered source, a
    /// test source not registered yet declares it, it is not a module
    /// declared with certainty, or a source changes it from outside its
    /// body.
    pub(in super::super) fn module_declarations(full: &str) -> Result<Vec<(String, usize, usize, Vec<String>)>, String> {
        if let Some(why) = late_reason(full) {
            return Err(why);
        }
        let last = full.rsplit("::").next().unwrap_or(full);
        let own: Vec<Decl> = decls(last).iter().filter(|d| d.names.iter().any(|n| n == full)).cloned().collect();
        if own.is_empty() {
            return Err(format!("`{full}` is not in a registered source"));
        }
        if own.iter().any(|d| !d.certain || d.names.len() > 1 || d.kind != Kind::Module) {
            return Err(format!("`{full}` is not a module declared with certainty"));
        }
        if let Some(site) = changed_from_outside(full, Reach::Methods) {
            return Err(format!("`{full}` {site}"));
        }
        Ok(own
            .into_iter()
            .map(|d| {
                let mut cref = d.cref.clone();
                cref.push(full.to_string());
                (d.path, d.start, d.end, cref)
            })
            .collect())
    }

    /// The test source not registered now that declares a constant
    /// written `written` (`A::B`, under any scope), by its path relative
    /// to the app.
    pub(in super::super) fn late_declared(written: &str) -> Option<String> {
        let last = written.rsplit("::").next().unwrap_or(written);
        let suffix = format!("::{written}");
        late_decls(last)
            .iter()
            .find(|d| d.names.iter().any(|n| n == written || n.ends_with(&suffix)))
            .map(|d| sources::relative_path(&d.path))
    }

    /// Is `last` the last name of one of `KNOWN_MODULES` (a framework
    /// module a test class has as an ancestor without a source)?
    pub(in super::super) fn known_module_named(last: &str) -> bool {
        KNOWN_MODULES.iter().any(|m| m.rsplit("::").next() == Some(last))
    }

    /// Namespaces of the framework's test case classes: a superclass
    /// under one that no source declares is the framework's.
    const FRAMEWORK: &[&str] = &[
        "ActiveSupport::", "ActionDispatch::", "ActionController::", "ActionView::", "ActionMailer::", "ActionCable::",
        "ActiveJob::", "ActionMailbox::", "Rails::", "Minitest::",
    ];

    /// The last names of the superclasses of a class whose superclass is
    /// written `written`, each as written (a class of that name in any
    /// namespace may be it), up to a framework class and its known
    /// superclasses. `Err` for one no source (registered, or a test
    /// source on disk) declares, or whose superclass is not a constant.
    pub(in super::super) fn superclass_names(written: &[String]) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        let mut todo: Vec<Vec<String>> = vec![written.to_vec()];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(segs) = todo.pop() {
            let full = segs.join("::");
            if !seen.insert(full.clone()) {
                continue;
            }
            if seen.len() > MAX_ANCESTORS {
                return Err(format!("it has more than {MAX_ANCESTORS} superclasses"));
            }
            let Some(last) = segs.last().cloned() else { continue };
            out.push(last.clone());
            let suffix = format!("::{full}");
            if let Some(&(mut class, _)) = KNOWN_SUPERCLASSES.iter().find(|(c, _)| *c == full || c.ends_with(&suffix)) {
                while let Some((_, parent)) = KNOWN_SUPERCLASSES.iter().find(|(c, _)| *c == class) {
                    out.extend(parent.rsplit("::").next().map(str::to_string));
                    class = parent;
                }
                continue;
            }
            let found: Vec<Decl> = decls(&last)
                .iter()
                .chain(late_decls(&last).iter())
                .filter(|d| d.kind == Kind::Class && d.names.iter().any(|n| *n == full || n.ends_with(&suffix)))
                .cloned()
                .collect();
            if found.is_empty() {
                if FRAMEWORK.iter().any(|f| full.starts_with(f)) {
                    continue;
                }
                return Err(format!("its superclass `{full}` is not declared in a source"));
            }
            for decl in found {
                let parent = with_any_text(&decl.path, |text| {
                    let result = ruby_prism::parse(text.as_bytes());
                    let root = result.node();
                    let class = node_at(&root, decl.start, decl.end)?.as_class_node()?;
                    Some(class.superclass().map(|s| segments(&s).map(|(segs, _)| segs)))
                })
                .flatten();
                match parent {
                    None => return Err(format!("the declaration of `{full}` was not found")),
                    Some(None) => {}
                    Some(Some(None)) => return Err(format!("the superclass of `{full}` is not a constant")),
                    Some(Some(Some(segs))) => todo.push(segs),
                }
            }
        }
        Ok(out)
    }

    /// The sources (registered, or test sources on disk not registered
    /// now) that have the capitalized word `name`, as (path, registered).
    pub(in super::super) fn files_mentioning(name: &str) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> =
            files_with(name, 0).into_iter().filter_map(|id| sources::path_of(FileId(id))).map(|p| (p, true)).collect();
        let (files, late) = late_sources(Some(name));
        out.extend(late.into_iter().map(|i| (files[i].0.clone(), false)));
        out
    }

    /// The registered files (ids above `after`) that have the capitalized
    /// word `name`, and the id of the last registered file.
    pub(in super::super) fn registered_mentioning(name: &str, after: u32) -> (Vec<String>, u32) {
        let ids = files_with(name, after);
        (ids.into_iter().filter_map(|id| sources::path_of(FileId(id))).collect(), registered_last())
    }

    /// Where a source other than the `module` statement at `start..end`
    /// of `path` (in the scope whose last name is `scope`) declares the
    /// module `full` again, or its `ClassMethods`, or changes either from
    /// outside (`X.class_eval`, `X::ClassMethods.define_method`): a
    /// registered source or a test source on disk.
    pub(in super::super) fn reopened_elsewhere(full: &str, path: &str, start: usize, end: usize) -> Option<String> {
        let last = full.rsplit("::").next().unwrap_or(full);
        let inside = |d: &Decl| d.path == path && start <= d.start && d.end <= end;
        let late = |p: &str| if is_registered(p) { String::new() } else { ", a test source not registered before this one".into() };
        if let Some(d) = decls(last).iter().chain(late_decls(last).iter()).find(|d| d.names.iter().any(|n| n == full) && !inside(d)) {
            return Some(format!("`{full}` is declared again in {}{}", sources::relative_path(&d.path), late(&d.path)));
        }
        let methods = format!("{full}::ClassMethods");
        for (file, _) in files_mentioning(last) {
            let found = with_any_text(&file, |text| {
                if !may_declare(text, "ClassMethods") {
                    return None;
                }
                let d = scan(text, &file, "ClassMethods").into_iter().find(|d| d.names.iter().any(|n| *n == methods) && !inside(d))?;
                Some(format!("`{methods}` is declared again in {}{}", sources::relative_path(&d.path), late(&d.path)))
            })
            .flatten();
            if found.is_some() {
                return found;
            }
        }
        // `X::ClassMethods.define_method(…)`, `X::ClassMethods.include(…)`.
        if let Some(site) = changed_from_outside(&methods, Reach::Methods) {
            return Some(format!("`{methods}` {site}"));
        }
        changed_from_outside(full, Reach::Methods).map(|site| format!("`{full}` {site}"))
    }

    /// May `module` (an ancestor) have the constant `name` itself?
    fn declares(module: &str, name: &str) -> bool {
        !matches!(find(&format!("{module}::{name}")), Found::Missing) || (known(module) && KNOWN_CONSTANTS.contains(&name))
    }

    thread_local! {
        /// `ancestors` answers, with the registry they were computed for:
        /// (its first path, how many files).
        #[allow(clippy::type_complexity)]
        static ANCESTRY: RefCell<(Option<String>, u32, HashMap<String, Result<Vec<String>, String>>)> =
            RefCell::new((None, 0, HashMap::new()));
        /// The scopes whose `ancestors` are being computed, and how many
        /// times one was asked for again inside (an answer that depends
        /// on such a cut is not kept).
        static IN_PROGRESS: RefCell<(Vec<String>, u32)> = const { RefCell::new((Vec::new(), 0)) };
    }

    /// The registry: its first path and how many files (ids are dense
    /// from 1, so found by bisection).
    fn registered() -> (Option<String>, u32) {
        let exists = |n: u32| n == 0 || sources::path_of(FileId(n)).is_some();
        let mut high = 1u32;
        while exists(high) {
            high = high.saturating_mul(2);
            if high == u32::MAX {
                break;
            }
        }
        // exists(low), !exists(high)
        let mut low = high / 2;
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if exists(mid) {
                low = mid;
            } else {
                high = mid;
            }
        }
        (sources::path_of(FileId(1)), low)
    }

    /// The ancestors of the class or module `scope` (a full name) other
    /// than itself, `Object` and what Object includes: its superclasses,
    /// and the modules each of them includes or prepends, by their full
    /// names. `Err` names one that is not known: not registered and not
    /// a `KNOWN_MODULES`, declared by an assignment, more than once
    /// uncertainly, including a module ingest does not follow (`include`
    /// in a conditional or a method, `send(:include, …)`), or changed
    /// from outside its body (`X.include`, `X.class_eval`, an `on_load`
    /// hook).
    fn ancestors(scope: &str) -> Result<Vec<String>, String> {
        let (first, count) = registered();
        if let Some(found) = ANCESTRY.with(|a| {
            let a = a.borrow();
            (a.0 == first && a.1 == count).then(|| a.2.get(scope).cloned()).flatten()
        }) {
            return found;
        }
        // Whether a use elsewhere changes a module may turn on the
        // ancestors of the class it is in, and those on that module's.
        let cuts = IN_PROGRESS.with(|p| {
            let mut p = p.borrow_mut();
            if p.0.iter().any(|s| s == scope) {
                p.1 += 1;
                return None;
            }
            p.0.push(scope.to_string());
            Some(p.1)
        });
        let Some(cuts) = cuts else {
            return Err(format!("the ancestors of `{scope}` turn on themselves"));
        };
        let found = ancestors_of(scope);
        let cut = IN_PROGRESS.with(|p| {
            let mut p = p.borrow_mut();
            p.0.pop();
            p.1 != cuts
        });
        if cut {
            return found;
        }
        ANCESTRY.with(|a| {
            let mut a = a.borrow_mut();
            if a.0 != first || a.1 != count {
                *a = (first, count, HashMap::new());
            }
            a.2.insert(scope.to_string(), found.clone());
        });
        found
    }

    fn ancestors_of(scope: &str) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        let mut todo: Vec<String> = vec![scope.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        let found = loop {
            let Some(module) = todo.pop() else { break Ok(out) };
            if !seen.insert(module.clone()) {
                continue;
            }
            if seen.len() > MAX_ANCESTORS {
                break Err(format!("`{scope}` has more than {MAX_ANCESTORS} ancestors"));
            }
            match direct_ancestors(&module) {
                Ok(parents) => {
                    if module != scope {
                        out.push(module);
                    }
                    todo.extend(parents.into_iter().rev());
                }
                Err(why) => break Err(why),
            }
        };
        found
    }

    /// The superclass and the included and prepended modules of
    /// `module`'s own declarations.
    fn direct_ancestors(module: &str) -> Result<Vec<String>, String> {
        let last = module.rsplit("::").next().unwrap_or(module);
        let mut parents: Vec<String> = Vec::new();
        if let Some((_, parent)) = KNOWN_SUPERCLASSES.iter().find(|(c, _)| *c == module) {
            parents.push(parent.to_string());
        }
        let own: Vec<Decl> = decls(last).iter().filter(|d| d.names.iter().any(|n| n == module)).cloned().collect();
        if let Some(why) = late_reason(module) {
            return Err(why);
        }
        if !known(module) {
            match find(module) {
                Found::Namespace => {}
                Found::Missing => return Err(format!("`{module}` is not in a registered source")),
                Found::Unknown => return Err(format!("`{module}` is not declared once with certainty")),
                Found::Value(_) => return Err(format!("`{module}` is assigned, not declared by `class`/`module`")),
            }
        } else if own.iter().any(|d| !d.certain || d.names.len() > 1 || d.kind == Kind::Const) {
            return Err(format!("`{module}` is not reopened with certainty"));
        }
        for decl in &own {
            body_ancestors(decl, module, &mut parents)?;
        }
        if let Some(site) = changed_from_outside(module, Reach::Ancestry) {
            return Err(format!("`{module}` {site}"));
        }
        if let Some((_, hook)) = LOAD_HOOKS.iter().find(|(c, _)| *c == module) {
            // `on_load` is ActiveSupport's: a file that runs one names it.
            let needle = format!(":{hook}");
            for id in files_with("ActiveSupport", 0) {
                let Some(path) = sources::path_of(FileId(id)) else { continue };
                if sources::with_text(&path, |t| t.contains(&needle)) == Some(true) {
                    return Err(format!("`{module}` has an `on_load(:{hook})` hook ({})", sources::relative_path(&path)));
                }
            }
            let (files, late) = late_sources(Some("ActiveSupport"));
            if let Some(i) = late.into_iter().find(|i| files[*i].1.contains(&needle)) {
                return Err(format!(
                    "`{module}` has an `on_load(:{hook})` hook ({}, a test source not registered before this one)",
                    sources::relative_path(&files[i].0)
                ));
            }
        }
        Ok(parents)
    }

    /// The superclass of the `class` `decl` and the modules its body
    /// includes or prepends (directly, or in a concern's `included do`).
    fn body_ancestors(decl: &Decl, module: &str, out: &mut Vec<String>) -> Result<(), String> {
        sources::with_text(&decl.path, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let Some(node) = node_at(&root, decl.start, decl.end) else {
                return Err(format!("the declaration of `{module}` was not found"));
            };
            let (body, superclass) = match (node.as_class_node(), node.as_module_node()) {
                (Some(class), _) => (class.body(), class.superclass()),
                (_, Some(m)) => (m.body(), None),
                _ => return Err(format!("the declaration of `{module}` was not found")),
            };
            if let Some(superclass) = superclass {
                let Some((segs, rooted)) = segments(&superclass) else {
                    return Err(format!("the superclass of `{module}` is not a constant"));
                };
                out.push(module_named(&decl.cref, &segs, rooted)?);
            }
            let mut inner = decl.cref.clone();
            inner.push(module.to_string());
            for stmt in body.map(flatten_statements).unwrap_or_default() {
                included_modules(&stmt, &inner, module, out, true)?;
            }
            Ok(())
        })
        .unwrap_or_else(|| Err(format!("the source of `{module}` is not registered")))
    }

    /// The modules the class-body statement `stmt` includes or prepends;
    /// `Err` for one in a place ingest does not follow.
    fn included_modules(stmt: &Node<'_>, cref: &[String], module: &str, out: &mut Vec<String>, top: bool) -> Result<(), String> {
        if stmt.as_class_node().is_some() || stmt.as_module_node().is_some() || stmt.as_singleton_class_node().is_some() {
            return Ok(());
        }
        if let Some(call) = stmt.as_call_node() {
            let name = call.name();
            let own = call.receiver().is_none_or(|r| r.as_self_node().is_some());
            if own && matches!(name.as_slice(), b"include" | b"prepend") && call.block().is_none() {
                for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
                    let Some((segs, rooted)) = segments(&arg) else {
                        return Err(format!("`{module}` includes `{}`, which is not a constant", super::first_line(&arg)));
                    };
                    out.push(module_named(cref, &segs, rooted)?);
                }
                return Ok(());
            }
            // An ActiveSupport::Concern's `included do … end` runs in the
            // class that includes it.
            if top && call.receiver().is_none() && name.as_slice() == b"included" && call.arguments().is_none() {
                if let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
                    for inner in flatten_statements(body) {
                        included_modules(&inner, cref, module, out, false)?;
                    }
                    return Ok(());
                }
            }
        }
        match nested_include(stmt) {
            Some(line) => Err(format!("`{module}` includes a module where ingest does not follow (`{line}`)")),
            None => Ok(()),
        }
    }

    /// The modules `module`, an ancestor of a class (or the class itself),
    /// adds to that class's singleton class: what a class's body
    /// `extend`s; for an included module, what its `included do` block
    /// `extend`s and its `ClassMethods` (ActiveSupport::Concern extends
    /// the includer with it). A module's own `extend` extends only the
    /// module. `Err` for an `extend` or a hook ingest does not follow.
    fn singleton_adds(module: &str) -> Result<Vec<String>, String> {
        let last = module.rsplit("::").next().unwrap_or(module);
        let mut out = Vec::new();
        let own: Vec<Decl> = decls(last).iter().filter(|d| d.names.iter().any(|n| n == module)).cloned().collect();
        for decl in &own {
            let found = sources::with_text(&decl.path, |text| -> Result<(), String> {
                let result = ruby_prism::parse(text.as_bytes());
                let root = result.node();
                let Some(node) = node_at(&root, decl.start, decl.end) else { return Ok(()) };
                let body = match (node.as_class_node(), node.as_module_node()) {
                    (Some(class), _) => class.body(),
                    (_, Some(m)) => m.body(),
                    _ => return Ok(()),
                };
                let mut inner = decl.cref.clone();
                inner.push(module.to_string());
                let stmts = body.map(flatten_statements).unwrap_or_default();
                let extends = |call: &ruby_prism::CallNode<'_>, out: &mut Vec<String>| -> Result<(), String> {
                    for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
                        let Some((segs, rooted)) = segments(&arg) else {
                            return Err(format!("`{module}` extends `{}`, which is not a constant", super::first_line(&arg)));
                        };
                        out.push(module_named(&inner, &segs, rooted)?);
                    }
                    Ok(())
                };
                for stmt in &stmts {
                    if let Some(def) = stmt.as_def_node().filter(|d| d.receiver().is_some_and(|r| r.as_self_node().is_some())) {
                        if matches!(def.name().as_slice(), b"included" | b"prepended" | b"append_features" | b"prepend_features" | b"inherited") {
                            return Err(format!("`{module}` has its own `self.{}` hook", String::from_utf8_lossy(def.name().as_slice())));
                        }
                    }
                    let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none_or(|r| r.as_self_node().is_some())) else { continue };
                    match (decl.kind, call.name().as_slice()) {
                        (Kind::Class, b"extend") => extends(&call, &mut out)?,
                        (Kind::Module, b"included" | b"prepended") if call.block().is_some() => {
                            let block = call.block().and_then(|b| b.as_block_node());
                            for inner_stmt in block.and_then(|b| b.body()).map(flatten_statements).unwrap_or_default() {
                                if let Some(c) = inner_stmt.as_call_node().filter(|c| c.receiver().is_none() && c.name().as_slice() == b"extend") {
                                    extends(&c, &mut out)?;
                                } else if let Some(line) = nested_extend(&inner_stmt) {
                                    return Err(format!("`{module}` extends its includer where ingest does not follow (`{line}`)"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(())
            });
            found.unwrap_or(Ok(()))?;
        }
        if own.iter().any(|d| d.kind == Kind::Module) {
            let methods = format!("{module}::ClassMethods");
            match find(&methods) {
                Found::Missing => {}
                Found::Namespace => out.push(methods),
                Found::Unknown | Found::Value(_) => return Err(format!("`{methods}` is not declared once with certainty")),
            }
        }
        Ok(out)
    }

    /// The first `extend` call in `node` (outside the class and module
    /// bodies it nests).
    fn nested_extend(node: &Node<'_>) -> Option<String> {
        use ruby_prism::Visit;
        struct Scan {
            found: Option<String>,
        }
        impl<'pr> Visit<'pr> for Scan {
            fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                if self.found.is_none() && node.name().as_slice() == b"extend" {
                    self.found = Some(super::first_line(&node.as_node()));
                }
                ruby_prism::visit_call_node(self, node);
            }
            fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
            fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
            fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
        }
        let mut scan = Scan { found: None };
        scan.visit(node);
        scan.found
    }

    /// The first `include`/`prepend` call, or `send(:include, …)`, in
    /// `node` outside the class and module bodies it nests. (A `send` of
    /// a name computed when it runs is taken for some other method.)
    fn nested_include(node: &Node<'_>) -> Option<String> {
        use ruby_prism::Visit;
        struct Scan {
            found: Option<String>,
        }
        impl<'pr> Visit<'pr> for Scan {
            fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                let name = node.name();
                let dynamic = matches!(name.as_slice(), b"send" | b"public_send" | b"__send__")
                    && node.arguments().and_then(|a| a.arguments().iter().next()).is_some_and(|first| {
                        let name = first.as_symbol_node().map(|s| s.unescaped().to_vec()).or_else(|| first.as_string_node().map(|s| s.unescaped().to_vec()));
                        name.is_some_and(|n| matches!(n.as_slice(), b"include" | b"prepend"))
                    });
                if self.found.is_none() && (matches!(name.as_slice(), b"include" | b"prepend") || dynamic) {
                    self.found = Some(super::first_line(&node.as_node()));
                }
                ruby_prism::visit_call_node(self, node);
            }
            fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
            fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
            fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
        }
        let mut scan = Scan { found: None };
        scan.visit(node);
        scan.found
    }

    /// The full name of the module `segs` names where `cref` is: a
    /// registered namespace or a `KNOWN_MODULES`.
    fn module_named(cref: &[String], segs: &[String], rooted: bool) -> Result<String, String> {
        let written = segs.join("::");
        let Some((first, rest)) = segs.split_first() else { return Err("an empty constant".into()) };
        let base = if rooted {
            match find(first) {
                Found::Missing => None,
                Found::Unknown | Found::Value(_) => return Err(format!("`::{written}` is not a known module")),
                Found::Namespace => Some(first.clone()),
            }
        } else {
            match first_name(cref, first) {
                Resolution::Missing => None,
                Resolution::Unknown => {
                    return Err(match late_declared(first) {
                        Some(path) => format!("`{first}` is declared in {path}, a test source not registered before this one"),
                        None => format!("`{written}` is not declared once with certainty"),
                    })
                }
                Resolution::Declared(full) => Some(full),
            }
        };
        let full = match base {
            None => written.clone(),
            Some(base) => rest.iter().fold(base, |acc, seg| format!("{acc}::{seg}")),
        };
        if known(&full) || matches!(find(&full), Found::Namespace) {
            return Ok(full);
        }
        Err(format!("`{written}` is not in a registered source"))
    }

    // ---- What sources do with a constant ------------------------------

    /// What a mention may let code do to a constant's value, in
    /// increasing order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    enum Need {
        /// Nothing: a read that returns no part of it (`size`, `join`,
        /// `include?`), a loop that declares tests or a block that only
        /// reads its parameters, a namespace (`ALL::X`), a value unused.
        Always,
        /// Its elements: `first`, `[i]`, `find`, a block that does more
        /// than read them, a new collection of them passed on.
        Elems,
        /// The object itself: passed on, bound, returned, an operand, or
        /// any call not listed here.
        Object,
        /// The constant: `const_set`/`remove_const` of its name.
        Rebind,
    }

    /// What in a constant's literal value may change in place, in
    /// increasing order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    enum Shape {
        /// Nothing: a Symbol, a number, a frozen String, `nil`, a module,
        /// a frozen Array or Hash of these.
        Deep,
        /// Its own Array or Hash, whose elements are `Deep`.
        Shallow,
        /// An element too: an Array or Hash in it that is not frozen.
        Nested,
    }

    impl Shape {
        /// May a constant of this shape be used so? A frozen value does
        /// not change; an unfrozen collection of frozen values may give
        /// its elements out; any other may only be read.
        fn allows(self, need: Need) -> bool {
            match self {
                Shape::Deep => need < Need::Rebind,
                Shape::Shallow => need <= Need::Elems,
                Shape::Nested => need == Need::Always,
            }
        }
    }

    /// The shape an element of shape `s` gives the collection holding it.
    fn part(s: Shape) -> Shape {
        if s == Shape::Deep { Shape::Deep } else { Shape::Nested }
    }

    /// The shape of one element of a collection of shape `s`.
    fn element(s: Shape) -> Shape {
        if s <= Shape::Shallow { Shape::Deep } else { Shape::Nested }
    }

    /// The shape of the constant value `node` (evaluated in `env`), read
    /// as `eval` computes it. `None` when it is not a literal of frozen
    /// values: an unfrozen String, a value a method computes. A constant
    /// in it is what that constant is; one no registered source declares
    /// (a gem's) is taken for what it names.
    fn shape(node: &Node<'_>, env: &Env) -> Option<Shape> {
        use Shape::*;
        let cx = env.cx.as_deref();
        if cx.is_some_and(|cx| cx.depth > MAX_DEPTH) {
            return None;
        }
        if matches!(
            node,
            Node::SymbolNode { .. }
                | Node::InterpolatedSymbolNode { .. }
                | Node::IntegerNode { .. }
                | Node::FloatNode { .. }
                | Node::RationalNode { .. }
                | Node::ImaginaryNode { .. }
                | Node::NilNode { .. }
                | Node::TrueNode { .. }
                | Node::FalseNode { .. }
        ) {
            return Some(Deep);
        }
        if let Some(s) = node.as_string_node() {
            return s.is_frozen().then_some(Deep);
        }
        if let Some(s) = node.as_interpolated_string_node() {
            return s.is_frozen().then_some(Deep);
        }
        if let Some(range) = node.as_range_node() {
            let end = |n: Option<Node<'_>>| n.is_none_or(|n| shape(&n, env) == Some(Deep));
            return (end(range.left()) && end(range.right())).then_some(Deep);
        }
        if let Some(parens) = node.as_parentheses_node() {
            return match parens.body().map(flatten_statements).unwrap_or_default().as_slice() {
                [only] => shape(only, env),
                _ => None,
            };
        }
        if let Some(array) = node.as_array_node() {
            return array.elements().iter().try_fold(Shallow, |out, e| Some(out.max(part(shape(&e, env)?))));
        }
        if let Some(hash) = node.as_hash_node() {
            return hash.elements().iter().try_fold(Shallow, |out, e| {
                let assoc = e.as_assoc_node()?;
                // A Hash keeps a frozen copy of a String key.
                let key = if assoc.key().as_string_node().is_some() { Deep } else { part(shape(&assoc.key(), env)?) };
                Some(out.max(key).max(part(shape(&assoc.value(), env)?)))
            });
        }
        if segments(node).is_some() {
            let Some(cx) = cx else { return Some(Deep) };
            return match quietly(|| hit(node, cx)) {
                None | Some(Hit::Namespace(_)) => Some(Deep),
                Some(Hit::Decl(d)) => shape_at(&d.path, d.start, d.end, d.cref.clone(), None, cx.depth + 1),
                Some(Hit::Local(_, stmt, start, end)) => {
                    let local = cx.local.clone().map(|l| Local { before: stmt, ..l });
                    shape_at(&cx.file, start, end, cx.cref.clone(), local, cx.depth + 1)
                }
            };
        }
        let call = node.as_call_node()?;
        let receiver = call.receiver()?;
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let name = constant_id_str(&call.name());
        let collection = || matches!(quietly(|| eval(&receiver, env)), Some(Value::Array(_) | Value::Hash(_) | Value::Unordered(_)));
        if let Some(block) = call.block() {
            // `map(&:to_sym)`: new Symbols.
            let sym = block.as_block_argument_node().and_then(|b| b.expression()).and_then(|e| e.as_symbol_node().map(|s| s.unescaped().to_vec()));
            return (matches!(name, "map" | "collect") && args.is_empty() && sym.as_deref() == Some(&b"to_sym"[..]) && collection()).then_some(Shallow);
        }
        match (name, args.as_slice()) {
            ("freeze", []) => {
                if receiver.as_string_node().is_some() || receiver.as_interpolated_string_node().is_some() {
                    return Some(Deep);
                }
                // It freezes the receiver, not its parts.
                Some(match shape(&receiver, env)? {
                    Shallow => Deep,
                    s => s,
                })
            }
            ("size" | "length" | "count" | "to_sym", []) => Some(Deep),
            ("constants", []) => Some(Shallow),
            // A new collection of the receiver's (and the arguments')
            // elements.
            ("keys" | "values", []) | ("+" | "-", [_]) | ("excluding" | "without", _) if collection() => {
                let mut out = Shallow.max(element(shape(&receiver, env)?));
                for arg in &args {
                    out = out.max(element(shape(arg, env)?));
                }
                Some(out)
            }
            // One of its elements.
            ("[]" | "fetch", [_]) | ("first" | "last", []) if collection() => Some(element(shape(&receiver, env)?)),
            _ => None,
        }
    }

    /// The shape of the value at `start..end` of `path` (`shape`).
    fn shape_at(path: &str, start: usize, end: usize, cref: Vec<String>, local: Option<Local>, depth: usize) -> Option<Shape> {
        sources::with_text(path, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let node = node_at(&root, start, end)?;
            let cx = ConstCx { file: path.to_string(), cref, local, depth };
            shape(&node, &Env { vars: Vec::new(), cx: Some(Rc::new(cx)), mode: Mode::Loaded })
        })
        .flatten()
    }

    /// `f`, keeping the refusal recorded before it: what a lookup `f`
    /// makes for itself refuses is not what a gap names.
    fn quietly<T>(f: impl FnOnce() -> T) -> T {
        let saved = REFUSAL.with(|r| r.borrow_mut().take());
        let out = f();
        REFUSAL.with(|r| *r.borrow_mut() = saved);
        out
    }

    /// Some when the constant `full` (whose value starts at `at` of
    /// `path`) is a literal of frozen values (`shape`) that every source
    /// uses only as its shape allows (`Shape::allows`), its module's
    /// uses included (`owner_need`: an alias, `const_get(name)`, an
    /// evaluated String may reach it); else records why not.
    fn unchanged(full: &str, path: &str, at: usize, shape: Option<Shape>) -> Option<()> {
        let Some(shape) = shape else {
            return refuse(NOT_LITERAL, format!("`{full}`: {}", describe_at(path, at)));
        };
        let name = full.rsplit("::").next().unwrap_or(full);
        if let Some(site) = sites_of(name).iter().find(|s| !shape.allows(s.need) && relevant(s, full)) {
            return refuse(USED, format!("`{full}`: {}", describe_at(&site.path, site.at)));
        }
        if let Some((owner, _)) = full.rsplit_once("::") {
            if let Some(site) = owner_reached(owner, shape, 0) {
                return refuse(USED, format!("`{full}`, through its module `{owner}`: {}", describe_at(&site.path, site.at)));
            }
        }
        let changed = |e: &Effect| !shape.allows(e.need) && e.named.as_deref().is_none_or(|n| n == name);
        // `mod.const_set(:ALL, …)` binds a constant of the module found.
        let found = |e: &Effect| match (&e.need, &e.named) {
            (Need::Rebind, Some(_)) => full.rsplit_once("::").map_or("Object", |(owner, _)| owner).to_string(),
            _ => full.to_string(),
        };
        if let Some((path, at)) = computed_change(changed, found) {
            return refuse(
                USED,
                format!("`{full}`: a constant looked up by a name not known before it runs is changed at {}", describe_at(&path, at)),
            );
        }
        Some(())
    }

    /// A use of the module `owner` that may reach a constant of it past
    /// what `shape` allows: by a name not known before it runs, a String
    /// evaluated, the module passed on. An alias (`KA = Kinds`, assigned
    /// once with certainty) counts by its own uses.
    fn owner_reached(owner: &str, shape: Shape, depth: usize) -> Option<Site> {
        let key = (owner.to_string(), shape, depth);
        let pure = !asking();
        if let Some(found) = pure.then(|| with_memo(|m| m.owner.get(&key).cloned())).flatten() {
            return found;
        }
        let found = owner_reached_uncached(owner, shape, depth);
        if pure {
            with_memo(|m| m.owner.insert(key, found.clone()));
        }
        found
    }

    fn owner_reached_uncached(owner: &str, shape: Shape, depth: usize) -> Option<Site> {
        let last = owner.rsplit("::").next().unwrap_or(owner);
        for site in sites_of(last).iter().filter(|s| relevant(s, owner)) {
            let alias = site.alias.as_ref().filter(|a| depth < MAX_ALIASES && matches!(find(a), Found::Value(d) if d.path == site.path && d.start == site.at));
            if let Some(alias) = alias {
                if let Some(found) = owner_reached(alias, shape, depth + 1) {
                    return Some(found);
                }
                continue;
            }
            if !shape.allows(owner_need(site)) {
                return Some(site.clone());
            }
        }
        None
    }

    /// How many aliases of an alias `owner_reached` follows.
    const MAX_ALIASES: usize = 4;

    /// Every mention of `name` in the registered sources and in the test
    /// sources not registered now.
    fn sites_of(name: &str) -> Rc<Vec<Site>> {
        if let Some(sites) = with_memo(|m| m.sites.get(name).cloned()) {
            return sites;
        }
        let late = late_sites(name);
        let sites = with_entry(name, |e| {
            if late.is_empty() {
                e.sites.clone()
            } else {
                let mut all = Vec::with_capacity(e.sites.len() + late.len());
                all.extend(e.sites.iter().cloned());
                all.extend(late);
                Rc::new(all)
            }
        });
        with_memo(|m| m.sites.insert(name.to_string(), sites.clone()));
        sites
    }

    /// What a mention of a module lets code do to the constants in it: a
    /// name not known before it runs (`const_get(name)`, `send(name)`), a
    /// String it evaluates, the module bound or passed on (where any of
    /// those may follow) reach each of them.
    fn owner_need(site: &Site) -> Need {
        match &site.used {
            Use::Read | Use::Mixin | Use::Adds => Need::Always,
            Use::Assigned | Use::Escapes => Need::Object,
            Use::Call { block, first } => match (site.method.as_str(), first) {
                ("const_get" | "const_source_location", Some(None)) => Need::Object,
                ("const_set" | "remove_const" | "send" | "public_send" | "__send__", Some(None)) => Need::Rebind,
                (m, Some(_)) if EVALS.contains(&m) => Need::Rebind,
                (m, None) if EVALS.contains(&m) && !block => Need::Rebind,
                _ => Need::Always,
            },
        }
    }

    /// Is the test class body constant `name`, assigned `value` by the
    /// statement at `at` of `file` (where `env` is), a literal of frozen
    /// values every source uses only as its shape allows? Records the
    /// refusal.
    pub(super) fn local_unchanged(env: &Env, file: &str, name: &str, value: &Node<'_>, at: usize) -> bool {
        let class = env.cx.as_ref().and_then(|cx| cx.local.as_ref()).map(|l| l.class.clone());
        let full = match &class {
            Some(class) => format!("{class}::{name}"),
            None => format!("?::{name}"),
        };
        let env = Env { cx: env.cx.as_ref().map(|cx| Rc::new(cx.before(at))), ..env.clone() };
        let shape = quietly(|| shape(value, &env));
        unchanged(&full, file, value.location().start_offset(), shape).is_some()
    }

    /// Where a source mentions a constant's name.
    #[derive(Clone, Debug)]
    struct Site {
        path: String,
        at: usize,
        /// The path written before it (`A::B::` → `["A", "B"]`); `None`
        /// when that is not a constant path (`self::X`, `x::X`, a name
        /// `const_get` looks up from `self`, a String evaluated).
        qualifier: Option<Vec<String>>,
        rooted: bool,
        /// The lexical scopes where it is written (full names, outer
        /// first); `None` when they are not certain (`class << self`, a
        /// compact `module A::B` inside another scope).
        cref: Option<Vec<String>>,
        need: Need,
        /// The method called on it (`each`, `<<`, `include`), if any.
        method: String,
        /// What it does with a module of the name.
        used: Use,
        /// `KA = Kinds`: the full name of the constant it is the value of,
        /// where the lexical scopes are certain.
        alias: Option<String>,
        /// A mixin (`Use::Mixin`) into a module not written there: a
        /// receiver not known (`base.extend(M)`), a call in a method or a
        /// block, or `include`/`prepend` into a module (whose instance
        /// methods a module that extends it gets) or into a receiver.
        /// Not `extend M` in a body, or `include M` in a class body.
        open_mixin: bool,
        /// A mixin by `include`/`prepend` (into instances, not `extend`).
        includes: bool,
        /// The superclass of a `class` (`class X < M`), whose constants
        /// `X::NAME` reaches.
        superclass: bool,
    }

    /// Where a mention of a module (or class) puts it.
    #[derive(Clone, Debug, PartialEq)]
    enum Use {
        /// `M::X` read, `defined?(M)`, `x.is_a?(M)`, `when M`, `rescue M`,
        /// a superclass, a Sorbet type, a statement whose value is unused.
        Read,
        /// `include M`, `extend M`, `prepend M`.
        Mixin,
        /// A constant declared or written in it: `class M::X`, `M::X = 1`,
        /// `M::X ||= 1`, `M::X, y = …`.
        Adds,
        /// The receiver of `Site::method`: with a block, and its first
        /// argument (`None` for none; `Some(None)` when not a literal
        /// Symbol or String).
        Call { block: bool, first: Option<Option<String>> },
        /// Assigned to a constant or a variable: an alias.
        Assigned,
        /// Passed on, returned, an element of a collection, anything else.
        Escapes,
    }

    /// How sure a use of a module must be to leave it as its declarations
    /// say (`changed_from_outside`).
    #[derive(Clone, Copy, PartialEq)]
    pub(in super::super) enum Reach {
        /// Its ancestors: only a call that may reopen it (`REOPENERS`) or
        /// an alias (through which `class Alias` reopens it) counts; a
        /// class is called and passed on in many ways that do not.
        Ancestry,
        /// The methods it gives: any use but a read, a mixin, a constant
        /// declared in it or a call of `MODULE_READS` counts.
        Methods,
        /// Its constants: a constant declared in it from outside counts too.
        Constants,
    }

    /// May this use of a module change it, for `reach`?
    fn changes(site: &Site, reach: Reach) -> bool {
        match (&site.used, reach) {
            (Use::Read | Use::Mixin, _) => false,
            (Use::Assigned, _) => true,
            (Use::Adds, r) => r == Reach::Constants,
            // `remove_const(:M)` of it, or a use not listed.
            (Use::Escapes, r) => r != Reach::Ancestry || site.need == Need::Rebind,
            (Use::Call { .. }, Reach::Ancestry) => REOPENERS.contains(&site.method.as_str()),
            (Use::Call { block, .. }, _) => *block || !MODULE_READS.contains(&site.method.as_str()),
        }
    }

    /// Calls on a module that only read it.
    const MODULE_READS: &[&str] = &[
        "name", "to_s", "inspect", "constants", "const_get", "const_defined?", "const_source_location", "instance_methods",
        "public_instance_methods", "instance_method", "public_instance_method", "method_defined?",
        "public_method_defined?", "private_method_defined?", "ancestors", "included_modules", "include?", "is_a?",
        "kind_of?", "instance_of?", "respond_to?", "==", "!=", "===", "<", "<=", ">", ">=", "equal?", "eql?", "hash",
        "frozen?", "module_parent", "module_parent_name",
    ];

    /// Calls that run code they are given (a String, or a block in the
    /// receiver's context).
    const EVALS: &[&str] =
        &["eval", "class_eval", "module_eval", "instance_eval", "class_exec", "module_exec", "instance_exec"];

    /// Methods that change a module from outside its body.
    const REOPENERS: &[&str] = &[
        "include", "prepend", "extend", "send", "public_send", "__send__", "class_eval", "module_eval", "class_exec",
        "module_exec", "instance_eval", "instance_exec", "const_set", "remove_const", "class_methods", "included",
        "prepended", "extended", "define_method", "define_singleton_method", "alias_method", "remove_method",
        "undef_method", "singleton_class", "attr_accessor", "attr_writer", "attr_reader", "private_constant",
        "instance_variable_set", "append_features", "prepend_features", "module_function", "private_class_method",
    ];

    /// The loops a test class body declares tests with.
    const LOOPS: &[&str] = &["each", "each_with_index", "each_pair"];

    /// Calls that return a new collection of the receiver's elements (or
    /// of what a block that is given them returns).
    const FRESH: &[&str] = &["keys", "values", "map", "collect", "flat_map", "filter_map", "select", "filter", "reject"];

    /// Calls that return one of the receiver's elements.
    const ELEMENT: &[&str] = &["first", "last", "[]", "fetch", "dig", "find", "detect"];

    /// Calls that return no part of the receiver (with no block, or one
    /// that only reads).
    const TERMINAL: &[&str] = &[
        "to_sentence", "join", "include?", "key?", "has_key?", "member?", "size", "length", "count", "empty?", "any?",
        "none?", "all?", "frozen?", "blank?", "present?", "to_s", "inspect",
    ];

    /// Calls a block may make and still only read what it is given (any
    /// predicate, `…?`, too).
    const READS: &[&str] = &[
        "to_s", "inspect", "to_sym", "to_i", "to_f", "[]", "fetch", "dig", "first", "last", "size", "length", "count",
        "join", "keys", "values", "==", "!=", "!", "eql?", "equal?", "<", ">", "<=", ">=", "<=>", "===", "=~", "+", "-",
        "*", "/", "%", "upcase", "downcase", "capitalize", "swapcase", "humanize", "titleize", "underscore", "camelize",
        "demodulize", "dasherize", "parameterize", "pluralize", "singularize", "tr", "gsub", "sub", "strip", "squish",
        "name", "to_param", "presence", "class",
    ];

    fn reads(name: &[u8]) -> bool {
        std::str::from_utf8(name).is_ok_and(|n| READS.contains(&n) || (n.ends_with('?') && !n.contains('!')))
    }

    fn span_of(node: &Node<'_>) -> (usize, usize) {
        let loc = node.location();
        (loc.start_offset(), loc.end_offset())
    }

    /// Does `node` (an expression, or a block or block argument) only
    /// read what it is given: literals, locals, constants, `READS` calls
    /// on them with no block but `&:<read>`, locals bound to such?
    fn reads_only(node: &Node<'_>) -> bool {
        use ruby_prism::Visit;
        if let Some(arg) = node.as_block_argument_node() {
            return arg.expression().and_then(|e| e.as_symbol_node().map(|s| reads(s.unescaped()))) == Some(true);
        }
        if let Some(block) = node.as_block_node() {
            return block.body().is_none_or(|b| reads_only(&b));
        }
        fn allowed(node: &Node<'_>) -> bool {
            if let Some(call) = node.as_call_node() {
                return call.receiver().is_some()
                    && reads(call.name().as_slice())
                    && call.block().is_none_or(|b| b.as_block_argument_node().is_some() && reads_only(&b));
            }
            matches!(
                node,
                Node::ArgumentsNode { .. }
                    | Node::BlockArgumentNode { .. }
                    | Node::SymbolNode { .. }
                    | Node::StringNode { .. }
                    | Node::InterpolatedStringNode { .. }
                    | Node::InterpolatedSymbolNode { .. }
                    | Node::EmbeddedStatementsNode { .. }
                    | Node::StatementsNode { .. }
                    | Node::ParenthesesNode { .. }
                    | Node::IntegerNode { .. }
                    | Node::FloatNode { .. }
                    | Node::RationalNode { .. }
                    | Node::ImaginaryNode { .. }
                    | Node::NilNode { .. }
                    | Node::TrueNode { .. }
                    | Node::FalseNode { .. }
                    | Node::SelfNode { .. }
                    | Node::LocalVariableReadNode { .. }
                    | Node::ItLocalVariableReadNode { .. }
                    | Node::LocalVariableWriteNode { .. }
                    | Node::InstanceVariableReadNode { .. }
                    | Node::ConstantReadNode { .. }
                    | Node::ConstantPathNode { .. }
                    | Node::ArrayNode { .. }
                    | Node::HashNode { .. }
                    | Node::KeywordHashNode { .. }
                    | Node::AssocNode { .. }
                    | Node::RangeNode { .. }
                    | Node::IfNode { .. }
                    | Node::UnlessNode { .. }
                    | Node::ElseNode { .. }
                    | Node::AndNode { .. }
                    | Node::OrNode { .. }
                    | Node::RegularExpressionNode { .. }
            )
        }
        struct Scan {
            ok: bool,
        }
        impl<'pr> Visit<'pr> for Scan {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                self.ok &= allowed(&node);
            }
            fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
                self.ok &= allowed(&node);
            }
        }
        let mut scan = Scan { ok: allowed(node) };
        scan.visit(node);
        scan.ok
    }

    /// `each`, `each_with_index` or `each_pair` with a block that only
    /// declares tests: such loops (over what only reads), locals bound to
    /// what only reads, and `test` calls whose arguments only read (their
    /// blocks run once every test is declared).
    fn loop_like(call: &ruby_prism::CallNode<'_>) -> bool {
        if !LOOPS.contains(&constant_id_str(&call.name())) {
            return false;
        }
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else { return false };
        block.body().map(flatten_statements).unwrap_or_default().iter().all(|stmt| {
            if let Some(inner) = stmt.as_call_node() {
                if inner.receiver().is_none() && inner.name().as_slice() == b"test" {
                    return inner.arguments().is_none_or(|a| a.arguments().iter().all(|a| reads_only(&a)));
                }
                return loop_like(&inner) && inner.receiver().is_some_and(|r| reads_only(&r));
            }
            stmt.as_local_variable_write_node().is_some_and(|w| reads_only(&w.value()))
        })
    }

    /// Is `parents[i]` a `loop_like` call written as a statement of a
    /// class body, or of the block of another such loop?
    fn loop_itself(parents: &[Node<'_>], i: usize) -> bool {
        if !parents[i].as_call_node().is_some_and(|c| loop_like(&c)) || i < 2 || parents[i - 1].as_statements_node().is_none() {
            return false;
        }
        let holder = &parents[i - 2];
        holder.as_class_node().is_some() || (holder.as_block_node().is_some() && i >= 3 && loop_itself(parents, i - 3))
    }

    /// What the mention `node` (inside `parents`, innermost last) may do
    /// to the constant's value, and the method called on it.
    fn classify(parents: &[Node<'_>], node: &Node<'_>) -> (Need, String) {
        let mention = span_of(node);
        let mut span = mention;
        let mut need = Need::Always;
        // A new collection of its elements, not the object itself.
        let mut fresh = false;
        let mut method = String::new();
        let mut i = parents.len();
        while i > 0 {
            i -= 1;
            let parent = &parents[i];
            let escape = if fresh { Need::Elems } else { Need::Object };
            if let Some(call) = parent.as_call_node() {
                if !call.receiver().is_some_and(|r| span_of(&r) == span) {
                    // An argument, a block argument (`&ALL`).
                    return (need.max(escape), method);
                }
                let name = constant_id_str(&call.name());
                if span == mention {
                    method = name.to_string();
                }
                let block = call.block();
                let only_reads = block.as_ref().is_none_or(reads_only);
                let plain = call.arguments().is_none();
                if LOOPS.contains(&name) && block.is_some() {
                    // Its value (the receiver) is a statement's, unused.
                    if loop_itself(parents, i) {
                        return (need, method);
                    }
                    if !only_reads {
                        need = need.max(Need::Elems);
                    }
                } else if name == "freeze" && plain && block.is_none() {
                    // It returns the receiver.
                } else if FRESH.contains(&name) && plain && matches!(name, "keys" | "values") == block.is_none() {
                    if !only_reads {
                        need = need.max(Need::Elems);
                    }
                    fresh = true;
                } else if ELEMENT.contains(&name) {
                    return (need.max(Need::Elems), method);
                } else if TERMINAL.contains(&name) && only_reads {
                    return (need, method);
                } else {
                    return (need.max(escape), method);
                }
                span = span_of(parent);
                continue;
            }
            if let Some(stmts) = parent.as_statements_node() {
                let last = stmts.body().iter().last().is_some_and(|s| span_of(&s) == span);
                let Some(holder) = i.checked_sub(1).map(|j| &parents[j]) else { return (need, method) };
                if last && (holder.as_parentheses_node().is_some() || holder.as_begin_node().is_some()) {
                    i -= 1;
                    span = span_of(holder);
                    continue;
                }
                // Its value unused, or interpolated (`to_s`).
                if !last
                    || holder.as_class_node().is_some()
                    || holder.as_module_node().is_some()
                    || holder.as_singleton_class_node().is_some()
                    || holder.as_program_node().is_some()
                    || holder.as_embedded_statements_node().is_some()
                {
                    return (need, method);
                }
                // The value of a method, a block, a branch.
                return (need.max(escape), method);
            }
            if parent.as_parentheses_node().is_some() {
                span = span_of(parent);
                continue;
            }
            let read = parent.as_constant_path_node().is_some_and(|p| p.parent().is_some_and(|p| span_of(&p) == span))
                || parent.as_defined_node().is_some()
                || parent.as_if_node().is_some_and(|c| span_of(&c.predicate()) == span)
                || parent.as_unless_node().is_some_and(|c| span_of(&c.predicate()) == span);
            if read {
                return (need, method);
            }
            if parent.as_splat_node().is_some() {
                return (need.max(Need::Elems), method);
            }
            if span == mention && parent.as_singleton_class_node().is_some() {
                method = "singleton_class".into();
            }
            // Passed on, bound, returned, an element or a key, an operand.
            return (need.max(escape), method);
        }
        (need, method)
    }

    /// A Symbol's or String's text.
    fn literal_text(node: &Node<'_>) -> Option<String> {
        let bytes = node.as_symbol_node().map(|s| s.unescaped().to_vec()).or_else(|| node.as_string_node().map(|s| s.unescaped().to_vec()))?;
        String::from_utf8(bytes).ok()
    }

    /// Every place `text` mentions the constant `name`, names it in a
    /// `const_get`/`const_set`/`remove_const` (through `send` too) or a
    /// literal String's `constantize`, or has it in a String it evaluates
    /// (`module_eval("…")`), with what the mention may do to its value
    /// and, for a module, where it puts it (`module_use`). A name not
    /// known before the code runs (`Mod.const_get(name)`) is counted at
    /// its module (`owner_need`), and by `scan_wide`.
    fn scan_uses(text: &str, path: &str, name: &str) -> Vec<Site> {
        if !text.contains(name) {
            return Vec::new();
        }
        scan_text(text, path, Some(name)).0
    }

    /// What `text` does that no constant's name tells (`Wide`).
    fn scan_wide(text: &str, path: &str) -> Wide {
        scan_text(text, path, None).1
    }

    /// What a source does that no constant's name tells: where it may
    /// define a method `Mod.constants` answers with (`REFLECTION`), and
    /// where it looks a constant up by a name not known before it runs
    /// and changes what it finds.
    #[derive(Clone, Debug, Default)]
    struct Wide {
        /// `def constants`, `alias`/`define_method`/`delegate` … of one
        /// (`WIDE_NAMES`), where it puts the method (`Definee`); a String
        /// evaluated that has the name, `Module` (or a module `EXTENDS`
        /// lists) reopened by anything but plain definitions, or changed
        /// by a call (`Module.prepend(…)`): `Definee::Any`.
        redefines: Vec<(usize, Definee)>,
        /// `Object.const_get(name)`, `name.constantize`,
        /// `names.map(&:constantize)`, what is done to what it finds
        /// there (`computed_need`), and what it may find (`Origin`).
        lookups: Vec<(usize, Effect, Origin)>,
    }

    /// Where a definition of a `WIDE_NAMES` method puts it, when that may
    /// be where `Mod.constants` of a module other than its own finds it
    /// (`definee`). Not a module's own singleton (`def self.constants`,
    /// `class << self`, `define_singleton_method` in its body, `def
    /// Mod.constants`), which its own checks see (`body_constants`, a use
    /// of `Mod` that may change it), nor the top level's (`Object`'s,
    /// which `Module#constants` comes before).
    #[derive(Clone, Debug, PartialEq)]
    enum Definee {
        /// Every module (`Module`, `Class`, a module `EXTENDS` lists), or
        /// one not known here: in a block that may be run in another
        /// module (`included do`, `Module.new do`), by a call in a method
        /// (whose `self` may be a module that extends it, or one found by
        /// a name not known before it runs), on a receiver not known; or
        /// a visibility change (`private_class_method :constants`), which
        /// a module's body check takes as declaring nothing.
        Any,
        /// The instance methods of the module (or class) of this name:
        /// those of a module that extends it, which counts where it may be
        /// mixed into a module not written there, and a class's, where it
        /// may be a `Module` subclass (`methods_reach`).
        Methods(String),
    }

    /// What a definition of a `WIDE_NAMES` method defines it for, before
    /// `definee` tells where that is.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Defines {
        /// `def constants`, `alias constants all`: the module the code is
        /// written in (through any `def` around it).
        Lexical,
        /// `define_method(:constants)`, `alias_method`, `attr_reader`,
        /// `delegate`, `def_delegator`, `remove_method`: `self`'s
        /// instance methods.
        Instance,
        /// `define_singleton_method(:constants)`, `def self.constants`,
        /// `def_single_delegator`, a stub: `self`'s own.
        Singleton,
    }

    /// The `Defines` of a call that defines (or changes) a method it is
    /// given the name of; `Some(None)` for a visibility change
    /// (`Definee::Any`); `None` for a call that defines none.
    fn definer(method: &str) -> Option<Option<Defines>> {
        match method {
            "define_method" | "alias_method" | "attr_reader" | "attr_writer" | "attr_accessor" | "attr" | "delegate"
            | "def_delegator" | "def_delegators" | "def_instance_delegator" | "def_instance_delegators"
            | "instance_delegate" | "remove_method" | "undef_method" | "stub_any_instance" => Some(Some(Defines::Instance)),
            "define_singleton_method" | "def_single_delegator" | "def_single_delegators" | "single_delegate" | "stubs"
            | "stub" | "expects" => Some(Some(Defines::Singleton)),
            "module_function" | "public" | "private" | "protected" | "public_class_method" | "private_class_method" => {
                Some(None)
            }
            _ => None,
        }
    }

    /// Calls whose block runs with the `self` around it, as it is written.
    const SELF_KEEPING: &[&str] = &[
        "each", "each_with_index", "each_with_object", "each_pair", "each_key", "each_value", "map", "flat_map",
        "collect", "select", "filter", "filter_map", "reject", "times", "upto", "downto", "step", "inject", "reduce",
        "zip", "tap",
    ];

    /// What a constant looked up by a name not known before it runs may
    /// be. `Object.const_get("A::#{x}")` (and `constantize`, which runs
    /// it) looks `A` up at the top level and each later name in the
    /// module before it and that module's ancestors, never at the top
    /// level again: it finds a constant under `A` (`reaches`).
    #[derive(Clone, Debug, PartialEq)]
    enum Origin {
        /// Any constant.
        Any,
        /// A String with one of these heads.
        Heads(Vec<Head>),
        /// What a method of this name called on `self` returns, where the
        /// class's body defines it: every definition of the name in the
        /// sources (`helper_heads`).
        Helper(String),
    }

    /// The literal start of a constant path a String names: the path
    /// itself (`"GraphApi"`), or, when `open`, a module under which a
    /// path not known follows (`"GraphApi::#{x}"`).
    #[derive(Clone, Debug, PartialEq)]
    struct Head {
        path: Vec<String>,
        open: bool,
    }

    /// The methods a source defining again anywhere may make a module's
    /// `constants` other than its body shows: `constants` itself, which
    /// a loop over `Mod.constants` calls, and `const_get`, which reaches
    /// its constants by name. (`const_defined?` and
    /// `const_source_location` answer neither.)
    const WIDE_NAMES: &[&str] = &["constants", "const_get"];

    /// A statement of a `Module` (or `EXTENDS` module) body reopened, or
    /// of a `refine Module` block, that defines only methods it names:
    /// `def`, `alias`, `alias_method`/`attr_*` of literal names, `private`
    /// and the like (`declaration_call`).
    fn names_its_methods(stmt: &Node<'_>) -> bool {
        if stmt.as_def_node().is_some() || stmt.as_alias_method_node().is_some() || declaration_call(stmt) {
            return true;
        }
        let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && c.block().is_none()) else { return false };
        matches!(constant_id_str(&call.name()), "alias_method" | "attr_reader" | "attr_writer" | "attr_accessor")
            && call.arguments().is_some_and(|a| a.arguments().iter().all(|a| literal_text(&a).is_some()))
    }

    /// Is `full` `Module` (or `Class`, whose `Object.const_get` a
    /// `constantize` runs), or a module a module body may `extend`
    /// (`EXTENDS`): one whose methods every module (or one that extends
    /// it) answers with?
    fn every_modules(full: &str) -> bool {
        matches!(full, "Module" | "Class") || (full != "self" && EXTENDS.contains(&full))
    }

    /// Calls of these change an Array, Hash, Set or String in place, or
    /// may (on any object): what a collection constant found by a name
    /// not known before it runs may be given. `mutates` names more (a
    /// `!` method, a setter, `include`): a collection has none of those
    /// but the ones listed here, so a call of one on it raises instead.
    /// `instance_variable_set` gives it a variable its elements are not.
    fn changes_collection(name: &str) -> bool {
        const BANGS: &[&str] = &[
            "map!", "collect!", "select!", "filter!", "reject!", "uniq!", "compact!", "flatten!", "sort!", "sort_by!",
            "shuffle!", "reverse!", "rotate!", "slice!", "merge!", "transform_keys!", "transform_values!", "gsub!", "sub!",
            "upcase!", "downcase!", "capitalize!", "swapcase!", "squeeze!", "strip!", "lstrip!", "rstrip!", "chomp!",
            "chop!", "succ!", "next!", "tr!", "tr_s!", "delete!", "encode!", "unicode_normalize!", "scrub!",
            "delete_prefix!", "delete_suffix!", "squish!",
        ];
        const NOT_COLLECTION: &[&str] = &[
            "class_eval", "module_eval", "class_exec", "module_exec", "define_method", "include", "const_set",
            "remove_const", "instance_variable_set", "singleton_class", "freeze",
        ];
        if name.ends_with('!') && name != "!" {
            return BANGS.contains(&name);
        }
        if name != "[]=" && name.ends_with('=') && name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') {
            return false;
        }
        super::mutates(name) && !NOT_COLLECTION.contains(&name)
    }

    /// Calls of these may give a module a constant or take one away.
    const CHANGES_MODULE: &[&str] = &[
        "const_set", "remove_const", "autoload", "include", "prepend", "class_eval", "module_eval", "class_exec",
        "module_exec", "instance_eval", "instance_exec", "send", "public_send", "__send__",
    ];

    /// Calls that, given a block, return what they are called on.
    const RETURNS_RECEIVER: &[&str] = &["each", "each_with_index", "each_pair", "each_key", "each_value", "reverse_each", "tap"];

    /// Calls on a `singleton_class` that only read it.
    const SINGLETON_READS: &[&str] =
        &["ancestors", "instance_methods", "public_instance_methods", "private_instance_methods", "instance_method", "name", "inspect", "to_s"];

    /// The method `call` runs and its arguments: `send(:x, a)`'s `x` and
    /// `a`; `None` for `send(name, …)`.
    fn sent<'pr>(call: &ruby_prism::CallNode<'pr>) -> Option<(String, Vec<Node<'pr>>)> {
        let name = constant_id_str(&call.name()).to_string();
        let mut args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if !matches!(name.as_str(), "send" | "public_send" | "__send__" | "try" | "try!") {
            return Some((name, args));
        }
        if args.is_empty() {
            return Some((name, args));
        }
        let sent = literal_text(&args.remove(0))?;
        Some((sent, args))
    }

    /// Does the block `block` change what it is given in place (by
    /// `super::closure_changes` of its parameters)? A block whose
    /// parameters are not plain names (`_1`, `it`, `*rest`): unless it
    /// only reads (`reads_only`).
    fn block_changes(block: &Node<'_>) -> bool {
        if let Some(arg) = block.as_block_argument_node() {
            return arg.expression().and_then(|e| e.as_symbol_node().map(|s| super::mutates(&String::from_utf8_lossy(s.unescaped())))) != Some(false);
        }
        let Some(node) = block.as_block_node() else { return true };
        let Some(params) = super::block_params(&node) else { return !reads_only(block) };
        if params.is_empty() {
            return node.parameters().is_some() && !reads_only(block);
        }
        let mut names = Vec::new();
        params.iter().for_each(|p| p.names(&mut names));
        node.body().is_some_and(|b| super::changed_in_place(&b, &names).is_some())
    }

    /// What code may do to a constant it finds by a name not known before
    /// it runs.
    #[derive(Clone, Debug, PartialEq)]
    struct Effect {
        /// To a collection: in place (`Need::Object`), to its elements
        /// (`Need::Elems`), bind a constant in it again (`Need::Rebind`).
        need: Need,
        /// A module may get or lose a constant.
        module: bool,
        /// For `Need::Rebind`, the constants so bound: those of this name
        /// (`mod.const_set(:ALL, …)`, `mod::ALL = …`).
        named: Option<String>,
    }

    impl Effect {
        fn new(need: Need, module: bool) -> Effect {
            Effect { need, module, named: None }
        }
    }

    /// Where a call on a constant found by a name not known before it
    /// runs leaves the walk (`computed_need`).
    enum Step {
        /// At a part of it: an element (`first`, `[]`, `fetch`), its
        /// `singleton_class` (true), or itself (`freeze`, `itself`).
        Part(Need, bool),
        /// Done: what the call may do to it, or nothing.
        Done(Option<Effect>),
    }

    /// `call` on what is at `need` (`Need::Object` the constant found,
    /// `Need::Elems` an element of it, `Need::Always` a new collection of
    /// the constants found, as `names.map(&:constantize)` returns), its
    /// `singleton_class` when `singleton`.
    fn step(call: &ruby_prism::CallNode<'_>, need: Need, singleton: bool) -> Step {
        let fresh = need == Need::Always;
        let Some((name, args)) = sent(call) else {
            // `x.send(name)`: any method.
            return Step::Done((!fresh).then(|| Effect::new(need, true)));
        };
        let name = name.as_str();
        if singleton {
            let reads = (name.ends_with('?') && !name.ends_with("!?")) || SINGLETON_READS.contains(&name);
            return Step::Done((!fresh && !reads).then(|| Effect::new(need, false)));
        }
        if name == "singleton_class" {
            return Step::Part(need, true);
        }
        if ELEMENT.contains(&name) {
            return Step::Part(if fresh { Need::Object } else { Need::Elems }, false);
        }
        if matches!(name, "freeze" | "itself") {
            return Step::Part(need, false);
        }
        let changes = call.block().is_some_and(|b| block_changes(&b));
        // `each { … }` and `tap { … }` return what they are called on.
        if !changes && call.block().is_some_and(|b| b.as_block_node().is_some()) && RETURNS_RECEIVER.contains(&name) {
            return Step::Part(need, false);
        }
        if fresh {
            // A block given the constants found.
            return Step::Done(changes.then(|| Effect::new(Need::Object, true)));
        }
        if matches!(name, "const_set" | "remove_const") {
            // A constant of that name bound again or removed; one whose
            // name is not known before it runs is not followed (a module
            // that may get or lose a constant, all the same).
            let named = args.first().and_then(literal_text);
            let need = if named.is_some() { Need::Rebind } else { Need::Always };
            return Step::Done(Some(Effect { need, module: true, named }));
        }
        let module = CHANGES_MODULE.contains(&name);
        let collection = if changes_collection(name) { need } else { Need::Always };
        if module || collection > Need::Always {
            return Step::Done(Some(Effect::new(collection, module)));
        }
        if changes {
            // `tap { |x| … }` is given the object; a loop, its elements.
            return Step::Done(Some(if matches!(name, "tap" | "then" | "yield_self") { Effect::new(need, true) } else { Effect::new(Need::Elems, false) }));
        }
        Step::Done(None)
    }

    /// `x::NAME = …` of a module found by a name not known before it runs.
    fn path_written(target: &ruby_prism::ConstantPathNode<'_>) -> Effect {
        let named = target.name().map(|n| constant_id_str(&n).to_string());
        Effect { need: Need::Rebind, module: true, named }
    }

    /// What code may do to a constant it looks up by a name not known
    /// before it runs (`node`, `Object.const_get(name)`; at `start`, as
    /// `step` tells): change it in place by calls on it (`<<`, `push`,
    /// `const_set`, a block that changes what it is given, `x::B = …`),
    /// or an element of it through `first`/`[]`/…, or its
    /// `singleton_class`, or the same through a local it is assigned to
    /// in the same scope (`local_effect`). A read, a new value, the value
    /// passed on, returned or kept elsewhere: nothing; those are not
    /// followed.
    fn computed_need(parents: &[Node<'_>], node: &Node<'_>, start: Need) -> Vec<Effect> {
        let mut span = span_of(node);
        let (mut need, mut singleton) = (start, false);
        let mut i = parents.len();
        while i > 0 {
            i -= 1;
            let parent = &parents[i];
            if let Some(call) = parent.as_call_node() {
                if !call.receiver().is_some_and(|r| span_of(&r) == span) {
                    return Vec::new();
                }
                match step(&call, need, singleton) {
                    Step::Part(n, s) => (need, singleton) = (n, s),
                    Step::Done(effect) => return effect.into_iter().collect(),
                }
                span = span_of(parent);
                continue;
            }
            if let Some(stmts) = parent.as_statements_node() {
                let last = stmts.body().iter().last().is_some_and(|s| span_of(&s) == span);
                let holder = i.checked_sub(1).map(|j| &parents[j]);
                if last && holder.is_some_and(|h| h.as_parentheses_node().is_some() || h.as_begin_node().is_some_and(|b| b.rescue_clause().is_none())) {
                    i -= 1;
                    span = span_of(&parents[i]);
                    continue;
                }
                // The value of a branch: `c ? Object.const_get(n) : nil`,
                // `if …; Object.const_get(n); else …; end`.
                let branch = |j: usize| parents[j].as_if_node().is_some() || parents[j].as_unless_node().is_some();
                if last && holder.is_some_and(|h| h.as_else_node().is_some()) && i >= 2 && branch(i - 2) {
                    i -= 2;
                    span = span_of(&parents[i]);
                    continue;
                }
                if last && i >= 1 && branch(i - 1) {
                    i -= 1;
                    span = span_of(&parents[i]);
                    continue;
                }
                return Vec::new();
            }
            // An `elsif`'s value is its `if`'s.
            if parent.as_if_node().is_some_and(|f| f.subsequent().is_some_and(|s| span_of(&s) == span)) {
                span = span_of(parent);
                continue;
            }
            // `Object.const_get(n) || []`, `c && Object.const_get(n)`.
            let operand = parent.as_or_node().map(|o| (o.left(), o.right())).or_else(|| parent.as_and_node().map(|a| (a.left(), a.right())));
            if operand.is_some_and(|(l, r)| span_of(&l) == span || span_of(&r) == span) {
                span = span_of(parent);
                continue;
            }
            if let Some(path) = parent.as_constant_path_node() {
                let whole = span_of(parent);
                let target = path_target(&parents[..i]).filter(|t| span_of(&t.as_node()) == whole);
                let written = path.parent().is_some_and(|p| span_of(&p) == span) && target.is_some();
                return if written && need > Need::Always && !singleton { vec![path_written(&path)] } else { Vec::new() };
            }
            let written = parent
                .as_local_variable_write_node()
                .filter(|w| span_of(&w.value()) == span)
                .map(|w| w.name())
                .or_else(|| parent.as_local_variable_or_write_node().filter(|w| span_of(&w.value()) == span).map(|w| w.name()));
            if let Some(local) = written {
                let scope = parents[..i].iter().rev().find(|p| {
                    matches!(
                        p,
                        Node::DefNode { .. }
                            | Node::BlockNode { .. }
                            | Node::LambdaNode { .. }
                            | Node::ClassNode { .. }
                            | Node::ModuleNode { .. }
                            | Node::SingletonClassNode { .. }
                            | Node::ProgramNode { .. }
                    )
                });
                let local = constant_id_str(&local).to_string();
                return scope.map_or_else(Vec::new, |scope| local_effect(scope, &local, (need, singleton)));
            }
            return Vec::new();
        }
        Vec::new()
    }

    /// The constant path the innermost of `parents` writes, when it is a
    /// `X::B = …` (`||=`, `&&=`, `+=`).
    fn path_target<'pr>(parents: &[Node<'pr>]) -> Option<ruby_prism::ConstantPathNode<'pr>> {
        parents.last().and_then(written_path)
    }

    /// The constant path a `X::B = …` (`||=`, `&&=`, `+=`) writes.
    fn written_path<'pr>(node: &Node<'pr>) -> Option<ruby_prism::ConstantPathNode<'pr>> {
        node.as_constant_path_write_node()
            .map(|w| w.target())
            .or_else(|| node.as_constant_path_or_write_node().map(|w| w.target()))
            .or_else(|| node.as_constant_path_and_write_node().map(|w| w.target()))
            .or_else(|| node.as_constant_path_operator_write_node().map(|w| w.target()))
    }

    /// What `scope` does to the object its local `name` holds (at
    /// `start`) by calls on it or on a part of it, `name::B = …`, as
    /// `step` tells. Anything else (the local passed on, bound again) is
    /// not followed.
    fn local_effect(scope: &Node<'_>, name: &str, start: (Need, bool)) -> Vec<Effect> {
        use ruby_prism::Visit;
        /// Where `node`, a chain of `Step::Part` calls from the local, is.
        fn reached(node: &Node<'_>, name: &str, start: (Need, bool)) -> Option<(Need, bool)> {
            if node.as_local_variable_read_node().is_some_and(|r| constant_id_str(&r.name()) == name) {
                return Some(start);
            }
            if let Some(call) = node.as_call_node() {
                let (need, singleton) = reached(&call.receiver()?, name, start)?;
                return match step(&call, need, singleton) {
                    Step::Part(n, s) => Some((n, s)),
                    Step::Done(_) => None,
                };
            }
            let parens = node.as_parentheses_node()?;
            match parens.body().map(flatten_statements).unwrap_or_default().as_slice() {
                [only] => reached(only, name, start),
                _ => None,
            }
        }
        struct Scan<'a> {
            name: &'a str,
            start: (Need, bool),
            out: Vec<Effect>,
        }
        impl<'pr> Visit<'pr> for Scan<'_> {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                if let Some(call) = node.as_call_node() {
                    if let Some((need, singleton)) = call.receiver().and_then(|r| reached(&r, self.name, self.start)) {
                        if let Step::Done(Some(effect)) = step(&call, need, singleton) {
                            self.out.push(effect);
                        }
                    }
                }
                if let Some(target) = written_path(&node) {
                    if let Some((need, false)) = target.parent().and_then(|p| reached(&p, self.name, self.start)) {
                        if need > Need::Always {
                            self.out.push(path_written(&target));
                        }
                    }
                }
            }
        }
        let mut scan = Scan { name, start, out: Vec::new() };
        scan.visit(scope);
        scan.out
    }

    fn scan_text(text: &str, path: &str, name: Option<&str>) -> (Vec<Site>, Wide) {
        use ruby_prism::Visit;
        struct Scan<'a, 'pr> {
            name: &'a str,
            path: &'a str,
            /// Looking for what `Wide` lists, not for `name`.
            wide: bool,
            found: Wide,
            /// The nodes around the one visited, outermost first (each
            /// statement list once, whichever way it is visited).
            parents: Vec<Node<'pr>>,
            pushed: Vec<bool>,
            /// The `class`/`module` scopes entered: (full name, whether
            /// Ruby certainly gives it that name, where its body starts).
            scopes: Vec<(String, bool, usize)>,
            scoped: Vec<bool>,
            out: Vec<Site>,
        }
        impl<'pr> Scan<'_, 'pr> {
            /// The lexical scopes at `at`: a `class`'s name and superclass
            /// are read outside it.
            fn cref(&self, at: usize) -> Option<Vec<String>> {
                let mut scopes = self.scopes.as_slice();
                if scopes.last().is_some_and(|s| at < s.2) {
                    scopes = &scopes[..scopes.len() - 1];
                }
                scopes.iter().all(|s| s.1).then(|| scopes.iter().map(|s| s.0.clone()).collect())
            }
            fn push(&mut self, at: usize, qualifier: Option<Vec<String>>, rooted: bool, need: Need, method: String, used: Use) {
                let cref = if qualifier.is_some() { self.cref(at) } else { None };
                self.out.push(Site {
                    path: self.path.to_string(),
                    at,
                    qualifier,
                    rooted,
                    cref,
                    need,
                    method,
                    used,
                    alias: None,
                    open_mixin: false,
                    includes: false,
                    superclass: false,
                });
            }
            fn mention(&mut self, node: &Node<'pr>, qualifier: Option<Vec<String>>, rooted: bool) {
                let (need, method) = classify(&self.parents, node);
                let used = module_use(&self.parents, node);
                let at = node.location().start_offset();
                let alias = self
                    .parents
                    .last()
                    .and_then(|p| p.as_constant_write_node())
                    .filter(|w| span_of(&w.value()) == span_of(node))
                    .and_then(|w| {
                        let name = constant_id_str(&w.name());
                        Some(match self.cref(at)?.last() {
                            Some(scope) => format!("{scope}::{name}"),
                            None => name.to_string(),
                        })
                    });
                let open_mixin = used == Use::Mixin && self.open_mixin();
                let includes = used == Use::Mixin
                    && self.parents.len().checked_sub(2).and_then(|i| self.parents[i].as_call_node()).is_some_and(|c| constant_id_str(&c.name()) != "extend");
                let superclass = self
                    .parents
                    .last()
                    .and_then(|p| p.as_class_node())
                    .is_some_and(|c| c.superclass().is_some_and(|s| span_of(&s) == span_of(node)));
                self.push(at, qualifier, rooted, need, method, used);
                if let Some(site) = self.out.last_mut() {
                    site.alias = alias;
                    site.open_mixin = open_mixin;
                    site.includes = includes;
                    site.superclass = superclass;
                }
            }
            /// For a mention given to `include`/`extend`/`prepend` (the
            /// innermost of `parents` its arguments): is the module it is
            /// mixed into one not written there (`Site::open_mixin`)?
            fn open_mixin(&self) -> bool {
                let n = self.parents.len();
                let Some(call) = n.checked_sub(2).and_then(|i| self.parents[i].as_call_node()) else { return true };
                let instance = constant_id_str(&call.name()) != "extend";
                match call.receiver() {
                    // `Y.extend(M)`: a use of `Y` its own uses see.
                    Some(r) if segments(&r).is_some() => instance,
                    Some(r) if r.as_self_node().is_none() => true,
                    _ => {
                        let body = n >= 4
                            && self.parents[n - 3].as_statements_node().is_some()
                            && (self.parents[n - 4].as_class_node().is_some() || self.parents[n - 4].as_module_node().is_some());
                        !body || (instance && self.parents[n - 4].as_module_node().is_some())
                    }
                }
            }
            /// Where a definition `kind` written inside the innermost
            /// `depth` of `parents` puts the method (`Definee`).
            fn definee(&self, depth: usize, kind: Defines) -> Option<Definee> {
                let parents = &self.parents[..depth];
                let scope = |n: &Node<'_>| matches!(n, Node::ModuleNode { .. } | Node::ClassNode { .. } | Node::SingletonClassNode { .. });
                for j in (0..parents.len()).rev() {
                    let p = &parents[j];
                    match p {
                        // A nested `def` defines where the one around it is.
                        Node::DefNode { .. } if kind == Defines::Lexical => {}
                        Node::DefNode { .. } | Node::LambdaNode { .. } => return Some(Definee::Any),
                        Node::BlockNode { .. } => {
                            let call = j.checked_sub(1).and_then(|k| parents[k].as_call_node());
                            if !call.is_some_and(|c| c.receiver().is_some() && SELF_KEEPING.contains(&constant_id_str(&c.name()))) {
                                return Some(Definee::Any);
                            }
                        }
                        Node::SingletonClassNode { .. } => {
                            let expression = p.as_singleton_class_node()?.expression();
                            let in_body = expression.as_self_node().is_some()
                                && j >= 2
                                && parents[j - 1].as_statements_node().is_some()
                                && matches!(parents[j - 2], Node::ModuleNode { .. } | Node::ClassNode { .. });
                            // `class << self` in a body: the module's own;
                            // `class << Mod`: a use of `Mod` its checks see.
                            return if in_body || segments(&expression).is_some() { None } else { Some(Definee::Any) };
                        }
                        Node::ModuleNode { .. } | Node::ClassNode { .. } => {
                            let k = parents[..=j].iter().filter(|n| scope(n)).count();
                            let Some((full, exact, _)) = k.checked_sub(1).and_then(|k| self.scopes.get(k)) else {
                                return Some(Definee::Any);
                            };
                            if !exact || every_modules(full) {
                                return Some(Definee::Any);
                            }
                            return (kind != Defines::Singleton).then(|| Definee::Methods(full.clone()));
                        }
                        _ => {}
                    }
                }
                None
            }
            /// What the lookup `call` (running `method` with `args`) may
            /// find: a String with a literal head given to
            /// `Object.const_get`, `constantize` (`"GraphApi::#{x}".constantize`,
            /// `ActiveSupport::Inflector.constantize(…)`), or one a
            /// same-class method returns (`helper`); else any constant.
            /// `Mod.const_get` looks its first name up in `Mod` too.
            fn origin(&self, call: &ruby_prism::CallNode<'pr>, method: &str, args: &[Node<'pr>]) -> Origin {
                let receiver = call.receiver();
                let named = |r: &Option<Node<'pr>>, names: &[&[&str]]| {
                    r.as_ref().and_then(segments).is_some_and(|(segs, _)| names.iter().any(|n| segs.iter().map(String::as_str).eq(n.iter().copied())))
                };
                match method {
                    "const_get" if named(&receiver, &[&["Object"]]) => args.first().map_or(Origin::Any, |a| self.origin_of(a)),
                    "constantize" | "safe_constantize" if args.is_empty() => receiver.map_or(Origin::Any, |r| self.origin_of(&r)),
                    "constantize" | "safe_constantize" if named(&receiver, &[&["Inflector"], &["ActiveSupport", "Inflector"]]) => {
                        args.first().map_or(Origin::Any, |a| self.origin_of(a))
                    }
                    _ => Origin::Any,
                }
            }
            /// What the name `arg` a lookup is given may be (`origin`).
            fn origin_of(&self, arg: &Node<'pr>) -> Origin {
                if let Some(parens) = arg.as_parentheses_node() {
                    return match parens.body().map(flatten_statements).unwrap_or_default().as_slice() {
                        [only] => self.origin_of(only),
                        _ => Origin::Any,
                    };
                }
                if let Some(head) = interpolated_head(arg) {
                    return Origin::Heads(vec![head]);
                }
                match arg.as_call_node().and_then(|c| self.helper(&c)) {
                    Some(m) => Origin::Helper(m),
                    None => Origin::Any,
                }
            }
            /// `m(…)` (or `self.m(…)`, no block) called in an instance
            /// method of a `class`/`module` body that defines `m` once
            /// (no receiver): its name, whose definitions `helper_heads`
            /// reads.
            fn helper(&self, call: &ruby_prism::CallNode<'pr>) -> Option<String> {
                const VISIBILITY: &[&str] = &["private", "protected", "public"];
                if call.block().is_some() || call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
                    return None;
                }
                let m = constant_id_str(&call.name()).to_string();
                let j = self.parents.iter().rposition(|p| {
                    matches!(
                        p,
                        Node::DefNode { .. }
                            | Node::BlockNode { .. }
                            | Node::LambdaNode { .. }
                            | Node::ClassNode { .. }
                            | Node::ModuleNode { .. }
                            | Node::SingletonClassNode { .. }
                    )
                })?;
                self.parents[j].as_def_node().filter(|d| d.receiver().is_none())?;
                let visibility = |n: &Node<'_>| {
                    n.as_call_node().is_some_and(|c| c.receiver().is_none() && VISIBILITY.contains(&constant_id_str(&c.name())))
                };
                let mut k = j.checked_sub(1)?;
                if self.parents[k].as_arguments_node().is_some() {
                    if !visibility(self.parents.get(k.checked_sub(1)?)?) {
                        return None;
                    }
                    k = k.checked_sub(2)?;
                }
                self.parents[k].as_statements_node()?;
                let owner = &self.parents[k.checked_sub(1)?];
                let body = owner.as_class_node().map(|c| c.body()).or_else(|| owner.as_module_node().map(|m| m.body()))?;
                let defs = body
                    .map(flatten_statements)
                    .unwrap_or_default()
                    .iter()
                    .filter(|s| {
                        let def = s.as_def_node().or_else(|| {
                            s.as_call_node()
                                .filter(|_| visibility(s))
                                .and_then(|c| c.arguments()?.arguments().iter().next()?.as_def_node())
                        });
                        def.is_some_and(|d| d.receiver().is_none() && constant_id_str(&d.name()) == m)
                    })
                    .count();
                (defs == 1).then_some(m)
            }
            /// A declaration's name (`class Levels::ALL`), not a read.
            fn declared(&self, node: &Node<'pr>) -> bool {
                self.parents.last().is_some_and(|p| {
                    let path = p.as_class_node().map(|c| c.constant_path()).or_else(|| p.as_module_node().map(|m| m.constant_path()));
                    path.is_some_and(|path| span_of(&path) == span_of(node))
                })
            }
            fn named(&mut self, call: &ruby_prism::CallNode<'pr>, node: &Node<'pr>) {
                let mut method = constant_id_str(&call.name()).to_string();
                let mut args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                if EVALS.contains(&method.as_str()) {
                    // The code a String gives `eval` is not parsed here.
                    let named = args.iter().any(|a| string_text(a).is_some_and(|t| has_word(&t, self.name)));
                    if named {
                        self.push(node.location().start_offset(), None, false, Need::Rebind, method, Use::Escapes);
                    }
                    return;
                }
                if matches!(method.as_str(), "send" | "public_send" | "__send__") && !args.is_empty() {
                    let Some(sent) = literal_text(&args[0]) else { return };
                    method = sent;
                    args.remove(0);
                }
                if matches!(method.as_str(), "constantize" | "safe_constantize") {
                    // `"Kinds::ALL".constantize`,
                    // `ActiveSupport::Inflector.constantize("Kinds::ALL")`:
                    // `Object.const_get` of the path, from the top level.
                    let text = match args.first() {
                        Some(arg) => literal_text(arg),
                        None => call.receiver().and_then(|r| r.as_string_node().and_then(|s| String::from_utf8(s.unescaped().to_vec()).ok())),
                    };
                    let Some(text) = text else { return };
                    let text = text.trim_start_matches("::");
                    let (prefix, last) = text.rsplit_once("::").unwrap_or(("", text));
                    if last != self.name {
                        return;
                    }
                    let qualifier = if prefix.is_empty() { Vec::new() } else { prefix.split("::").map(str::to_string).collect() };
                    let (need, method) = classify(&self.parents, node);
                    let used = module_use(&self.parents, node);
                    self.push(node.location().start_offset(), Some(qualifier), true, need, method, used);
                    return;
                }
                if !matches!(method.as_str(), "const_get" | "const_set" | "remove_const") {
                    return;
                }
                let Some(text) = args.first().and_then(literal_text) else { return };
                let (prefix, last) = match text.rsplit_once("::") {
                    Some((prefix, last)) => (Some(prefix.to_string()), last.to_string()),
                    None => (None, text),
                };
                if last != self.name {
                    return;
                }
                // With no receiver, `self`'s constant: not a lexical one.
                let (qualifier, rooted) = match (prefix, call.receiver()) {
                    (Some(prefix), Some(r)) if segments(&r).is_some_and(|(s, _)| s == ["Object"]) => {
                        (Some(prefix.trim_start_matches("::").split("::").map(str::to_string).collect()), true)
                    }
                    (Some(_), _) | (None, None) => (None, false),
                    (None, Some(r)) => match segments(&r) {
                        Some((segs, rooted)) => (Some(segs), rooted),
                        None => (None, false),
                    },
                };
                if method == "const_get" {
                    let (need, method) = classify(&self.parents, node);
                    self.push(node.location().start_offset(), qualifier, rooted, need, method, Use::Escapes);
                } else {
                    self.push(node.location().start_offset(), qualifier, rooted, Need::Rebind, method, Use::Escapes);
                }
            }
            /// The full name of the module `class`/`module` `path` opens
            /// here, when certain.
            fn opened(&self, path: &Node<'pr>) -> Option<String> {
                let (segs, rooted) = segments(path)?;
                let joined = segs.join("::");
                match self.scopes.last() {
                    _ if rooted => Some(joined),
                    None => Some(joined),
                    Some((outer, true, _)) if self.scopes.iter().all(|s| s.1) => Some(format!("{outer}::{joined}")),
                    Some(_) => None,
                }
            }
            /// `Wide`'s part of `node`.
            fn wide_node(&mut self, node: &Node<'pr>) {
                let at = node.location().start_offset();
                let depth = self.parents.len();
                if let Some(def) = node.as_def_node() {
                    if WIDE_NAMES.contains(&constant_id_str(&def.name())) {
                        let definee = match def.receiver() {
                            None => self.definee(depth, Defines::Lexical),
                            Some(r) if r.as_self_node().is_some() => self.definee(depth, Defines::Singleton),
                            // `def Mod.constants`: a use of `Mod` its checks see.
                            Some(r) if segments(&r).is_some() => None,
                            Some(_) => Some(Definee::Any),
                        };
                        self.found.redefines.extend(definee.map(|d| (at, d)));
                    }
                    return;
                }
                if let Some(alias) = node.as_alias_method_node() {
                    // `alias constants all`; an interpolated name is not known.
                    let definee = match literal_text(&alias.new_name()) {
                        Some(n) if !WIDE_NAMES.contains(&n.as_str()) => None,
                        Some(_) => self.definee(depth, Defines::Lexical),
                        None => Some(Definee::Any),
                    };
                    self.found.redefines.extend(definee.map(|d| (at, d)));
                    return;
                }
                let opened = node
                    .as_class_node()
                    .map(|c| (c.constant_path(), c.body()))
                    .or_else(|| node.as_module_node().map(|m| (m.constant_path(), m.body())));
                if let Some((path, body)) = opened {
                    if self.opened(&path).is_some_and(|full| every_modules(&full)) {
                        if let Some(stmt) = body.map(flatten_statements).unwrap_or_default().iter().find(|s| !names_its_methods(s)) {
                            self.found.redefines.push((stmt.location().start_offset(), Definee::Any));
                        }
                    }
                    return;
                }
                let Some(call) = node.as_call_node() else { return };
                let mut method = constant_id_str(&call.name()).to_string();
                let mut args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                if matches!(method.as_str(), "send" | "public_send" | "__send__" | "try" | "try!") && !args.is_empty() {
                    if let Some(sent) = literal_text(&args[0]) {
                        method = sent;
                        args.remove(0);
                    }
                }
                let every = |n: &Node<'_>| segments(n).is_some_and(|(segs, _)| every_modules(&segs.join("::")));
                if call.receiver().is_some_and(|r| every(&r)) && REOPENERS.contains(&method.as_str()) {
                    self.found.redefines.push((at, Definee::Any));
                }
                if method == "refine" && args.first().is_some_and(every) {
                    let body = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body());
                    if body.map(flatten_statements).unwrap_or_default().iter().any(|s| !names_its_methods(s)) {
                        self.found.redefines.push((at, Definee::Any));
                    }
                }
                let spoken = |a: &Node<'_>| string_text(a).is_some_and(|t| WIDE_NAMES.iter().any(|m| has_word(&t, m)));
                if EVALS.contains(&method.as_str()) && args.iter().any(spoken) {
                    self.found.redefines.push((at, Definee::Any));
                }
                let computed = match method.as_str() {
                    "const_get" => args.first().is_some_and(|a| literal_text(a).is_none()),
                    "constantize" | "safe_constantize" => match args.first() {
                        Some(a) => literal_text(a).is_none(),
                        None => call.receiver().is_some_and(|r| r.as_string_node().is_none()),
                    },
                    _ => false,
                };
                if computed {
                    let origin = self.origin(&call, &method, &args);
                    let effects = computed_need(&self.parents, node, Need::Object);
                    self.found.lookups.extend(effects.into_iter().map(|e| (at, e, origin.clone())));
                }
                // `names.map(&:constantize)`: the constants found, in a new
                // collection.
                let by_symbol = call.block().and_then(|b| b.as_block_argument_node()).and_then(|b| b.expression()).and_then(|e| literal_text(&e));
                if by_symbol.is_some_and(|m| matches!(m.as_str(), "constantize" | "safe_constantize" | "const_get")) {
                    let effects = computed_need(&self.parents, node, Need::Always);
                    self.found.lookups.extend(effects.into_iter().map(|e| (at, e, Origin::Any)));
                }
            }
            /// `:constants` (or `:const_get`, `WIDE_NAMES`, or a String of
            /// one) given to a call that defines a method by name
            /// (`definer`: `define_method`, `alias_method :constants, …`,
            /// `delegate`, a stub run as the file loads, `send(:define_method,
            /// :constants)`), where it puts it (`definee`); or an element of
            /// an Array, which code may give one (`Definee::Any`). Not a
            /// Symbol code only returns, keeps or passes to another call
            /// (`def category = :constants`, `respond_to?(:constants)`,
            /// `send(:const_get, :X)`), a Hash key, a `when`, an alias's
            /// names (`wide_node` counts the alias), a stub's in a test's
            /// code, which is undone before the next test and runs after
            /// every class body. A name such a call is given by a
            /// variable (`names.each { |n| define_method(n) }`) is not
            /// followed.
            fn wide_name(&mut self, node: &Node<'pr>) {
                const STUBS: &[&str] = &["stubs", "stub", "expects", "stub_any_instance"];
                if !literal_text(node).is_some_and(|t| WIDE_NAMES.contains(&t.as_str())) {
                    return;
                }
                let at = node.location().start_offset();
                let span = span_of(node);
                let depth = self.parents.len();
                let parent = self.parents.last();
                if parent.is_some_and(|p| {
                    p.as_assoc_node().is_some_and(|a| span_of(&a.key()) == span)
                        || p.as_when_node().is_some()
                        || p.as_alias_method_node().is_some()
                }) {
                    return;
                }
                if parent.is_some_and(|p| p.as_array_node().is_some()) {
                    self.found.redefines.push((at, Definee::Any));
                    return;
                }
                let call = match self.parents.as_slice() {
                    [.., call, args] if args.as_arguments_node().is_some() => call.as_call_node(),
                    _ => None,
                };
                let Some(call) = call else { return };
                let args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                let first = args.first().is_some_and(|a| span_of(a) == span);
                let mut method = constant_id_str(&call.name()).to_string();
                if matches!(method.as_str(), "send" | "public_send" | "__send__" | "try" | "try!") {
                    // `send(:const_get, :X)` calls it.
                    match args.first().and_then(literal_text) {
                        Some(sent) if !first => method = sent,
                        _ => return,
                    }
                }
                // `alias_method :new, :constants` defines `new`.
                if method == "alias_method" && !first {
                    return;
                }
                let in_code = self.parents.iter().any(|p| matches!(p, Node::DefNode { .. } | Node::BlockNode { .. } | Node::LambdaNode { .. }));
                if STUBS.contains(&method.as_str()) && in_code {
                    return;
                }
                let Some(kind) = definer(&method) else { return };
                let Some(kind) = kind else {
                    self.found.redefines.push((at, Definee::Any));
                    return;
                };
                let definee = match call.receiver() {
                    None => self.definee(depth - 2, kind),
                    Some(r) if r.as_self_node().is_some() => self.definee(depth - 2, kind),
                    Some(r) => match segments(&r) {
                        Some((segs, _)) if every_modules(&segs.join("::")) => Some(Definee::Any),
                        // `Mod.define_singleton_method(:constants)`: a use of
                        // `Mod` its checks see.
                        Some(_) if kind == Defines::Singleton => None,
                        // `Mod.define_method(:constants)`, where `Mod` is the
                        // module of that name.
                        Some((segs, rooted)) if rooted || self.cref(at).is_some_and(|c| c.is_empty()) => {
                            Some(Definee::Methods(segs.join("::")))
                        }
                        _ => Some(Definee::Any),
                    },
                };
                self.found.redefines.extend(definee.map(|d| (at, d)));
            }
            /// `X, ALL = …`, `rescue => ALL`: the constant bound again.
            fn target(&mut self, node: &Node<'pr>) {
                let at = node.location().start_offset();
                if node.as_constant_target_node().is_some_and(|t| constant_id_str(&t.name()) == self.name) {
                    self.push(at, Some(Vec::new()), false, Need::Rebind, String::new(), Use::Assigned);
                }
                if let Some(t) = node.as_constant_path_target_node().filter(|t| t.name().is_some_and(|n| constant_id_str(&n) == self.name)) {
                    let (qualifier, rooted) = match t.parent() {
                        None => (Some(Vec::new()), true),
                        Some(p) => segments(&p).map_or((None, false), |(segs, rooted)| (Some(segs), rooted)),
                    };
                    self.push(at, qualifier, rooted, Need::Rebind, String::new(), Use::Assigned);
                }
            }
        }
        /// Nodes prism also visits as a typed field (`CallNode#arguments`,
        /// `ConstantPathWriteNode#target`), not through
        /// `visit_branch_node_enter`: each is pushed by its own `visit_…`.
        fn typed(node: &Node<'_>) -> bool {
            matches!(
                node,
                Node::StatementsNode { .. }
                    | Node::ArgumentsNode { .. }
                    | Node::BlockArgumentNode { .. }
                    | Node::BlockNode { .. }
                    | Node::ConstantPathNode { .. }
                    | Node::ElseNode { .. }
                    | Node::EnsureNode { .. }
                    | Node::RescueNode { .. }
                    | Node::SplatNode { .. }
                    | Node::ParametersNode { .. }
            )
        }
        impl<'pr> Scan<'_, 'pr> {
            fn constant_path(&mut self, path: &ruby_prism::ConstantPathNode<'pr>) {
                let node = path.as_node();
                // `Mod::NAME = …` is a declaration (`Walk`), not a use.
                let span = span_of(&node);
                let written = self.parents.last().is_some_and(|w| {
                    let target = |t: Option<ruby_prism::ConstantPathNode<'_>>| t.is_some_and(|t| span_of(&t.as_node()) == span);
                    target(w.as_constant_path_write_node().map(|w| w.target()))
                        || target(w.as_constant_path_or_write_node().map(|w| w.target()))
                        || target(w.as_constant_path_and_write_node().map(|w| w.target()))
                        || target(w.as_constant_path_operator_write_node().map(|w| w.target()))
                });
                if path.name().is_some_and(|n| constant_id_str(&n) == self.name) && !self.declared(&node) && !written {
                    let (qualifier, rooted) = match path.parent() {
                        None => (Some(Vec::new()), true),
                        Some(p) => match segments(&p) {
                            Some((segs, rooted)) => (Some(segs), rooted),
                            None => (None, false),
                        },
                    };
                    self.mention(&node, qualifier, rooted);
                }
            }
        }
        macro_rules! pushed {
            ($($visit:ident: $ty:ident),* $(,)?) => {
                $(fn $visit(&mut self, node: &ruby_prism::$ty<'pr>) {
                    self.parents.push(node.as_node());
                    ruby_prism::$visit(self, node);
                    self.parents.pop();
                })*
            };
        }
        impl<'pr> Visit<'pr> for Scan<'_, 'pr> {
            pushed!(
                visit_statements_node: StatementsNode,
                visit_arguments_node: ArgumentsNode,
                visit_block_argument_node: BlockArgumentNode,
                visit_block_node: BlockNode,
                visit_else_node: ElseNode,
                visit_ensure_node: EnsureNode,
                visit_rescue_node: RescueNode,
                visit_splat_node: SplatNode,
                visit_parameters_node: ParametersNode,
            );
            fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
                self.constant_path(node);
                self.parents.push(node.as_node());
                ruby_prism::visit_constant_path_node(self, node);
                self.parents.pop();
            }
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                if self.wide {
                    self.wide_node(&node);
                }
                if let Some(call) = node.as_call_node() {
                    self.named(&call, &node);
                } else {
                    self.target(&node);
                }
                let scope = node
                    .as_module_node()
                    .map(|m| (Some(m.constant_path()), m.body()))
                    .or_else(|| node.as_class_node().map(|c| (Some(c.constant_path()), c.body())))
                    .or_else(|| node.as_singleton_class_node().map(|c| (None, c.body())));
                let scoped = scope.is_some();
                if let Some((path, body)) = scope {
                    let outer = self.scopes.last();
                    let (name, exact) = match path.as_ref().and_then(segments) {
                        Some((segs, rooted)) => {
                            let joined = segs.join("::");
                            match outer {
                                _ if rooted => (joined, true),
                                None => (joined, true),
                                Some((outer, exact, _)) => (format!("{outer}::{joined}"), *exact && segs.len() == 1),
                            }
                        }
                        // `class << self` has its own lookup.
                        None => (String::from("#<Class>"), false),
                    };
                    let body_at = body.map_or(node.location().end_offset(), |b| b.location().start_offset());
                    self.scopes.push((name, exact, body_at));
                }
                self.scoped.push(scoped);
                // A typed field's node is pushed by its `visit_…`, which
                // visits it however it is reached.
                let push = !typed(&node);
                if push {
                    self.parents.push(node);
                }
                self.pushed.push(push);
            }
            fn visit_branch_node_leave(&mut self) {
                if self.pushed.pop() == Some(true) {
                    self.parents.pop();
                }
                if self.scoped.pop() == Some(true) {
                    self.scopes.pop();
                }
            }
            fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
                if self.wide {
                    self.wide_name(&node);
                }
                if node.as_constant_read_node().is_some_and(|r| constant_id_str(&r.name()) == self.name) && !self.declared(&node) {
                    self.mention(&node, Some(Vec::new()), false);
                }
                self.target(&node);
            }
        }
        let result = ruby_prism::parse(text.as_bytes());
        let mut scan = Scan {
            // No constant has this name.
            name: name.unwrap_or("\0"),
            path,
            wide: name.is_none(),
            found: Wide::default(),
            parents: Vec::new(),
            pushed: Vec::new(),
            scopes: Vec::new(),
            scoped: Vec::new(),
            out: Vec::new(),
        };
        scan.visit(&result.node());
        (scan.out, scan.found)
    }

    /// The text of the String `node` (a heredoc's body too), with the
    /// source of what it interpolates.
    fn string_text(node: &Node<'_>) -> Option<String> {
        if let Some(s) = node.as_string_node() {
            return Some(String::from_utf8_lossy(s.unescaped()).into_owned());
        }
        let parts = node.as_interpolated_string_node()?.parts();
        let mut out = String::new();
        for part in parts.iter() {
            match string_text(&part) {
                Some(t) => out.push_str(&t),
                None => out.push_str(&String::from_utf8_lossy(part.location().as_slice())),
            }
            out.push(' ');
        }
        Some(out)
    }

    /// Does `text` have `word` with no identifier character beside it?
    fn has_word(text: &str, word: &str) -> bool {
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let bytes = text.as_bytes();
        text.match_indices(word).any(|(at, _)| {
            let end = at + word.len();
            !(at > 0 && ident(bytes[at - 1])) && !(end < bytes.len() && ident(bytes[end]))
        })
    }

    /// Calls whose arguments a module is compared or tested against.
    const TESTS: &[&str] = &[
        "is_a?", "kind_of?", "instance_of?", "===", "==", "!=", "<", "<=", ">", ">=", "equal?", "eql?", "include?",
        "assert_kind_of", "assert_instance_of", "refute_kind_of", "refute_instance_of", "assert_equal", "refute_equal",
        "assert_same", "refute_same",
    ];

    /// Where the mention `node` (inside `parents`, innermost last) puts a
    /// module of its name.
    fn module_use(parents: &[Node<'_>], node: &Node<'_>) -> Use {
        let span = span_of(node);
        let Some((parent, outer)) = parents.split_last() else { return Use::Read };
        if sorbet_type(parents, span) {
            return Use::Read;
        }
        if let Some(path) = parent.as_constant_path_node() {
            if path.parent().is_some_and(|p| span_of(&p) == span) {
                let whole = span_of(parent);
                let target = |t: Option<ruby_prism::ConstantPathNode<'_>>| t.is_some_and(|t| span_of(&t.as_node()) == whole);
                let declared = outer.last().is_some_and(|g| {
                    g.as_class_node().is_some_and(|c| span_of(&c.constant_path()) == whole)
                        || g.as_module_node().is_some_and(|m| span_of(&m.constant_path()) == whole)
                        || target(g.as_constant_path_write_node().map(|w| w.target()))
                        || target(g.as_constant_path_or_write_node().map(|w| w.target()))
                        || target(g.as_constant_path_and_write_node().map(|w| w.target()))
                        || target(g.as_constant_path_operator_write_node().map(|w| w.target()))
                });
                return if declared { Use::Adds } else { Use::Read };
            }
        }
        if parent.as_constant_path_target_node().is_some() {
            return Use::Adds;
        }
        if let Some(call) = parent.as_call_node() {
            if call.receiver().is_some_and(|r| span_of(&r) == span) {
                let first = call.arguments().and_then(|a| a.arguments().iter().next()).map(|a| literal_text(&a));
                return Use::Call { block: call.block().is_some_and(|b| b.as_block_node().is_some()), first };
            }
            return Use::Escapes;
        }
        if parent.as_arguments_node().is_some() {
            if let Some(call) = outer.last().and_then(|g| g.as_call_node()) {
                let name = constant_id_str(&call.name());
                if matches!(name, "include" | "extend" | "prepend") {
                    return Use::Mixin;
                }
                if TESTS.contains(&name) {
                    return Use::Read;
                }
            }
            return Use::Escapes;
        }
        if parent.as_class_node().is_some_and(|c| c.superclass().is_some_and(|s| span_of(&s) == span))
            || parent.as_defined_node().is_some()
            || parent.as_when_node().is_some()
            || parent.as_rescue_node().is_some()
        {
            return Use::Read;
        }
        if let Some(stmts) = parent.as_statements_node() {
            let last = stmts.body().iter().last().is_some_and(|s| span_of(&s) == span);
            let unused = outer.last().is_none_or(|h| {
                h.as_class_node().is_some()
                    || h.as_module_node().is_some()
                    || h.as_singleton_class_node().is_some()
                    || h.as_program_node().is_some()
                    || h.as_embedded_statements_node().is_some()
            });
            return if !last || unused { Use::Read } else { Use::Escapes };
        }
        let multi = parent.as_array_node().is_some() && outer.last().is_some_and(|g| g.as_multi_write_node().is_some());
        if multi
            || matches!(
                parent,
                Node::ConstantWriteNode { .. }
                    | Node::ConstantOrWriteNode { .. }
                    | Node::ConstantPathWriteNode { .. }
                    | Node::ConstantPathOrWriteNode { .. }
                    | Node::LocalVariableWriteNode { .. }
                    | Node::LocalVariableOrWriteNode { .. }
                    | Node::InstanceVariableWriteNode { .. }
                    | Node::InstanceVariableOrWriteNode { .. }
                    | Node::ClassVariableWriteNode { .. }
                    | Node::ClassVariableOrWriteNode { .. }
                    | Node::GlobalVariableWriteNode { .. }
                    | Node::GlobalVariableOrWriteNode { .. }
                    | Node::MultiWriteNode { .. }
            )
        {
            return Use::Assigned;
        }
        Use::Escapes
    }

    /// Is the mention at `span` a Sorbet type: in a `sig { … }`, an
    /// argument of `T.nilable(…)`, `T::Array[…]`, the type of `T.let`?
    fn sorbet_type(parents: &[Node<'_>], span: (usize, usize)) -> bool {
        let inside = |n: &Node<'_>| {
            let (start, end) = span_of(n);
            start <= span.0 && span.1 <= end
        };
        let in_sig = parents.iter().any(|p| {
            p.as_call_node().is_some_and(|c| c.receiver().is_none() && c.name().as_slice() == b"sig" && c.block().is_some_and(|b| inside(&b)))
        });
        let [.., call, args] = parents else { return in_sig };
        let Some(call) = call.as_call_node().filter(|_| args.as_arguments_node().is_some()) else { return in_sig };
        let of_t = call.receiver().and_then(|r| segments(&r)).is_some_and(|(s, _)| s.first().map(String::as_str) == Some("T"));
        if !of_t {
            return in_sig;
        }
        // `T.let(value, Type)`: the value is no type.
        let value = matches!(call.name().as_slice(), b"let" | b"cast" | b"bind" | b"must" | b"unsafe" | b"assert_type!")
            && args.as_arguments_node().and_then(|a| a.arguments().iter().next()).is_some_and(|first| inside(&first));
        in_sig || !value
    }

    /// Is the mention `site` possibly of the constant (or module) `full`?
    /// Only one Ruby certainly resolves to another constant, or to none a
    /// source declares, is not (`denotes`). A constant under a first name
    /// no source declares is a gem's or Ruby's, whose other names are not
    /// known (`MiniTest::Test` is `Minitest::Test`): a mention of one of
    /// theirs may be of any other.
    fn relevant(site: &Site, full: &str) -> bool {
        // Inside `mixed_into_object`'s cycle check an answer may be the
        // cautious one a cycle gives, not this site's own.
        let pure = !asking();
        let key = (site.path.clone(), site.at, site.qualifier.clone(), site.rooted, site.cref.clone(), full.to_string());
        if let Some(found) = pure.then(|| with_memo(|m| m.relevant.get(&key).copied())).flatten() {
            return found;
        }
        let found = relevant_uncached(site, full);
        if pure {
            with_memo(|m| m.relevant.insert(key, found));
        }
        found
    }

    fn relevant_uncached(site: &Site, full: &str) -> bool {
        let name = full.rsplit("::").next().unwrap_or(full);
        let theirs = |n: &str| matches!(find(n.split("::").next().unwrap_or(n)), Found::Missing);
        match denotes(site, name) {
            Some(Some(found)) => found == full || (theirs(&found) && theirs(full)),
            Some(None) => theirs(full),
            None => true,
        }
    }

    /// The constant the mention `site` of `name` certainly names, by
    /// Ruby's lookup where it is written: `Some(None)` when no registered
    /// source declares one there; `None` when not certain (lexical scopes
    /// not known, `x::NAME`, an alias such as `KAlias::NAME`, a name
    /// `A::` does not declare itself and so may inherit, a scope whose
    /// ancestors are not known).
    fn denotes(site: &Site, name: &str) -> Option<Option<String>> {
        let qualifier = site.qualifier.as_ref()?;
        // `Object::K`, `Object.const_get(:K)`: the top level's `K`, unless
        // `Object` here is a constant of the app's own.
        let top_object = || match lookup(site, "Object") {
            Some(Resolution::Missing) => true,
            Some(Resolution::Declared(full)) => full == "Object",
            _ => false,
        };
        if qualifier.first().is_some_and(|f| f == "Object") && (site.rooted || top_object()) {
            // A module a source mixes into `Object` has its constants
            // there too (`Object.include(Kinds)`: `Object::ALL` is
            // `Kinds::ALL`).
            let after = qualifier.get(1).map_or(name, String::as_str);
            if name != "Object" && (changed_from_outside("Object", Reach::Ancestry).is_some() || mixed_into_object(after)) {
                return None;
            }
            let top = Site { qualifier: Some(qualifier[1..].to_vec()), rooted: true, ..site.clone() };
            return denotes(&top, name);
        }
        let Some((first, rest)) = qualifier.split_first() else {
            return match (site.rooted, find(name)) {
                (true, Found::Missing) => Some(None),
                (true, Found::Namespace | Found::Value(_)) => Some(Some(name.to_string())),
                (true, Found::Unknown) => None,
                (false, _) => match lookup(site, name)? {
                    Resolution::Declared(full) => Some(Some(full)),
                    Resolution::Missing => Some(None),
                    Resolution::Unknown => None,
                },
            };
        };
        let mut full = if site.rooted {
            match find(first) {
                Found::Missing => return gem_path(first, rest, name),
                _ => first.clone(),
            }
        } else {
            match lookup(site, first)? {
                Resolution::Declared(full) => full,
                Resolution::Missing => return gem_path(first, rest, name),
                Resolution::Unknown => return None,
            }
        };
        for seg in rest.iter().map(String::as_str).chain([name]) {
            // `A::B` is `A`'s own `B`, or one an ancestor of `A` has.
            if !(matches!(find(&full), Found::Namespace) || declared_before(site, &full, true)) {
                return None;
            }
            full = format!("{full}::{seg}");
            if matches!(find(&full), Found::Missing | Found::Unknown) && !declared_before(site, &full, false) {
                return None;
            }
        }
        Some(Some(full))
    }

    thread_local! {
        /// The names `mixed_into_object` is asking about, outermost first.
        static ASKED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// May a module that declares `name` (or an ancestor of one) be mixed
    /// into `Object` where no mention of `Object` shows it: `class
    /// Object` reopened (`include Kinds` in its body); an `include` or
    /// `prepend` of the module whose target is not written there (at the
    /// top level `include Kinds` is `Object`'s, and in a module's body it
    /// reaches what includes that module); the module passed on or
    /// aliased (`send(:include, Kinds)`, `KA = Kinds`)?
    fn mixed_into_object(name: &str) -> bool {
        if !decls("Object").is_empty() || !late_decls("Object").is_empty() {
            return true;
        }
        // A cycle of such questions (`A::B` and `B::A`) is not certain.
        if ASKED.with(|a| a.borrow().iter().any(|n| n == name)) {
            return true;
        }
        let owners: Vec<String> = decls(name)
            .iter()
            .chain(late_decls(name).iter())
            .flat_map(|d| d.names.iter().cloned())
            .filter_map(|n| n.strip_suffix(name)?.strip_suffix("::").map(str::to_string))
            .filter(|owner| !owner.is_empty())
            .collect();
        ASKED.with(|a| a.borrow_mut().push(name.to_string()));
        let mixed = owners.iter().any(|owner| {
            let seg = owner.rsplit("::").next().unwrap_or(owner);
            sites_of(seg).iter().any(|s| {
                relevant(s, owner)
                    && match &s.used {
                        Use::Assigned | Use::Escapes => true,
                        Use::Mixin => s.includes && s.open_mixin,
                        Use::Call { .. } => RETURNS_SELF.contains(&s.method.as_str()),
                        Use::Read | Use::Adds => false,
                    }
            })
        });
        ASKED.with(|a| a.borrow_mut().pop());
        mixed
    }

    /// A path whose first name no registered source declares is through a
    /// gem's or Ruby's module (`FixtureFactory::Registry::ClassMethods`):
    /// it names the constant it spells, one of theirs, unless a source
    /// changes the ancestors of a module along it, whose constants it
    /// then reaches too (`Minitest::Test.include(Helper)` makes
    /// `Minitest::Test::MODES` `Helper::MODES`).
    fn gem_path(first: &str, rest: &[String], name: &str) -> Option<Option<String>> {
        let mut path = first.to_string();
        for seg in rest.iter().map(String::as_str).chain([name]) {
            if changed_from_outside(&path, Reach::Ancestry).is_some() {
                return None;
            }
            path = format!("{path}::{seg}");
        }
        Some(Some(path))
    }

    /// `resolution` of `name` at `site`, where a scope's constant its own
    /// source declares above it unconditionally is certain, though that
    /// source is a test source not registered yet (whose declarations
    /// `find` does not take as certain).
    fn lookup(site: &Site, name: &str) -> Option<Resolution> {
        let cref = site.cref.as_ref()?;
        for scope in cref.iter().rev() {
            let full = format!("{scope}::{name}");
            if declared_before(site, &full, false) {
                return Some(Resolution::Declared(full));
            }
            if !matches!(find(&full), Found::Missing) {
                break;
            }
        }
        Some(resolution(cref, name))
    }

    /// Does the source of `site` declare `full` (a `class`/`module` when
    /// `namespace`) once, unconditionally, above it?
    fn declared_before(site: &Site, full: &str, namespace: bool) -> bool {
        let last = full.rsplit("::").next().unwrap_or(full);
        let all: Vec<Decl> = decls(last).iter().chain(late_decls(last).iter()).filter(|d| d.names.iter().any(|n| n == full)).cloned().collect();
        let [d] = all.as_slice() else { return false };
        d.path == site.path && d.certain && d.names.len() == 1 && d.start < site.at && (!namespace || d.kind != Kind::Const)
    }

    /// The line at `at` of `path` (registered, or a late test source),
    /// and where it is.
    fn describe_at(path: &str, at: usize) -> String {
        let line = any_line_at(path, at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
        let text = with_any_text(path, |t| {
            let at = at.min(t.len());
            let start = t[..at].rfind('\n').map_or(0, |n| n + 1);
            let end = t[at..].find('\n').map_or(t.len(), |n| at + n);
            let line = t[start..end].trim();
            if line.chars().count() > 80 { format!("{}…", line.chars().take(80).collect::<String>()) } else { line.to_string() }
        })
        .unwrap_or_default();
        let late = if is_registered(path) { "" } else { ", a test source not registered before this one" };
        format!("`{text}` ({} line {line}{late})", sources::relative_path(path))
    }

    /// The first use of the module `module` in a registered source (or a
    /// test source not registered yet) that may change it, for `reach`
    /// (`changes`): `Mod.include(X)`, `Mod.class_methods do`, `Alias =
    /// Mod`, and for `Reach::Methods`/`Reach::Constants` any use not
    /// listed.
    pub(in super::super) fn changed_from_outside(module: &str, reach: Reach) -> Option<String> {
        let name = module.rsplit("::").next().unwrap_or(module);
        sites_of(name)
            .iter()
            .find(|s| changes(s, reach) && relevant(s, module))
            .map(|s| format!("may be changed at {}", describe_at(&s.path, s.at)))
    }

    // ---- What no constant's name tells --------------------------------

    /// `Wide` facts of the registered sources (up to `seen`) and of the
    /// late test sources (for `late`'s app root), each with its path:
    /// only the sources that have one.
    #[derive(Default)]
    struct WideCache {
        registry: Option<(String, usize)>,
        seen: u32,
        found: Vec<(String, Wide)>,
        late: Option<(Option<String>, Vec<(String, Wide)>)>,
    }

    thread_local! {
        static WIDE: RefCell<WideCache> = RefCell::new(WideCache::default());
    }

    /// A cheap test before parsing: may `text` have a `Wide` fact?
    fn may_be_wide(text: &str) -> bool {
        const WORDS: &[&str] = &["constants", "const_get", "constantize"];
        WORDS.iter().any(|w| text.contains(w))
            || ["Module", "Class", "refine"].iter().any(|w| has_word(text, w))
            || EXTENDS.iter().filter(|e| **e != "self").any(|e| {
                let seg = e.rsplit("::").next().unwrap_or(e);
                may_declare(text, seg) || text.contains(&format!("{seg}."))
            })
    }

    fn wide_of(text: &str, path: &str) -> Option<Wide> {
        let wide = if may_be_wide(text) { scan_wide(text, path) } else { return None };
        (!wide.redefines.is_empty() || !wide.lookups.is_empty()).then_some(wide)
    }

    /// The first `Wide` fact `pick` chooses, in the registered sources
    /// and the test sources not registered now: its path and offset.
    fn wide_find(pick: impl Fn(&Wide) -> Option<usize>) -> Option<(String, usize)> {
        let registry = registry_key();
        let root = late_root();
        let late_files = WIDE.with(|w| w.borrow().late.as_ref().is_some_and(|(r, _)| *r == root));
        let late = if late_files {
            None
        } else {
            let (files, _) = late_sources(None);
            Some(files.iter().filter_map(|(path, text)| Some((path.clone(), wide_of(text, path)?))).collect::<Vec<_>>())
        };
        // `pick` reads other caches: it runs on a copy.
        let all: Vec<(String, Wide)> = WIDE.with(|w| {
            let mut w = w.borrow_mut();
            if w.registry != registry || (w.seen > 0 && sources::path_of(FileId(w.seen)).is_none()) {
                *w = WideCache { registry: registry.clone(), late: w.late.take(), ..WideCache::default() };
            }
            if let Some(late) = late {
                w.late = Some((root.clone(), late));
            }
            let mut id = w.seen + 1;
            while let Some(path) = sources::path_of(FileId(id)) {
                if let Some(found) = sources::with_text(&path, |text| wide_of(text, &path)).flatten() {
                    w.found.push((path, found));
                }
                w.seen = id;
                id += 1;
            }
            let late = w.late.as_ref().map(|l| l.1.iter().filter(|(path, _)| !is_registered(path)).cloned().collect::<Vec<_>>()).unwrap_or_default();
            w.found.iter().cloned().chain(late).collect()
        });
        all.iter().find_map(|(path, f)| pick(f).map(|at| (path.clone(), at)))
    }

    /// Where a source may define a method `Mod.constants` answers with
    /// again (`Wide::redefines`), where that may reach another module.
    fn redefinition() -> Option<(String, usize)> {
        wide_find(|w| {
            w.redefines
                .iter()
                .find(|(_, definee)| match definee {
                    Definee::Any => true,
                    Definee::Methods(module) => methods_reach(module),
                })
                .map(|(at, _)| *at)
        })
    }

    /// May the instance methods of `module` be methods of a module other
    /// than one whose own uses show it? A module's, where it is mixed in
    /// not as a body's own `extend` or a class body's `include` (a
    /// receiver not known, `include` into a module, which a module may
    /// extend), passed on or aliased, or called in a way that may change
    /// it (`Mod.extend_object(x)`); a class's, unless every declaration
    /// of it is a certain `class` with no superclass (whose instances are
    /// no modules).
    fn methods_reach(module: &str) -> bool {
        if every_modules(module) {
            return true;
        }
        let last = module.rsplit("::").next().unwrap_or(module);
        let all: Vec<Decl> = decls(last).iter().chain(late_decls(last).iter()).filter(|d| d.names.iter().any(|n| n == module)).cloned().collect();
        if !all.is_empty() && all.iter().all(|d| d.kind == Kind::Class) {
            let plain = |d: &Decl| {
                d.certain
                    && d.names.len() == 1
                    && with_any_text(&d.path, |text| {
                        let result = ruby_prism::parse(text.as_bytes());
                        let root = result.node();
                        node_at(&root, d.start, d.end).and_then(|n| n.as_class_node().map(|c| c.superclass().is_none())).unwrap_or(false)
                    })
                    .unwrap_or(false)
            };
            return !all.iter().all(plain);
        }
        sites_of(last).iter().any(|s| {
            relevant(s, module)
                && match &s.used {
                    Use::Assigned | Use::Escapes => true,
                    Use::Mixin => s.open_mixin,
                    Use::Call { .. } => changes(s, Reach::Methods),
                    Use::Read | Use::Adds => false,
                }
        })
    }

    /// Where code changes a constant it looks up by a name not known
    /// before it runs, as `changed` tells from the `Effect`, where what
    /// it finds may be the constant (or module) `found` gives for it.
    fn computed_change(changed: impl Fn(&Effect) -> bool, found: impl Fn(&Effect) -> String) -> Option<(String, usize)> {
        wide_find(|w| w.lookups.iter().find(|(_, effect, origin)| changed(effect) && reaches(origin, &found(effect))).map(|(at, _, _)| *at))
    }

    /// May a lookup of `origin` find the constant `found`? Under a head
    /// (`"GraphApi::#{x}"`) it finds `GraphApi`'s own constants (each
    /// head name a module of its own, `own_module`), and those an ancestor of a
    /// module there has, which may be anything mixed in or a superclass,
    /// or (through a class, whose ancestors `Object` is) of `Kernel` and
    /// what is mixed into `Object`; or anything passed on or aliased
    /// there (`leaks`).
    fn reaches(origin: &Origin, found: &str) -> bool {
        let heads = match origin {
            Origin::Any => return true,
            Origin::Heads(heads) => heads.clone(),
            Origin::Helper(method) => match helper_heads(method) {
                Some(heads) => heads,
                None => return true,
            },
        };
        if found.starts_with('?') || found.starts_with("Kernel::") || found.starts_with("BasicObject::") {
            return true;
        }
        let under = |h: &Head| {
            let uncertain = (1..=h.path.len()).any(|k| !own_module(&h.path[..k].join("::")));
            let joined = h.path.join("::");
            uncertain || if h.open { found.starts_with(&format!("{joined}::")) } else { found == joined }
        };
        heads.iter().any(under) || leaks(found)
    }

    /// Is `full` a module (or class) of its own, not another one bound to
    /// the name: every source that may declare it (a test source not
    /// registered yet too, such as Core's `module GraphApi` around a
    /// test class) opens it with `module`/`class`, none assigns it.
    fn own_module(full: &str) -> bool {
        let last = full.rsplit("::").next().unwrap_or(full);
        let all: Vec<Decl> = decls(last).iter().chain(late_decls(last).iter()).filter(|d| d.names.iter().any(|n| n == full)).cloned().collect();
        !all.is_empty() && all.iter().all(|d| d.kind != Kind::Const)
    }

    /// Calls on a module that return it or give it to code (as a block's
    /// `self` or argument).
    const RETURNS_SELF: &[&str] = &[
        "itself", "tap", "then", "yield_self", "dup", "clone", "freeze", "send", "public_send", "__send__", "try", "try!",
        "instance_eval", "instance_exec", "class_eval", "module_eval", "class_exec", "module_exec",
    ];

    /// May `found` be reached as a constant of another module: it or a
    /// module it is under aliased or passed on (where `const_set` may put
    /// it anywhere), or a module it is under mixed in, a superclass, or
    /// asked for one of its constants (`Mod.const_get(:X)`)?
    fn leaks(found: &str) -> bool {
        let segs: Vec<&str> = found.split("::").collect();
        (1..=segs.len()).any(|k| {
            let prefix = segs[..k].join("::");
            let proper = k < segs.len();
            sites_of(segs[k - 1]).iter().any(|s| {
                relevant(s, &prefix)
                    && match &s.used {
                        Use::Assigned | Use::Escapes => true,
                        Use::Mixin => proper,
                        Use::Read => proper && s.superclass,
                        Use::Call { .. } => RETURNS_SELF.contains(&s.method.as_str()) || (proper && s.method == "const_get"),
                        Use::Adds => false,
                    }
            })
        })
    }

    /// The head of an interpolated String (or Symbol) given to a lookup:
    /// its literal start up to the last `::`, when that is a path of
    /// constant names (`"GraphApi::#{x}"`, `"::A::B#{x}"`).
    fn interpolated_head(node: &Node<'_>) -> Option<Head> {
        let parts: Vec<Node<'_>> = match (node.as_interpolated_string_node(), node.as_interpolated_symbol_node()) {
            (Some(s), _) => s.parts().iter().collect(),
            (_, Some(s)) => s.parts().iter().collect(),
            _ => return None,
        };
        let mut text = String::new();
        for part in &parts {
            match part.as_string_node() {
                Some(s) => text.push_str(&String::from_utf8_lossy(s.unescaped())),
                None => break,
            }
        }
        let text = text.strip_prefix("::").unwrap_or(&text);
        let (head, _) = text.rsplit_once("::")?;
        Some(Head { path: constant_names(head)?, open: true })
    }

    /// `"A::B"` as the names of a constant path, when each is one.
    fn constant_names(text: &str) -> Option<Vec<String>> {
        let text = text.strip_prefix("::").unwrap_or(text);
        let names: Vec<String> = text.split("::").map(str::to_string).collect();
        let valid = |n: &String| {
            n.as_bytes().first().is_some_and(u8::is_ascii_uppercase) && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        };
        names.iter().all(valid).then_some(names)
    }

    /// The heads of what the method `def` returns: every `return`'s and
    /// its last statement's value a String with a head (`"GraphApi"`,
    /// `"GraphApi::#{x}"`), through `if`/`unless` branches and
    /// parentheses. `None` for any other value, a `rescue`/`ensure`
    /// body, or no body.
    fn def_heads(def: &ruby_prism::DefNode<'_>) -> Option<Vec<Head>> {
        use ruby_prism::Visit;
        struct Returns {
            heads: Option<Vec<Head>>,
        }
        impl<'pr> Visit<'pr> for Returns {
            fn visit_return_node(&mut self, node: &ruby_prism::ReturnNode<'pr>) {
                let args: Vec<Node<'pr>> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                let found = match args.as_slice() {
                    [only] => value_heads(only),
                    _ => None,
                };
                match (found, self.heads.as_mut()) {
                    (Some(found), Some(heads)) => heads.extend(found),
                    _ => self.heads = None,
                }
                ruby_prism::visit_return_node(self, node);
            }
            // A nested method's or a lambda's `return` is its own.
            fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}
            fn visit_lambda_node(&mut self, _: &ruby_prism::LambdaNode<'pr>) {}
        }
        let body = def.body()?;
        if body.as_begin_node().is_some() {
            return None;
        }
        let mut returns = Returns { heads: Some(Vec::new()) };
        returns.visit(&body);
        let mut heads = returns.heads?;
        heads.extend(value_heads(&body)?);
        Some(heads)
    }

    /// The heads of the value of `node` (`def_heads`): a `return` gives
    /// none here (its own are counted).
    fn value_heads(node: &Node<'_>) -> Option<Vec<Head>> {
        if let Some(stmts) = node.as_statements_node() {
            return value_heads(&stmts.body().iter().last()?);
        }
        if let Some(parens) = node.as_parentheses_node() {
            return value_heads(&parens.body()?);
        }
        if let Some(s) = node.as_string_node() {
            let path = constant_names(&String::from_utf8_lossy(s.unescaped()))?;
            return Some(vec![Head { path, open: false }]);
        }
        if node.as_interpolated_string_node().is_some() {
            return Some(vec![interpolated_head(node)?]);
        }
        if node.as_return_node().is_some() {
            return Some(Vec::new());
        }
        let branches = |then: Option<ruby_prism::StatementsNode<'_>>, other: Option<Node<'_>>| -> Option<Vec<Head>> {
            let mut heads = value_heads(&then?.as_node())?;
            let other = other?;
            let other = match other.as_else_node() {
                Some(e) => value_heads(&e.statements()?.as_node())?,
                None => value_heads(&other)?,
            };
            heads.extend(other);
            Some(heads)
        };
        if let Some(f) = node.as_if_node() {
            return branches(f.statements(), f.subsequent());
        }
        if let Some(u) = node.as_unless_node() {
            return branches(u.statements(), u.else_clause().map(|e| e.as_node()));
        }
        None
    }

    thread_local! {
        /// `helper_heads` by method name, for a registry, how many sources
        /// it has, and the late sources' root.
        static HELPERS: RefCell<(Option<(Option<(String, usize)>, usize, Option<String>)>, HashMap<String, Option<Vec<Head>>>)> =
            RefCell::new((None, HashMap::new()));
    }

    /// The heads of what a call of `method` on `self` may return: those of
    /// every `def` of the name in the registered sources and the test
    /// sources not registered now (`def_heads`). `None` when one has no
    /// head, or a source names the method otherwise (`define_method(:m)`,
    /// `alias_method :m, …`, `send(:m)`), which may define it again.
    fn helper_heads(method: &str) -> Option<Vec<Head>> {
        use ruby_prism::Visit;
        struct Scan<'a> {
            method: &'a str,
            heads: Option<Vec<Head>>,
            defs: usize,
        }
        impl<'pr> Visit<'pr> for Scan<'_> {
            fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
                if let Some(def) = node.as_def_node().filter(|d| constant_id_str(&d.name()) == self.method) {
                    self.defs += 1;
                    match (def_heads(&def), self.heads.as_mut()) {
                        (Some(found), Some(heads)) => heads.extend(found),
                        _ => self.heads = None,
                    }
                }
            }
            fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
                if literal_text(&node).is_some_and(|t| t == self.method) {
                    self.heads = None;
                }
            }
        }
        let ids = files_with(method, 0);
        let key = (registry_key(), ids.len(), late_root());
        if let Some(found) = HELPERS.with(|h| {
            let h = h.borrow();
            (h.0.as_ref() == Some(&key)).then(|| h.1.get(method).cloned()).flatten()
        }) {
            return found;
        }
        let mut scan = Scan { method, heads: Some(Vec::new()), defs: 0 };
        let mut read = |text: &str| {
            if has_word(text, method) {
                let result = ruby_prism::parse(text.as_bytes());
                scan.visit(&result.node());
            }
        };
        for id in ids {
            if let Some(path) = sources::path_of(FileId(id)) {
                sources::with_text(&path, |text| read(text));
            }
        }
        let (files, late) = late_sources(Some(method));
        for i in late {
            read(&files[i].1);
        }
        let found = if scan.defs == 0 { None } else { scan.heads };
        HELPERS.with(|h| {
            let mut h = h.borrow_mut();
            if h.0.as_ref() != Some(&key) {
                *h = (Some(key), HashMap::new());
            }
            h.1.insert(method.to_string(), found.clone());
        });
        found
    }

    // ---- Test sources not registered yet -----------------------------

    /// The app's test sources as they are on disk (`test/**/*.rb`, its
    /// fixtures aside, and its `test_paths`), by the capital words in
    /// each. App ingest
    /// registers the test files one at a time in path order, so a file
    /// sorted after a test (a support file that changes a constant the
    /// test's loop reads, or reopens an ancestor of its class) is not
    /// registered yet when that test is ingested: an answer such a file
    /// may change is refused, never computed from fewer sources than
    /// the program loads.
    #[derive(Default)]
    struct Late {
        root: Option<String>,
        files: Rc<Vec<(String, String)>>,
        words: HashMap<String, Vec<usize>>,
        decls: HashMap<String, Rc<Vec<Decl>>>,
        sites: HashMap<String, Rc<Vec<Site>>>,
    }

    thread_local! {
        static LATE: RefCell<Late> = RefCell::new(Late::default());
        /// `app_root`'s answer for a registry: (its first path, root).
        static ROOT: RefCell<(Option<String>, Option<String>)> = const { RefCell::new((None, None)) };
    }

    /// The app root registered paths are under (the one
    /// `sources::relative_path` strips), when one is set.
    fn app_root() -> Option<String> {
        let first = sources::path_of(FileId(1));
        if let Some(root) = ROOT.with(|r| {
            let r = r.borrow();
            (r.0 == first && first.is_some()).then(|| r.1.clone())
        }) {
            return root;
        }
        let root = (1..=64u32).map_while(|id| sources::path_of(FileId(id))).find_map(|path| {
            let rel = sources::relative_path(&path);
            (rel != path).then(|| path.strip_suffix(&rel).map(str::to_string)).flatten()
        });
        ROOT.with(|r| *r.borrow_mut() = (first, root.clone()));
        root
    }

    fn is_registered(path: &str) -> bool {
        sources::with_text(path, |_| ()).is_some()
    }

    /// `f` of the late sources (registered since or not), read once per
    /// app root.
    fn with_late<R>(f: impl FnOnce(&mut Late) -> R) -> R {
        let root = app_root();
        LATE.with(|late| {
            let mut late = late.borrow_mut();
            if late.root != root {
                let files = root.as_deref().map(read_test_sources).unwrap_or_default();
                let mut words: HashMap<String, Vec<usize>> = HashMap::new();
                for (at, (_, text)) in files.iter().enumerate() {
                    for word in capital_words(text) {
                        words.entry(word).or_default().push(at);
                    }
                }
                *late = Late { root: root.clone(), files: Rc::new(files), words, ..Late::default() };
            }
            f(&mut late)
        })
    }

    /// Every `.rb` file under `root`'s `test/` but `test/fixtures`, and
    /// under the `test_paths` of its `roundhouse.yml` (Core's
    /// `components/*/test/…`), skipping hidden entries and symlinks, with
    /// its text.
    fn read_test_sources(root: &str) -> Vec<(String, String)> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Config {
            test_paths: Vec<std::path::PathBuf>,
        }
        let root = std::path::Path::new(root);
        let base = root.join("test");
        let fixtures = base.join("fixtures");
        let config: Config = std::fs::read_to_string(root.join("roundhouse.yml"))
            .ok()
            .and_then(|text| serde_yaml_ng::from_str::<Option<Config>>(&text).ok().flatten())
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut todo = vec![base];
        todo.extend(
            config
                .test_paths
                .iter()
                .filter(|p| p.is_relative() && !p.components().any(|c| matches!(c, std::path::Component::ParentDir)))
                .map(|p| root.join(p)),
        );
        while let Some(dir) = todo.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_name().to_string_lossy().starts_with('.') || path == fixtures {
                    continue;
                }
                let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    todo.push(path);
                } else if path.extension().is_some_and(|e| e == "rb") {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        out.push((path.display().to_string(), text));
                    }
                }
            }
        }
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        out
    }

    /// The test sources on disk, and which of them are not registered
    /// now: those that have `word` when it is given.
    pub(in super::super) fn late_sources(word: Option<&str>) -> (Rc<Vec<(String, String)>>, Vec<usize>) {
        with_late(|late| {
            let ids: Vec<usize> = match word {
                Some(w) if w.as_bytes().first().is_some_and(u8::is_ascii_uppercase) => {
                    late.words.get(w).cloned().unwrap_or_default()
                }
                Some(w) => (0..late.files.len()).filter(|i| late.files[*i].1.contains(w)).collect(),
                None => (0..late.files.len()).collect(),
            };
            let ids = ids.into_iter().filter(|i| !is_registered(&late.files[*i].0)).collect();
            (late.files.clone(), ids)
        })
    }

    /// The app root the late sources were read from (a key for what a
    /// caller derives from all of them).
    pub(in super::super) fn late_root() -> Option<String> {
        with_late(|late| late.root.clone())
    }

    /// The declarations of `name` in test sources not registered now.
    fn late_decls(name: &str) -> Rc<Vec<Decl>> {
        if let Some(found) = with_memo(|m| m.late_decls.get(name).cloned()) {
            return found;
        }
        let found = with_late(|late| {
            if let Some(found) = late.decls.get(name) {
                return found.clone();
            }
            let ids = late.words.get(name).cloned().unwrap_or_default();
            let files = late.files.clone();
            let mut found = Vec::new();
            for i in ids {
                let (path, text) = &files[i];
                if may_declare(text, name) {
                    found.extend(scan(text, path, name));
                }
            }
            let found = Rc::new(found);
            late.decls.insert(name.to_string(), found.clone());
            found
        });
        let found = unregistered(found, |d| &d.path);
        with_memo(|m| m.late_decls.insert(name.to_string(), found.clone()));
        found
    }

    /// `all` but those in a registered source (`all` itself when none is).
    fn unregistered<T: Clone>(all: Rc<Vec<T>>, path: impl Fn(&T) -> &String) -> Rc<Vec<T>> {
        if all.iter().all(|t| !is_registered(path(t))) {
            return all;
        }
        Rc::new(all.iter().filter(|t| !is_registered(path(t))).cloned().collect())
    }

    /// Where test sources not registered now mention `name`.
    fn late_sites(name: &str) -> Vec<Site> {
        if let Some(found) = with_memo(|m| m.late_sites.get(name).cloned()) {
            return found.as_ref().clone();
        }
        let found = with_late(|late| {
            if let Some(found) = late.sites.get(name) {
                return found.clone();
            }
            let ids = late.words.get(name).cloned().unwrap_or_default();
            let files = late.files.clone();
            let mut found = Vec::new();
            for i in ids {
                let (path, text) = &files[i];
                found.extend(scan_uses(text, path, name));
            }
            let found = Rc::new(found);
            late.sites.insert(name.to_string(), found.clone());
            found
        });
        let found = unregistered(found, |s| &s.path);
        with_memo(|m| m.late_sites.insert(name.to_string(), found.clone()));
        found.as_ref().clone()
    }

    /// Why `full` is not known when a test source not registered now
    /// declares it.
    fn late_reason(full: &str) -> Option<String> {
        let last = full.rsplit("::").next().unwrap_or(full);
        late_decls(last).iter().find(|d| d.names.iter().any(|n| n == full)).map(|d| {
            format!("`{full}` is declared in {}, a test source not registered before this one", sources::relative_path(&d.path))
        })
    }

    /// `f` of the text of `path`, registered or a late test source.
    fn with_any_text<R>(path: &str, f: impl Fn(&str) -> R) -> Option<R> {
        if let Some(r) = sources::with_text(path, &f) {
            return Some(r);
        }
        with_late(|late| late.files.iter().find(|(p, _)| p == path).map(|(_, text)| f(text)))
    }

    /// The line of `path` (registered or a late test source) at `at`.
    fn any_line_at(path: &str, at: usize) -> Option<u32> {
        sources::line_at(path, at).or_else(|| with_any_text(path, |t| t.get(..at).map(|b| b.matches('\n').count() as u32 + 1)).flatten())
    }

    // ---- Registered sources by name ----------------------------------

    thread_local! {
        static DECLS: RefCell<Cache> = RefCell::new(Cache::default());
        static INDEX: RefCell<Index> = RefCell::new(Index::default());
    }

    /// The registry a cache was filled from: its first path and that
    /// file's length.
    fn registry_key() -> Option<(String, usize)> {
        let first = sources::path_of(FileId(1))?;
        let len = sources::with_text(&first, str::len)?;
        Some((first, len))
    }

    /// The registered files each word starting with a capital (a constant
    /// name) is in.
    #[derive(Default)]
    struct Index {
        registry: Option<(String, usize)>,
        seen: u32,
        files: HashMap<String, Vec<u32>>,
    }

    /// The registered files (ids above `after`) that have `name` as a
    /// whole word.
    fn files_with(name: &str, after: u32) -> Vec<u32> {
        sync_index();
        INDEX.with(|index| {
            let index = index.borrow();
            match name.as_bytes().first() {
                Some(b) if b.is_ascii_uppercase() => {
                    index.files.get(name).map(|ids| ids.iter().copied().filter(|i| *i > after).collect()).unwrap_or_default()
                }
                // Not a word the index keeps: every file.
                _ => ((after + 1)..=index.seen).collect(),
            }
        })
    }

    /// Index the files registered since the last call; the id of the
    /// last registered file.
    fn sync_index() -> u32 {
        let registry = registry_key();
        INDEX.with(|index| {
            let mut index = index.borrow_mut();
            if index.registry != registry || (index.seen > 0 && sources::path_of(FileId(index.seen)).is_none()) {
                *index = Index { registry, seen: 0, files: HashMap::new() };
            }
            let mut id = index.seen + 1;
            while let Some(path) = sources::path_of(FileId(id)) {
                let words = sources::with_text(&path, capital_words).unwrap_or_default();
                for word in words {
                    index.files.entry(word).or_default().push(id);
                }
                index.seen = id;
                id += 1;
            }
            index.seen
        })
    }

    fn capital_words(text: &str) -> Vec<String> {
        let bytes = text.as_bytes();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
        let mut out: HashSet<&str> = HashSet::new();
        let mut i = 0;
        while i < bytes.len() {
            if word(bytes[i]) {
                let start = i;
                while i < bytes.len() && word(bytes[i]) {
                    i += 1;
                }
                if bytes[start].is_ascii_uppercase() {
                    out.insert(&text[start..i]);
                }
            } else {
                i += 1;
            }
        }
        out.into_iter().map(str::to_string).collect()
    }

    /// Declarations and mentions by name, and up to which registered file
    /// each has seen.
    #[derive(Default)]
    struct Cache {
        registry: Option<(String, usize)>,
        by_name: HashMap<String, Entry>,
    }

    #[derive(Default)]
    struct Entry {
        seen: u32,
        decls: Rc<Vec<Decl>>,
        sites: Rc<Vec<Site>>,
    }

    /// `f` of what the registered sources declare and say about `name`.
    fn with_entry<R>(name: &str, f: impl FnOnce(&Entry) -> R) -> R {
        let registry = registry_key();
        let seen = DECLS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.registry != registry {
                cache.by_name.clear();
                cache.registry = registry.clone();
            }
            let entry = cache.by_name.entry(name.to_string()).or_default();
            if entry.seen > 0 && sources::path_of(FileId(entry.seen)).is_none() {
                *entry = Entry::default();
            }
            entry.seen
        });
        let files = files_with(name, seen);
        let mut decls = Vec::new();
        let mut sites = Vec::new();
        for id in files {
            let Some(path) = sources::path_of(FileId(id)) else { continue };
            sources::with_text(&path, |text| {
                if may_declare(text, name) {
                    decls.extend(scan(text, &path, name));
                }
                sites.extend(scan_uses(text, &path, name));
            });
        }
        let last = registered_last();
        DECLS.with(|cache| {
            let mut cache = cache.borrow_mut();
            let entry = cache.by_name.entry(name.to_string()).or_default();
            if !decls.is_empty() {
                Rc::make_mut(&mut entry.decls).extend(decls);
            }
            if !sites.is_empty() {
                Rc::make_mut(&mut entry.sites).extend(sites);
            }
            entry.seen = entry.seen.max(last);
            f(entry)
        })
    }

    /// The id of the last registered file.
    fn registered_last() -> u32 {
        INDEX.with(|index| index.borrow().seen)
    }

    /// Every statement in a registered source that declares a constant
    /// called `name` (a module, a class, or an assignment).
    fn decls(name: &str) -> Rc<Vec<Decl>> {
        if let Some(found) = with_memo(|m| m.decls.get(name).cloned()) {
            return found;
        }
        let found = with_entry(name, |e| e.decls.clone());
        with_memo(|m| m.decls.insert(name.to_string(), found.clone()));
        found
    }

    /// What the lookups by name answer for one state of the sources: the
    /// registry (its first file, how many files and the last one's
    /// length) and the late sources' app root. A lookup made inside an
    /// unroll is asked again for each constant, alias and mention it
    /// passes, so each is made once per state.
    #[derive(Default)]
    struct Memo {
        generation: Option<Generation>,
        decls: HashMap<String, Rc<Vec<Decl>>>,
        late_decls: HashMap<String, Rc<Vec<Decl>>>,
        late_sites: HashMap<String, Rc<Vec<Site>>>,
        sites: HashMap<String, Rc<Vec<Site>>>,
        found: HashMap<String, Found>,
        /// By what `denotes` reads of the site: path, offset, qualifier,
        /// rootedness, lexical scopes; and the full name.
        relevant: HashMap<(String, usize, Option<Vec<String>>, bool, Option<Vec<String>>, String), bool>,
        owner: HashMap<(String, Shape, usize), Option<Site>>,
    }

    type Generation = (Option<(String, usize)>, u32, Option<usize>, Option<String>);

    thread_local! {
        static MEMO: RefCell<Memo> = RefCell::new(Memo::default());
    }

    fn generation() -> Generation {
        let last = sync_index();
        let last_len = sources::path_of(FileId(last)).and_then(|p| sources::with_text(&p, str::len));
        (registry_key(), last, last_len, app_root())
    }

    /// `f` of the memo for the sources as they are now.
    fn with_memo<R>(f: impl FnOnce(&mut Memo) -> R) -> R {
        let generation = generation();
        MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo.generation.as_ref() != Some(&generation) {
                *memo = Memo { generation: Some(generation), ..Memo::default() };
            }
            f(&mut memo)
        })
    }

    /// Is `mixed_into_object` asking (where a cycle answers cautiously)?
    fn asking() -> bool {
        ASKED.with(|a| !a.borrow().is_empty())
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
            // `X, Kinds::ALL = …`: bound, but not by a declaration.
            if let Some(target) = node.as_constant_path_target_node() {
                if target.name().is_some_and(|n| constant_id_str(&n) == self.name) {
                    let names = match target.parent() {
                        None => vec![self.name.to_string()],
                        Some(parent) => segments(&parent)
                            .map(|(s, rooted)| {
                                let joined = format!("{}::{}", s.join("::"), self.name);
                                if rooted { vec![joined] } else { vec![self.qualified(&joined), joined] }
                            })
                            .unwrap_or_default(),
                    };
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

        fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
            // `X, ALL = 1, %i[z]`, `rescue => ALL`.
            if node.as_constant_target_node().is_some_and(|t| constant_id_str(&t.name()) == self.name) {
                let names = vec![self.qualified(self.name)];
                self.push(names, false, Kind::Const, 0, 0);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // An unroll asks the same lookups for every constant, alias and
        // mention it passes: each is answered once for a state of the
        // sources, without copying what they declare, and again once a
        // source registers.
        #[test]
        fn lookups_are_shared_and_answered_again_when_a_source_registers() {
            sources::register("/memo/app/models/kinds.rb", "module Kinds\n  ALL = [:a].freeze\nend\n");
            assert!(matches!(find("Kinds::ALL"), Found::Value(_)));
            assert!(Rc::ptr_eq(&decls("ALL"), &decls("ALL")));
            assert!(Rc::ptr_eq(&sites_of("Kinds"), &sites_of("Kinds")));
            assert!(owner_reached("Kinds", Shape::Deep, 0).is_none());

            sources::register("/memo/app/models/rebind.rb", "Kinds.const_set(name, [])\n");
            assert_eq!(owner_reached("Kinds", Shape::Deep, 0).map(|s| s.path), Some("/memo/app/models/rebind.rb".to_string()));

            sources::register("/memo/app/models/again.rb", "module Kinds\n  ALL = [:b].freeze\nend\n");
            assert_eq!(decls("ALL").len(), 2);
            assert!(matches!(find("Kinds::ALL"), Found::Unknown));
        }

        fn needs(src: &str) -> Vec<Need> {
            scan_uses(src, "x.rb", "ALL").iter().map(|s| s.need).collect()
        }

        // What a mention lets code do to the value: only the listed calls
        // and a loop that only declares tests leave it to the constant.
        #[test]
        fn only_listed_uses_leave_a_constant_alone() {
            use Need::*;
            let class = |body: &str| format!("class XTest\n{body}\nend\n");
            for body in [
                "ALL.each do |x|\n  test \"#{x}\" do\n    x << 1\n  end\nend",
                "ALL.each_with_index { |x, i| y = x.to_s }",
                "ALL.size",
                "ALL.join(\", \")",
                "ALL.include?(:a)",
                "ALL.map(&:to_s).to_sentence",
                "ALL.keys.each do |k|\n  test \"#{k}\" do\n  end\nend",
                "ALL::X",
                "x = \"#{ALL}\"",
                "defined?(ALL)",
                "ALL.count { |x| x == 1 }",
            ] {
                assert_eq!(needs(&class(body)), [Always], "{body:?}");
            }
            for body in ["ALL.first", "ALL[0]", "ALL.find { |x| x == 1 }", "ALL.each { |x| puts x }", "y = ALL.map(&:to_s)", "take(*ALL)", "ALL.each(&:freeze)"] {
                assert_eq!(needs(&class(body)), [Elems], "{body:?}");
            }
            for body in [
                "ALL << :x",
                "ALL[0] = 1",
                "ALL.concat([1])",
                "ALL.to_a",
                "take(ALL)",
                "x = ALL",
                "[ALL]",
                "{ ALL => 1 }",
                "ALL == [1]",
                "def all\n  ALL\nend",
                "ALL.dup",
            ] {
                assert_eq!(needs(&class(body)), [Object], "{body:?}");
            }
            for body in ["const_set(:ALL, [])", "Levels.send(:remove_const, :ALL)"] {
                assert_eq!(needs(&class(body)), [Rebind], "{body:?}");
            }
            // `const_get` of its name is the constant; a declaration is not
            // a mention.
            assert_eq!(needs("Levels.const_get(:ALL) << :c\nclass Levels::ALL\nend\nLevels::ALL = 1\n"), [Object]);
            // Test bodies are no exception.
            assert_eq!(needs(&class("test \"t\" do\n  ALL << 1\nend")), [Object]);
        }

        fn shape_of(src: &str) -> Option<Shape> {
            let result = ruby_prism::parse(src.as_bytes());
            let root = result.node();
            let stmts = flatten_statements(root.as_program_node().unwrap().statements().as_node());
            shape(&stmts[0], &Env::default())
        }

        #[test]
        fn literals_are_told_by_what_in_them_may_change() {
            use Shape::*;
            for src in [":a", "1", "nil", "[:a, 1].freeze", "\"a\".freeze", "# frozen_string_literal: true\n\"a\"", "[[:a].freeze].freeze", "{ \"k\" => :v }.freeze"] {
                assert_eq!(shape_of(src), Some(Deep), "{src:?}");
            }
            for src in ["[:a, 1, nil, 1..2, X]", "{ a: :b, \"k\" => 1 }", "[\"a\".freeze]", "# frozen_string_literal: true\n[\"a\", \"b\"]", "%i[a b]", "[[:a].freeze]"] {
                assert_eq!(shape_of(src), Some(Shallow), "{src:?}");
            }
            for src in ["[[:a]].freeze", "[{ a: 1 }]", "{ a: [1] }.freeze", "[[:m, :n]].freeze"] {
                assert_eq!(shape_of(src), Some(Nested), "{src:?}");
            }
            for src in ["\"a\"", "[\"a\"]", "%w[a b].freeze", "[Foo.new]", "{ a: \"x\" }", "Foo.all"] {
                assert_eq!(shape_of(src), None, "{src:?}");
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

    // ActiveSupport 8.1's `excluding(*elements)` is `self - elements.flatten(1)`.
    #[test]
    fn excluding_flattens_its_arguments_one_level() {
        assert_eq!(shown(r##""#{[:a, :b, :c, :d].excluding([:a, :b])}""##).as_deref(), Some("[:c, :d]"));
        assert_eq!(shown(r##""#{[:a, :b, :c, :d].without(:c, [:d])}""##).as_deref(), Some("[:a, :b]"));
        assert_eq!(shown(r##""#{[:a, [:a], :b].excluding([[:a]])}""##).as_deref(), Some("[:a, :b]"));
        assert_eq!(shown(r##""#{[:a, [:a], :b].excluding([[:a]], :b)}""##).as_deref(), Some("[:a]"));
        assert_eq!(shown(r##""#{[:a, :b].excluding(:a, :b)}""##).as_deref(), Some("[]"));
    }

    #[test]
    fn mutating_calls_are_told_from_predicates() {
        for name in ["<<", "[]=", "push", "concat", "delete", "freeze", "map!", "name="] {
            assert!(mutates(name), "{name}");
        }
        for name in ["==", "!=", "<=", ">=", "include?", "merge", "first", "[]", "size"] {
            assert!(!mutates(name), "{name}");
        }
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
