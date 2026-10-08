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
//! `maximum`, an Integer-literal `in`/`within` Range), `inclusion` /
//! `exclusion` (`in`/`within` of any non-callable expression),
//! `numericality` (`only_integer` and the comparison options against
//! numeric literals, `odd`, `even`), `format` (`with`/`without` a regexp
//! literal), and on each validator `allow_nil`, `allow_blank`,
//! `message` (a String, or a lambda constant/literal) and Symbol `if` /
//! `unless`. Every other validator or option becomes a
//! `ValidationRule::Unsupported` that `roundhouse check` reports at the
//! call, never a dropped or weakened check.

use ruby_prism::Node;

use crate::dialect::{NumericCheck, NumericOp, RuleMessage, RuleOptions, Validation, ValidationRule};
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
    let mut own: Vec<(&str, &Node<'_>)> = Vec::new();
    // Defaults first, then the validator's own keys over them.
    let merged = defaults.iter().map(|(k, v)| (k.as_str(), v, true)).chain(entries.iter().map(|(k, v)| (k.as_str(), v, false)));
    for (k, v, _from_defaults) in merged {
        match k {
            "allow_nil" | "allow_blank" => match super::util::bool_value(v) {
                Some(b) if k == "allow_nil" => options.allow_nil = b,
                Some(b) => options.allow_blank = b,
                None => return Ok(Some(unsupported(key, format!("`{k}:` that is not `true` or `false`")))),
            },
            "if" | "unless" => match symbol_value(v) {
                Some(m) if k == "if" => options.if_method = Some(Symbol::from(m.as_str())),
                Some(m) => options.unless_method = Some(Symbol::from(m.as_str())),
                None => return Ok(Some(unsupported(key, format!("`{k}:` condition that is not a Symbol method name")))),
            },
            "on" => return Ok(Some(unsupported(key, "`on:` validation context"))),
            "strict" => return Ok(Some(unsupported(key, "`strict:` (raising validations)"))),
            "message" => match message(v, file)? {
                Ok(m) => options.message = Some(m),
                Err(detail) => return Ok(Some(unsupported(key, detail))),
            },
            _ => {
                own.retain(|(seen, _)| *seen != k);
                own.push((k, v));
            }
        }
    }
    let rule = match key {
        "presence" | "absence" => match own.first() {
            Some((k, _)) => Err(format!("`{k}:` option")),
            None if key == "presence" => Ok(ValidationRule::Presence),
            None => Ok(ValidationRule::Absence),
        },
        "length" | "size" => length_rule(&own),
        "inclusion" | "exclusion" => inclusion_rule(key == "exclusion", &own, file)?,
        "numericality" => numericality_rule(&own),
        "format" => format_rule(&own),
        _ => Err(match key {
            "uniqueness" => "a uniqueness check (a database query per validation) is not modeled".to_string(),
            other => format!("the `{other}` validator is not modeled"),
        }),
    };
    let rule = match rule {
        Ok(rule) => rule,
        Err(detail) => return Ok(Some(unsupported(key, detail))),
    };
    if let (Some(RuleMessage::Text { text }), false) = (&options.message, matches!(rule, ValidationRule::Length { .. })) {
        if text.contains("%{count}") {
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

/// `message:` as a String literal (its `%{...}` keys must be ones the
/// error supplies) or a callable constant / lambda literal.
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
    let expr = ingest_expr(value, file)?;
    let callable = match &*expr.node {
        ExprNode::Const { .. } | ExprNode::Lambda { .. } => true,
        ExprNode::Send { recv: None, method, block: Some(_), args, .. } => {
            args.is_empty() && matches!(method.as_str(), "lambda" | "proc")
        }
        _ => false,
    };
    if callable {
        return Ok(Ok(RuleMessage::Call { callable: expr }));
    }
    Ok(Err(match &*expr.node {
        ExprNode::Lit { value: Literal::Sym { .. } } => {
            "a Symbol `message:` (an I18n error type) is not modeled".to_string()
        }
        _ => "a `message:` that is not a String literal, a constant or a lambda".to_string(),
    }))
}

fn int_literal(v: &Node<'_>) -> Option<i64> {
    super::util::integer_value(v)
}

/// `length: { minimum:, maximum: }` or an Integer-literal `in:`/`within:`
/// Range.
fn length_rule(own: &[(&str, &Node<'_>)]) -> Result<ValidationRule, String> {
    let mut min: Option<u32> = None;
    let mut max: Option<u32> = None;
    let bound = |v: &Node<'_>, k: &str| -> Result<u32, String> {
        int_literal(v)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("`{k}:` that is not a non-negative Integer literal"))
    };
    for (k, v) in own {
        match *k {
            "minimum" => min = Some(bound(v, k)?),
            "maximum" => max = Some(bound(v, k)?),
            "in" | "within" => {
                let range = v.as_range_node().ok_or_else(|| format!("`{k}:` that is not a Range literal"))?;
                let lo = range.left().as_ref().map(|n| bound(n, k)).transpose()?;
                let hi = range.right().as_ref().map(|n| bound(n, k)).transpose()?;
                let exclusive = range.is_exclude_end();
                min = lo;
                max = match (hi, exclusive) {
                    (Some(h), true) => Some(h.checked_sub(1).ok_or_else(|| format!("an empty `{k}:` Range"))?),
                    (h, _) => h,
                };
            }
            other => return Err(format!("`{other}:` option")),
        }
    }
    if min.is_none() && max.is_none() {
        return Err("no `minimum:`, `maximum:`, `in:` or `within:` (ActiveModel raises at load)".into());
    }
    Ok(ValidationRule::Length { min, max, message: None })
}

fn inclusion_rule(
    exclusion: bool,
    own: &[(&str, &Node<'_>)],
    file: &str,
) -> IngestResult<Result<ValidationRule, String>> {
    let mut list: Option<&Node<'_>> = None;
    for (k, v) in own {
        match *k {
            "in" | "within" => list = Some(*v),
            other => return Ok(Err(format!("`{other}:` option"))),
        }
    }
    let Some(list) = list else {
        return Ok(Err("no `in:`/`within:` list (ActiveModel raises at load)".into()));
    };
    let expr = ingest_expr(list, file)?;
    match &*expr.node {
        ExprNode::Lit { value: Literal::Sym { .. } } => {
            return Ok(Err("an `in:` method name (evaluated on the record) is not modeled".into()))
        }
        ExprNode::Lambda { .. } => return Ok(Err("an `in:` lambda is not modeled".into())),
        ExprNode::Send { recv: None, method, block: Some(_), .. }
            if matches!(method.as_str(), "lambda" | "proc") =>
        {
            return Ok(Err("an `in:` lambda is not modeled".into()))
        }
        _ => {}
    }
    // Rails asks a Range over numbers (or times) `cover?`, any other
    // enumerable `include?`.
    let cover = matches!(&*expr.node, ExprNode::Range { begin, end, .. }
        if begin.as_ref().or(end.as_ref()).is_some_and(|e| matches!(&*e.node,
            ExprNode::Lit { value: Literal::Int { .. } | Literal::Float { .. } })));
    Ok(Ok(ValidationRule::Inclusion { list: expr, exclusion, cover }))
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

fn numericality_rule(own: &[(&str, &Node<'_>)]) -> Result<ValidationRule, String> {
    let mut only_integer = false;
    let mut found: Vec<NumericCheck> = Vec::new();
    for (k, v) in own {
        if *k == "only_integer" {
            only_integer = super::util::bool_value(v)
                .ok_or_else(|| "`only_integer:` that is not `true` or `false`".to_string())?;
            continue;
        }
        let Some(op) = NumericOp::from_key(k) else {
            return Err(format!("`{k}:` option"));
        };
        let bound = match op {
            // `odd:` / `even:` check parity whatever their value.
            NumericOp::Odd | NumericOp::Even => None,
            _ => Some(numeric_literal(v).ok_or_else(|| {
                format!("`{k}:` bound that is not an Integer or Float literal")
            })?),
        };
        found.retain(|c| c.op != op);
        found.push(NumericCheck { op, bound });
    }
    let checks = NumericOp::ORDER
        .into_iter()
        .filter_map(|op| found.iter().find(|c| c.op == op).cloned())
        .collect();
    Ok(ValidationRule::Numericality { only_integer, checks })
}

fn format_rule(own: &[(&str, &Node<'_>)]) -> Result<ValidationRule, String> {
    let mut found: Option<(String, String, bool)> = None;
    for (k, v) in own {
        match *k {
            "with" | "without" => {
                let re = v
                    .as_regular_expression_node()
                    .ok_or_else(|| format!("`{k}:` that is not a regexp literal"))?;
                let pattern = String::from_utf8_lossy(re.unescaped()).into_owned();
                let src = String::from_utf8_lossy(re.content_loc().as_slice()).into_owned();
                // FormatValidator#check_options_validity: a `^`/`$`
                // anchor without `multiline: true` raises at load.
                if src.starts_with('^') || (src.ends_with('$') && !src.ends_with("\\$")) {
                    return Err("a regexp with a `^`/`$` anchor (ActiveModel raises at load without `multiline: true`)".into());
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
                found = Some((pattern, flags, *k == "without"));
            }
            other => return Err(format!("`{other}:` option")),
        }
    }
    let (pattern, flags, without) =
        found.ok_or_else(|| "no `with:` or `without:` (ActiveModel raises at load)".to_string())?;
    Ok(ValidationRule::Format { pattern, flags, without })
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
    let mut if_method = None;
    let mut unless_method = None;
    for (k, v) in defaults.iter().chain(entries.iter()) {
        match k.as_str() {
            "on" | "strict" => return Ok(unsupported(key, format!("`{k}:` on a custom validator"))),
            "if" | "unless" => match symbol_value(v) {
                Some(m) if k == "if" => if_method = Some(Symbol::from(m.as_str())),
                Some(m) => unless_method = Some(Symbol::from(m.as_str())),
                None => return Ok(unsupported(key, format!("`{k}:` condition that is not a Symbol method name"))),
            },
            _ => {}
        }
        let value = ingest_expr(v, file)?;
        hash.retain(|(hk, _)| !matches!(&*hk.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == k));
        hash.push((sym(k), value));
    }
    let class = format!("{}Validator", crate::naming::camelize(key));
    Ok(ValidationRule::With {
        validator: Expr::new(s, ExprNode::Const { path: vec![Symbol::from(class.as_str())] }),
        options: Some(Expr::new(s, ExprNode::Hash { entries: hash, kwargs: false })),
        if_method,
        unless_method,
    })
}
