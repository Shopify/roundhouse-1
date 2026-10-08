//! `validates :a, :b, key: options, ...` and the `validates_<key>_of`
//! family, as ActiveModel reads them, into `Validation` items.
//!
//! `validates` splits its options into the defaults every validator
//! takes (`if:`, `unless:`, `on:`, `allow_nil:`, `allow_blank:`,
//! `strict:`) and one validator per remaining key, run in key order over
//! every attribute. Each validator's options merge over the defaults
//! (`defaults.merge(_parse_validates_options(options))`): `true` is
//! `{}`, a Hash is itself, a Range or Array is `{ in: it }`, anything
//! else `{ with: it }`, and a falsy value skips the key. A key without a
//! built-in validator names the app's `<Key>Validator`, run through
//! `validates_with` with the attributes.
//!
//! Items come out validator-major (`presence` for each attribute, then
//! `length` for each), which is the order Rails adds the errors in; a
//! single attribute's rules ride on one item, in key order.
//!
//! What is modeled: `presence`, `absence`, `length` (`minimum`,
//! `maximum`, `is`, `in`/`within`, the per-check messages), `inclusion` /
//! `exclusion` (`in`/`within`), `numericality` (`only_integer`, the
//! comparisons, `odd`, `even`, `in`), `format` (`with`/`without`) and
//! `uniqueness` (`scope`, `case_sensitive`, `conditions`). A bound, list
//! or regexp may be a literal, a constant or other expression, a Symbol
//! (a method on the record) or a lambda, as ActiveModel's
//! `resolve_value` takes them. Every validator takes `allow_nil`,
//! `allow_blank`, `message` (a String, a Symbol error type, or a lambda),
//! `if` / `unless` (a Symbol, a lambda, or an Array of them) and `on`
//! (validation contexts). Options a validator does not read are passed
//! through to `errors.add` by Rails and are ignored here too. Every
//! other validator or option becomes a `ValidationRule::Unsupported`
//! that `roundhouse check` reports at the call, never a dropped or
//! weakened check.

use ruby_prism::Node;

use crate::dialect::{
    LengthCheck, LengthOp, NumericCheck, NumericOp, RuleMessage, RuleOptions, RuleValue, Validation, ValidationRule,
};
use crate::expr::{ArrayStyle, Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::span::Span;

use super::expr::ingest_expr;
use super::util::{constant_id_str, symbol_value};
use super::IngestResult;

/// Validators ActiveModel/ActiveRecord ship, by key.
const BUILT_IN: &[&str] = &[
    "presence", "absence", "length", "size", "inclusion", "exclusion", "numericality", "format",
    "uniqueness", "acceptance", "confirmation", "comparison",
];

/// The `validates`-level keys that are defaults for every validator.
const DEFAULT_KEYS: &[&str] = &["if", "unless", "on", "allow_blank", "allow_nil", "strict"];

/// `validates ...` / `validates_<key>_of ...`: the Validations it
/// declares, or `None` for any other call. A declaration whose shape is
/// not read (a splat attribute list, a non-Symbol key) answers one
/// `Unsupported` rule rather than nothing.
pub(super) fn parse_validation_call(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
) -> IngestResult<Option<Vec<Validation>>> {
    if call.receiver().is_some() || call.block().is_some() {
        return Ok(None);
    }
    let name = constant_id_str(&call.name()).to_string();
    let family_key = name
        .strip_prefix("validates_")
        .and_then(|rest| rest.strip_suffix("_of"))
        .filter(|key| BUILT_IN.contains(key))
        .map(str::to_string);
    if name != "validates" && family_key.is_none() {
        return Ok(None);
    }
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let mut attrs: Vec<Symbol> = Vec::new();
    let mut entries: Vec<(String, Node<'_>)> = Vec::new();
    let mut shape_gap: Option<String> = None;
    for (i, arg) in args.iter().enumerate() {
        if let Some(sym) = symbol_value(arg).or_else(|| super::util::string_value(arg)) {
            attrs.push(Symbol::from(sym.as_str()));
            continue;
        }
        if i + 1 == args.len() {
            if let Some(kh) = arg.as_keyword_hash_node() {
                for el in kh.elements().iter() {
                    match el.as_assoc_node().and_then(|a| symbol_value(&a.key()).map(|k| (k, a.value()))) {
                        Some((k, v)) => entries.push((k, v)),
                        None => shape_gap = Some("an option that is not a `key: value` pair with a Symbol key".into()),
                    }
                }
                continue;
            }
        }
        shape_gap = Some(format!(
            "an attribute argument that is not a Symbol or String literal (`{}`)",
            String::from_utf8_lossy(arg.location().as_slice())
        ));
    }
    let construct = format!("`{name}`");
    if attrs.is_empty() && shape_gap.is_none() {
        shape_gap = Some("no attribute named".into());
    }
    if let Some(detail) = shape_gap {
        return Ok(Some(vec![Validation {
            attribute: attrs.first().cloned().unwrap_or_else(|| Symbol::from("base")),
            rules: vec![ValidationRule::Unsupported { construct, detail }],
        }]));
    }

    // (validator key, its option entries or a shorthand value)
    let mut validators: Vec<(String, ValidatorOptions<'_>)> = Vec::new();
    let mut defaults: Vec<(String, Node<'_>)> = Vec::new();
    match family_key {
        // `validates_inclusion_of :a, in: [...], allow_nil: true`: every
        // option is the validator's own.
        Some(key) => validators.push((key, ValidatorOptions::Entries(entries))),
        None => {
            for (key, value) in entries {
                if DEFAULT_KEYS.contains(&key.as_str()) {
                    defaults.push((key, value));
                } else {
                    validators.push((key, ValidatorOptions::from_value(value)));
                }
            }
        }
    }
    if validators.is_empty() {
        return Ok(Some(vec![Validation {
            attribute: attrs[0].clone(),
            rules: vec![ValidationRule::Unsupported {
                construct,
                detail: "no validator named (ActiveModel raises \"You need to supply at least one validation\")".into(),
            }],
        }]));
    }

    let mut out = Vec::new();
    for (key, opts) in validators {
        let entries = match opts {
            ValidatorOptions::Skip => continue,
            ValidatorOptions::Entries(e) => e,
            // `inclusion: ALLOWED` is `{ in: ALLOWED }` when the constant
            // holds a Range or an Array and `{ with: ALLOWED }` otherwise;
            // the second raises at load for these validators, so a class
            // that loads holds one of the first.
            ValidatorOptions::Shorthand("with", v) if matches!(key.as_str(), "inclusion" | "exclusion" | "length" | "size") => {
                vec![("in".to_string(), v)]
            }
            ValidatorOptions::Shorthand(k, v) => vec![(k.to_string(), v)],
            ValidatorOptions::Unreadable(detail) => {
                for attr in &attrs {
                    out.push(Validation {
                        attribute: attr.clone(),
                        rules: vec![ValidationRule::Unsupported {
                            construct: format!("`validates {key}:`"),
                            detail: detail.clone(),
                        }],
                    });
                }
                continue;
            }
        };
        for attr in &attrs {
            let rule = validator_rule(&key, attr, &defaults, &entries, file)?;
            if let Some(rule) = rule {
                out.push(Validation { attribute: attr.clone(), rules: vec![rule] });
            }
        }
    }
    // One attribute: one Validation carrying its rules in key order,
    // which is the same run order.
    if attrs.len() == 1 && out.len() > 1 {
        let rules = out.drain(..).flat_map(|v| v.rules).collect();
        out.push(Validation { attribute: attrs[0].clone(), rules });
    }
    // Empty when every validator was `key: false`/`nil`: nothing is declared.
    Ok(Some(out))
}

enum ValidatorOptions<'pr> {
    Skip,
    Entries(Vec<(String, Node<'pr>)>),
    Shorthand(&'static str, Node<'pr>),
    Unreadable(String),
}

impl<'pr> ValidatorOptions<'pr> {
    /// `_parse_validates_options`.
    fn from_value(value: Node<'pr>) -> Self {
        if value.as_false_node().is_some() || value.as_nil_node().is_some() {
            return ValidatorOptions::Skip;
        }
        if value.as_true_node().is_some() {
            return ValidatorOptions::Entries(Vec::new());
        }
        let elements = value
            .as_hash_node()
            .map(|h| h.elements())
            .or_else(|| value.as_keyword_hash_node().map(|h| h.elements()));
        if let Some(elements) = elements {
            let mut out = Vec::new();
            for el in elements.iter() {
                match el.as_assoc_node().and_then(|a| symbol_value(&a.key()).map(|k| (k, a.value()))) {
                    Some(pair) => out.push(pair),
                    None => {
                        return ValidatorOptions::Unreadable(
                            "an option that is not a `key: value` pair with a Symbol key".into(),
                        )
                    }
                }
            }
            return ValidatorOptions::Entries(out);
        }
        if value.as_array_node().is_some() || value.as_range_node().is_some() {
            return ValidatorOptions::Shorthand("in", value);
        }
        ValidatorOptions::Shorthand("with", value)
    }
}

fn unsupported(key: &str, detail: impl Into<String>) -> ValidationRule {
    ValidationRule::Unsupported { construct: format!("`validates {key}:`"), detail: detail.into() }
}

/// The options each built-in validator reads; any other key is passed
/// through to `errors.add`, where only a `message:` interpolating it
/// could see it, so it is ignored as Rails ignores it.
fn own_keys(key: &str) -> &'static [&'static str] {
    match key {
        "length" | "size" => &["minimum", "maximum", "is", "in", "within", "too_short", "too_long", "wrong_length"],
        "inclusion" | "exclusion" => &["in", "within"],
        "numericality" => &[
            "greater_than", "greater_than_or_equal_to", "equal_to", "less_than", "less_than_or_equal_to",
            "other_than", "odd", "even", "in", "only_integer", "only_numeric",
        ],
        "format" => &["with", "without", "multiline"],
        "uniqueness" => &["scope", "case_sensitive", "conditions", "class"],
        _ => &[],
    }
}

/// One validator on one attribute: its rule under the merged options,
/// `None` when the rule can never fire (`presence` with `allow_blank`).
fn validator_rule(
    key: &str,
    attr: &Symbol,
    defaults: &[(String, Node<'_>)],
    entries: &[(String, Node<'_>)],
    file: &str,
) -> IngestResult<Option<ValidationRule>> {
    if !BUILT_IN.contains(&key) {
        return custom_validator(key, attr, defaults, entries, file).map(Some);
    }
    let mut options = RuleOptions::default();
    let mut allow_given = false;
    let mut allow_blank_false = false;
    let mut own: Vec<(&str, &Node<'_>)> = Vec::new();
    // Defaults first, then the validator's own keys over them.
    let merged = defaults.iter().chain(entries.iter()).map(|(k, v)| (k.as_str(), v));
    for (k, v) in merged {
        match k {
            "allow_nil" | "allow_blank" => {
                let b = match super::util::bool_value(v) {
                    Some(b) => b,
                    // `allow_nil: nil` is "not given" to `skip_nil_check?`
                    // and false to EachValidator.
                    None if v.as_nil_node().is_some() => false,
                    None => return Ok(Some(unsupported(key, format!("`{k}:` that is not `true`, `false` or `nil`")))),
                };
                allow_given |= v.as_nil_node().is_none();
                if k == "allow_nil" {
                    options.allow_nil = b;
                } else {
                    options.allow_blank = b;
                    allow_blank_false = v.as_false_node().is_some();
                }
            }
            "if" | "unless" => match conditions(v, file)? {
                Ok(conds) if k == "if" => options.if_conds = conds,
                Ok(conds) => options.unless_conds = conds,
                Err(detail) => return Ok(Some(unsupported(key, format!("`{k}:` {detail}")))),
            },
            "on" => match contexts(v) {
                Some(on) => options.on = on,
                None => return Ok(Some(unsupported(key, "`on:` that is not a Symbol or an Array of Symbols"))),
            },
            "strict" => return Ok(Some(unsupported(key, "`strict:` (raising validations)"))),
            "message" => match message(v, file)? {
                Ok(m) => options.message = Some(m),
                Err(detail) => return Ok(Some(unsupported(key, detail))),
            },
            _ if own_keys(key).contains(&k) => {
                own.retain(|(seen, _)| *seen != k);
                own.push((k, v));
            }
            // Passed through to `errors.add` and otherwise unread.
            _ => {}
        }
    }
    let rule = match key {
        "presence" => Ok(ValidationRule::Presence),
        "absence" => Ok(ValidationRule::Absence),
        "length" | "size" => {
            let nil_fails_maximum = allow_given && !options.allow_nil && !options.allow_blank;
            length_rule(&own, nil_fails_maximum, allow_blank_false, file)?
        }
        "inclusion" | "exclusion" => inclusion_rule(key == "exclusion", &own, file)?,
        "numericality" => numericality_rule(&own, file)?,
        "format" => format_rule(&own, file)?,
        "uniqueness" => uniqueness_rule(&own, file)?,
        other => Err(format!("the `{other}` validator is not modeled")),
    };
    let rule = match rule {
        Ok(rule) => rule,
        Err(detail) => return Ok(Some(unsupported(key, detail))),
    };
    let needs_count = match &options.message {
        Some(RuleMessage::Text { text }) => text.contains("%{count}"),
        Some(RuleMessage::Key { key }) => crate::dialect::active_model_en_message(key.as_str())
            .is_some_and(|(one, other)| one.is_some() || other.contains("%{count}")),
        _ => false,
    };
    if needs_count {
        let counted = matches!(
            rule,
            ValidationRule::Length { .. } | ValidationRule::LengthOf { .. }
        );
        if !counted {
            return Ok(Some(unsupported(
                key,
                "a `message:` interpolating %{count} on a validator with an error that has no count (I18n raises MissingInterpolationArgument)",
            )));
        }
    }
    // `presence` under `allow_blank` never fires: presence fails only on
    // a blank value, which `allow_blank` skips. Rails runs validations
    // before `before_save`, and lobsters' `validates :session_token,
    // allow_blank: true, presence: true` relies on exactly that.
    if options.allow_blank && matches!(rule, ValidationRule::Presence) {
        return Ok(None);
    }
    if options == RuleOptions::default() {
        return Ok(Some(rule));
    }
    Ok(Some(ValidationRule::Guarded { rule: Box::new(rule), options }))
}

/// `on: :create` / `on: [:create, :publish]`.
fn contexts(v: &Node<'_>) -> Option<Vec<Symbol>> {
    if let Some(sym) = symbol_value(v) {
        return Some(vec![Symbol::from(sym.as_str())]);
    }
    let array = v.as_array_node()?;
    array.elements().iter().map(|e| symbol_value(&e).map(|s| Symbol::from(s.as_str()))).collect()
}

/// `if:` / `unless:`: a Symbol (a method on the record), a lambda/proc
/// literal (run with the record as `self`, and as its argument when it
/// takes one), or an Array of those, every one of which counts.
fn conditions(v: &Node<'_>, file: &str) -> IngestResult<Result<Vec<RuleValue>, String>> {
    let mut out = Vec::new();
    match v.as_array_node() {
        Some(a) => {
            for item in a.elements().iter() {
                match condition(&item, file)? {
                    Ok(c) => out.push(c),
                    Err(e) => return Ok(Err(e)),
                }
            }
        }
        None => match condition(v, file)? {
            Ok(c) => out.push(c),
            Err(e) => return Ok(Err(e)),
        },
    }
    Ok(Ok(out))
}

fn condition(item: &Node<'_>, file: &str) -> IngestResult<Result<RuleValue, String>> {
    if let Some(sym) = symbol_value(item) {
        return Ok(Ok(RuleValue::Method { name: Symbol::from(sym.as_str()) }));
    }
    let expr = ingest_expr(item, file)?;
    Ok(match callable(&expr) {
        Some((callable, arity)) if arity <= 1 => Ok(RuleValue::Call { callable, with_record: arity == 1 }),
        Some(_) => Err("lambda with more than one parameter".into()),
        None => Err("condition that is not a Symbol, a lambda/proc literal or an Array of those".into()),
    })
}

/// A lambda / proc literal and its parameter count.
fn callable(expr: &Expr) -> Option<(Expr, usize)> {
    let lambda = match &*expr.node {
        ExprNode::Lambda { .. } => expr,
        ExprNode::Send { recv: None, method, block: Some(block), args, .. }
            if args.is_empty() && matches!(method.as_str(), "lambda" | "proc") =>
        {
            block
        }
        _ => return None,
    };
    let ExprNode::Lambda { params, rest_param, block_param, .. } = &*lambda.node else { return None };
    if rest_param.is_some() || block_param.is_some() {
        return None;
    }
    Some((expr.clone(), params.len()))
}

/// Whether evaluating `expr` reads `self` — an ivar, `self`, or a call
/// with no receiver. A value ActiveModel resolves (`resolve_value`) is
/// evaluated where the class body wrote it, with the class as `self`;
/// lowered into the record's `validate`, such a read would see the
/// record instead.
fn reads_self(expr: &Expr) -> bool {
    const KERNEL: &[&str] = &[
        "lambda", "proc", "raise", "format", "sprintf", "Integer", "Float", "String", "Array", "Hash",
        "Rational", "BigDecimal", "loop", "freeze",
    ];
    let mut found = false;
    let mut stack = vec![expr];
    while let Some(e) = stack.pop() {
        match &*e.node {
            ExprNode::SelfRef | ExprNode::Ivar { .. } => found = true,
            ExprNode::Send { recv: None, method, .. } if !KERNEL.contains(&method.as_str()) => found = true,
            _ => {}
        }
        if found {
            return true;
        }
        e.node.for_each_child(&mut |c| stack.push(c));
    }
    false
}

/// A value ActiveModel resolves on each run (`resolve_value`): an
/// Integer/Float literal, a Symbol (a method on the record), a
/// lambda/proc literal (called with the record when it takes one), or
/// any other expression, evaluated as written.
fn rule_value(v: &Node<'_>, k: &str, file: &str) -> IngestResult<Result<RuleValue, String>> {
    if let Some(lit) = numeric_literal(v) {
        return Ok(Ok(RuleValue::Literal { value: lit }));
    }
    if let Some(sym) = symbol_value(v) {
        return Ok(Ok(RuleValue::Method { name: Symbol::from(sym.as_str()) }));
    }
    let expr = ingest_expr(v, file)?;
    if let Some((callable, arity)) = callable(&expr) {
        if arity > 1 {
            return Ok(Err(format!("`{k}:` lambda with more than one parameter")));
        }
        if reads_self(&callable) {
            return Ok(Err(format!(
                "`{k}:` lambda that reads `self` (ActiveModel calls it with the class as `self`)"
            )));
        }
        return Ok(Ok(RuleValue::Call { callable, with_record: arity == 1 }));
    }
    if reads_self(&expr) {
        return Ok(Err(format!("`{k}:` expression that reads `self` (evaluated in the class body)")));
    }
    Ok(Ok(RuleValue::Expr { expr }))
}

/// `message:` as a String literal (its `%{...}` keys must be ones the
/// error supplies), a callable constant / lambda literal, or a Symbol
/// error type.
fn message(value: &Node<'_>, file: &str) -> IngestResult<Result<RuleMessage, String>> {
    if let Some(text) = super::util::string_value(value) {
        let mut rest = text.as_str();
        while let Some(start) = rest.find("%{") {
            let tail = &rest[start + 2..];
            let Some(end) = tail.find('}') else { break };
            let name = &tail[..end];
            if !matches!(name, "attribute" | "model" | "value" | "count") {
                return Ok(Err(format!(
                    "a `message:` interpolating %{{{name}}}, which the error does not supply (I18n raises MissingInterpolationArgument)"
                )));
            }
            rest = &tail[end + 1..];
        }
        return Ok(Ok(RuleMessage::Text { text }));
    }
    if let Some(sym) = symbol_value(value) {
        return Ok(Ok(RuleMessage::Key { key: Symbol::from(sym.as_str()) }));
    }
    let expr = ingest_expr(value, file)?;
    if let Some((callable, _)) = callable(&expr) {
        // ActiveModel calls it with the class body's `self`; the emitted
        // check calls it inside the record's method.
        if reads_self(&callable) {
            return Ok(Err("a `message:` lambda that reads `self` (ActiveModel calls it with the class as `self`)".into()));
        }
        return Ok(Ok(RuleMessage::Call { callable: expr }));
    }
    // A constant may hold a String or a Proc; which, the run tells. An
    // interpolated String is evaluated where the class body wrote it.
    let constant = matches!(&*expr.node, ExprNode::Const { .. });
    let interpolated = matches!(&*expr.node, ExprNode::StringInterp { .. });
    if (constant || interpolated) && !reads_self(&expr) {
        return Ok(Ok(RuleMessage::Value { expr }));
    }
    Ok(Err("a `message:` that is not a String or Symbol literal, a constant, an interpolated String or a lambda".to_string()))
}

fn int_literal(v: &Node<'_>) -> Option<i64> {
    super::util::integer_value(v)
}

/// `length:`. Literal `minimum`/`maximum` (or an Integer-literal
/// `in:`/`within:` Range) is `Length`; anything else Rails takes
/// (`is:`, a constant, a method, a lambda, `too_long:`, ...) is
/// `LengthOf`.
fn length_rule(
    own: &[(&str, &Node<'_>)],
    nil_fails_maximum: bool,
    allow_blank_false: bool,
    file: &str,
) -> IngestResult<Result<ValidationRule, String>> {
    let mut bounds: Vec<(LengthOp, RuleValue)> = Vec::new();
    let mut messages: Vec<(LengthOp, RuleMessage)> = Vec::new();
    let set = |bounds: &mut Vec<(LengthOp, RuleValue)>, op: LengthOp, v: RuleValue| {
        bounds.retain(|(o, _)| *o != op);
        bounds.push((op, v));
    };
    for (k, v) in own {
        match *k {
            "minimum" | "maximum" | "is" => {
                let op = match *k {
                    "minimum" => LengthOp::Minimum,
                    "maximum" => LengthOp::Maximum,
                    _ => LengthOp::Is,
                };
                let value = match rule_value(v, k, file)? {
                    Ok(RuleValue::Literal { value: Literal::Int { value } }) if value < 0 => {
                        return Ok(Err(format!("a negative `{k}:` (ActiveModel raises at load)")))
                    }
                    Ok(RuleValue::Literal { value: Literal::Float { .. } }) => {
                        return Ok(Err(format!("a Float `{k}:` (ActiveModel raises at load)")))
                    }
                    Ok(value) => value,
                    Err(detail) => return Ok(Err(detail)),
                };
                set(&mut bounds, op, value);
            }
            "in" | "within" => {
                let literal = v.as_range_node().filter(|r| {
                    let end = |n: Option<Node<'_>>| n.is_none_or(|n| int_literal(&n).is_some_and(|n| n >= 0));
                    end(r.left()) && end(r.right())
                });
                let Some(range) = literal else {
                    // A constant or computed Range: Rails reads its
                    // `min` / `max` (it raises at load for anything but
                    // a Range).
                    let expr = ingest_expr(v, file)?;
                    // A Range literal says which ends it has; Rails sets no
                    // bound for a missing one. A constant's Range is read
                    // per run (an endless one raises at `max`).
                    let (has_min, has_max) = match &*expr.node {
                        ExprNode::Range { begin, end, .. } => (begin.is_some(), end.is_some()),
                        ExprNode::Const { .. } => (true, true),
                        _ => return Ok(Err(format!("`{k}:` that is not a Range literal or a constant"))),
                    };
                    if reads_self(&expr) {
                        return Ok(Err(format!("`{k}:` that reads the record")));
                    }
                    let s = Span::synthetic();
                    let end = |m: &str| RuleValue::Expr {
                        expr: Expr::new(
                            s,
                            ExprNode::Send {
                                recv: Some(expr.clone()),
                                method: Symbol::from(m),
                                args: Vec::new(),
                                block: None,
                                parenthesized: false,
                            },
                        ),
                    };
                    if has_min {
                        set(&mut bounds, LengthOp::Minimum, end("min"));
                    }
                    if has_max {
                        set(&mut bounds, LengthOp::Maximum, end("max"));
                    }
                    continue;
                };
                let end = |n: &Node<'_>| int_literal(n).filter(|n| *n >= 0);
                let lo = match range.left() {
                    Some(n) => Some(end(&n).ok_or_else(|| format!("`{k}:` with a bound that is not a non-negative Integer literal"))),
                    None => None,
                };
                let hi = match range.right() {
                    Some(n) => Some(end(&n).ok_or_else(|| format!("`{k}:` with a bound that is not a non-negative Integer literal"))),
                    None => None,
                };
                if let Some(lo) = lo {
                    match lo {
                        Ok(n) => set(&mut bounds, LengthOp::Minimum, RuleValue::Literal { value: Literal::Int { value: n } }),
                        Err(e) => return Ok(Err(e)),
                    }
                }
                if let Some(hi) = hi {
                    let hi = match hi {
                        Ok(n) => n,
                        Err(e) => return Ok(Err(e)),
                    };
                    let hi = if range.is_exclude_end() { hi - 1 } else { hi };
                    set(&mut bounds, LengthOp::Maximum, RuleValue::Literal { value: Literal::Int { value: hi } });
                }
            }
            "too_short" | "too_long" | "wrong_length" => {
                let op = match *k {
                    "too_short" => LengthOp::Minimum,
                    "too_long" => LengthOp::Maximum,
                    _ => LengthOp::Is,
                };
                match message(v, file)? {
                    Ok(m) => messages.push((op, m)),
                    Err(detail) => return Ok(Err(format!("`{k}:` {detail}"))),
                }
            }
            _ => {}
        }
    }
    // LengthValidator#initialize: `allow_blank: false` without a minimum
    // or `is` means a minimum of 1.
    if allow_blank_false && !bounds.iter().any(|(o, _)| matches!(o, LengthOp::Minimum | LengthOp::Is)) {
        set(&mut bounds, LengthOp::Minimum, RuleValue::Literal { value: Literal::Int { value: 1 } });
    }
    if bounds.is_empty() {
        return Ok(Err("no `minimum:`, `maximum:`, `is:`, `in:` or `within:` (ActiveModel raises at load)".into()));
    }
    let literal = |op: LengthOp| {
        bounds.iter().find(|(o, _)| *o == op).map(|(_, v)| match v {
            RuleValue::Literal { value: Literal::Int { value } } => u32::try_from(*value).ok(),
            _ => None,
        })
    };
    let (min, max) = (literal(LengthOp::Minimum), literal(LengthOp::Maximum));
    let simple = messages.is_empty()
        && !nil_fails_maximum
        && !bounds.iter().any(|(o, _)| *o == LengthOp::Is)
        && min.is_none_or(|m| m.is_some())
        && max.is_none_or(|m| m.is_some());
    if simple {
        return Ok(Ok(ValidationRule::Length { min: min.flatten(), max: max.flatten(), message: None }));
    }
    let checks = [LengthOp::Is, LengthOp::Minimum, LengthOp::Maximum]
        .into_iter()
        .filter_map(|op| {
            let bound = bounds.iter().find(|(o, _)| *o == op)?.1.clone();
            let message = messages.iter().rev().find(|(o, _)| *o == op).map(|(_, m)| m.clone());
            Some(LengthCheck { op, bound, message })
        })
        .collect();
    Ok(Ok(ValidationRule::LengthOf { checks, nil_fails_maximum }))
}

fn inclusion_rule(
    exclusion: bool,
    own: &[(&str, &Node<'_>)],
    file: &str,
) -> IngestResult<Result<ValidationRule, String>> {
    let mut list: Option<&Node<'_>> = None;
    for (k, v) in own {
        if matches!(*k, "in" | "within") {
            list = Some(*v);
        }
    }
    let Some(list) = list else {
        return Ok(Err("no `in:`/`within:` list (ActiveModel raises at load)".into()));
    };
    let nil = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil });
    if symbol_value(list).is_some() || callable(&ingest_expr(list, file)?).is_some() {
        return Ok(match rule_value(list, "in", file)? {
            Ok(value) => Ok(ValidationRule::Inclusion { list: nil, list_of: Some(value), exclusion, cover: false }),
            Err(detail) => Err(detail),
        });
    }
    let expr = ingest_expr(list, file)?;
    if reads_self(&expr) {
        return Ok(Err("an `in:` expression that reads `self` (evaluated in the class body)".into()));
    }
    // Rails asks a Range over numbers (or times) `cover?`, any other
    // enumerable `include?`.
    let cover = matches!(&*expr.node, ExprNode::Range { begin, end, .. }
        if begin.as_ref().or(end.as_ref()).is_some_and(|e| matches!(&*e.node,
            ExprNode::Lit { value: Literal::Int { .. } | Literal::Float { .. } })));
    Ok(Ok(ValidationRule::Inclusion { list: expr, list_of: None, exclusion, cover }))
}

fn numeric_literal(v: &Node<'_>) -> Option<Literal> {
    if let Some(n) = int_literal(v) {
        return Some(Literal::Int { value: n });
    }
    let float = |n: &Node<'_>| {
        n.as_float_node().map(|f| f.value())
    };
    if let Some(f) = float(v) {
        return Some(Literal::Float { value: f });
    }
    // `-1.5` parses as a unary minus call on some Prism versions.
    let call = v.as_call_node()?;
    if constant_id_str(&call.name()) == "-@" && call.arguments().is_none() {
        let recv = call.receiver()?;
        return float(&recv).map(|f| Literal::Float { value: -f });
    }
    None
}

fn numericality_rule(own: &[(&str, &Node<'_>)], file: &str) -> IngestResult<Result<ValidationRule, String>> {
    let mut only_integer = false;
    let mut found: Vec<NumericCheck> = Vec::new();
    for (k, v) in own {
        match *k {
            "only_integer" => match super::util::bool_value(v) {
                Some(b) => only_integer = b,
                None => return Ok(Err("`only_integer:` that is not `true` or `false`".into())),
            },
            "only_numeric" => return Ok(Err("`only_numeric:` option".into())),
            _ => {
                let Some(op) = NumericOp::from_key(k) else { continue };
                let bound = match op {
                    // `odd:` / `even:` check parity whatever their value.
                    NumericOp::Odd | NumericOp::Even => None,
                    NumericOp::In => {
                        let value = match rule_value(v, k, file)? {
                            Ok(RuleValue::Expr { expr })
                                if !matches!(&*expr.node, ExprNode::Range { .. } | ExprNode::Const { .. }) =>
                            {
                                return Ok(Err("`in:` that is not a Range, a constant, a Symbol or a lambda".into()))
                            }
                            Ok(RuleValue::Literal { .. }) => {
                                return Ok(Err("`in:` that is not a Range (ActiveModel raises at load)".into()))
                            }
                            Ok(value) => value,
                            Err(detail) => return Ok(Err(detail)),
                        };
                        Some(value)
                    }
                    _ => match rule_value(v, k, file)? {
                        Ok(value) => Some(value),
                        Err(detail) => return Ok(Err(detail)),
                    },
                };
                found.retain(|c| c.op != op);
                found.push(NumericCheck { op, bound });
            }
        }
    }
    let checks = NumericOp::ORDER
        .into_iter()
        .filter_map(|op| found.iter().find(|c| c.op == op).cloned())
        .collect();
    Ok(Ok(ValidationRule::Numericality { only_integer, checks }))
}

fn format_rule(own: &[(&str, &Node<'_>)], file: &str) -> IngestResult<Result<ValidationRule, String>> {
    let multiline = own.iter().any(|(k, v)| *k == "multiline" && v.as_true_node().is_some());
    let mut found: Option<ValidationRule> = None;
    let mut given = 0;
    for (k, v) in own {
        if !matches!(*k, "with" | "without") {
            continue;
        }
        given += 1;
        let without = *k == "without";
        let Some(re) = v.as_regular_expression_node() else {
            found = Some(match rule_value(v, k, file)? {
                Ok(RuleValue::Literal { .. } | RuleValue::Method { .. }) => {
                    return Ok(Err(format!("`{k}:` that is not a regexp or a Proc (ActiveModel raises at load)")))
                }
                Ok(regexp) => ValidationRule::FormatOf { regexp, without },
                Err(detail) => return Ok(Err(detail)),
            });
            continue;
        };
        let pattern = String::from_utf8_lossy(re.unescaped()).into_owned();
        let src = String::from_utf8_lossy(re.content_loc().as_slice()).into_owned();
        // FormatValidator#check_options_validity: a `^`/`$` anchor
        // without `multiline: true` raises at load.
        if !multiline && (src.starts_with('^') || (src.ends_with('$') && !src.ends_with("\\$"))) {
            return Ok(Err(
                "a regexp with a `^`/`$` anchor (ActiveModel raises at load without `multiline: true`)".into(),
            ));
        }
        let mut flags = String::new();
        if re.is_ignore_case() {
            flags.push('i');
        }
        if re.is_multi_line() {
            flags.push('m');
        }
        if re.is_extended() {
            flags.push('x');
        }
        found = Some(ValidationRule::Format { pattern, flags, without });
    }
    match (found, given) {
        (Some(rule), 1) => Ok(Ok(rule)),
        _ => Ok(Err("not exactly one of `with:` and `without:` (ActiveModel raises at load)".into())),
    }
}

/// `uniqueness:` — `scope:` (a Symbol or an Array of them),
/// `case_sensitive:` and a `conditions:` lambda.
fn uniqueness_rule(own: &[(&str, &Node<'_>)], file: &str) -> IngestResult<Result<ValidationRule, String>> {
    let mut scope = Vec::new();
    let mut case_sensitive = true;
    let mut conditions = None;
    for (k, v) in own {
        match *k {
            "scope" => {
                scope.clear();
                let name = |item: &Node<'_>| symbol_value(item).or_else(|| super::util::string_value(item));
                let names: Option<Vec<String>> = match v.as_array_node() {
                    Some(a) => a.elements().iter().map(|e| name(&e)).collect(),
                    None => name(v).map(|n| vec![n]),
                };
                match names {
                    Some(names) => scope.extend(names.iter().map(|n| Symbol::from(n.as_str()))),
                    None => return Ok(Err("`scope:` that is not a Symbol or an Array of Symbols".into())),
                }
            }
            "case_sensitive" => match super::util::bool_value(v) {
                Some(b) => case_sensitive = b,
                None => return Ok(Err("`case_sensitive:` that is not `true` or `false`".into())),
            },
            "conditions" => {
                let expr = ingest_expr(v, file)?;
                match callable(&expr) {
                    Some((callable, arity)) if arity <= 1 && where_chain_lambda(&callable) => {
                        conditions = Some(callable)
                    }
                    Some(_) => {
                        return Ok(Err(
                            "`conditions:` lambda that is not a `where(...)` / `where.not(...)` chain".into(),
                        ))
                    }
                    _ => return Ok(Err("`conditions:` that is not a lambda literal".into())),
                }
            }
            "class" => return Ok(Err("`class:` option".into())),
            _ => {}
        }
    }
    Ok(Ok(ValidationRule::Uniqueness { scope, case_sensitive, conditions }))
}

/// A `conditions:` lambda whose body is a `where(...)` / `where.not(...)`
/// chain on the relation it is `instance_exec`ed on, with arguments that
/// read nothing of `self` (the relation) — the shape the lowering writes
/// onto its relation.
fn where_chain_lambda(callable: &Expr) -> bool {
    let lambda = match &*callable.node {
        ExprNode::Lambda { .. } => callable,
        ExprNode::Send { block: Some(block), .. } => block,
        _ => return false,
    };
    let ExprNode::Lambda { body, .. } = &*lambda.node else { return false };
    let body = match &*body.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
        _ => body,
    };
    fn chain(e: &Expr) -> bool {
        let ExprNode::Send { recv, method, args, block: None, .. } = &*e.node else { return false };
        if args.is_empty() || args.iter().any(reads_self) {
            return false;
        }
        let on = |r: &Option<Expr>| r.as_ref().is_none_or(chain);
        match method.as_str() {
            "where" => on(recv),
            "not" => match recv.as_ref().map(|r| &*r.node) {
                Some(ExprNode::Send { recv: root, method: w, args: none, block: None, .. }) => {
                    w.as_str() == "where" && none.is_empty() && on(root)
                }
                _ => false,
            },
            _ => false,
        }
    }
    chain(body)
}

/// `validates :a, foo: opts` → `validates_with FooValidator,
/// attributes: [:a], <defaults>, <opts>`, the app's EachValidator.
fn custom_validator(
    key: &str,
    attr: &Symbol,
    defaults: &[(String, Node<'_>)],
    entries: &[(String, Node<'_>)],
    file: &str,
) -> IngestResult<ValidationRule> {
    let s = Span::synthetic();
    let sym = |name: &str| Expr::new(s, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(name) } });
    let mut hash: Vec<(Expr, Expr)> = vec![(
        sym("attributes"),
        Expr::new(s, ExprNode::Array { elements: vec![sym(attr.as_str())], style: ArrayStyle::Brackets }),
    )];
    // `on:` / `if:` / `unless:` guard the validator's run (Rails turns
    // them into the `validate` callback's conditions) and, as Rails
    // passes every option, ride in its options too.
    let mut guard = RuleOptions::default();
    for (k, v) in defaults.iter().chain(entries.iter()) {
        match k.as_str() {
            "strict" => return Ok(unsupported(key, format!("`{k}:` on a custom validator"))),
            "on" => match contexts(v) {
                Some(on) => guard.on = on,
                None => return Ok(unsupported(key, "`on:` that is not a Symbol or an Array of Symbols")),
            },
            "if" | "unless" => match conditions(v, file)? {
                Ok(conds) if k == "if" => guard.if_conds = conds,
                Ok(conds) => guard.unless_conds = conds,
                Err(detail) => return Ok(unsupported(key, format!("`{k}:` {detail}"))),
            },
            _ => {}
        }
        let value = ingest_expr(v, file)?;
        hash.retain(|(hk, _)| !matches!(&*hk.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == k));
        hash.push((sym(k), value));
    }
    let class = format!("{}Validator", crate::naming::camelize(key));
    let rule = ValidationRule::With {
        validator: Expr::new(s, ExprNode::Const { path: vec![Symbol::from(class.as_str())] }),
        options: Some(Expr::new(s, ExprNode::Hash { entries: hash, kwargs: false })),
        if_method: None,
        unless_method: None,
    };
    if guard == RuleOptions::default() {
        return Ok(rule);
    }
    Ok(ValidationRule::Guarded { rule: Box::new(rule), options: guard })
}
