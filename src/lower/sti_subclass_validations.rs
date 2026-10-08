//! The validations an STI SUBCLASS body declares.
//!
//! `class Rooms::Open < Room; validates :name, presence: true; end`
//!
//! An STI subclass declares no table, so ingest keeps it as a library
//! class (see [`super::sti_subclass_callbacks`]) and its class-body
//! statements as expressions in `unknown_calls`. The model path that
//! lowers `validates` / `validate` / `validates_with` into the record's
//! `validate` walks `app.models`, so these declarations were dropped:
//! the subclass accepted every record its own rules reject.
//!
//! Rails runs a subclass's validations for instances of that subclass
//! (and its own subclasses) only, after the ones it inherits. The
//! lowering is the same shape: the subclass gets
//!
//! ```text
//! def validate
//!   super
//!   <the checks its own declarations lower to>
//! end
//! ```
//!
//! with the checks built by the model path's own generator
//! ([`validation_statements`]) against the base model's attributes, as
//! the subclass sees them, and with the subclass as the declaring class:
//! `%{model}` and a missing translation name the subclass (then its
//! parents up to the base, ActiveRecord's `lookup_ancestors`), and a
//! `uniqueness:` check asks the subclass's relation, which keeps the
//! type condition (`type IN` the subclass and every class below it).
//!
//! The declarations are read again from source, at their own offsets,
//! by the same readers the model path uses (`ingest::validates`); a
//! `validate do ... end` keeps the block expression ingest already
//! built, which the earlier passes have rewritten like any other body.
//! A declaration with a validator or option those readers do not model
//! is reported by `roundhouse check` ([`diagnose`]) and stays in
//! `unknown_calls`; one that cannot be re-read is reported the same way.
//!
//! `acceptance:` on an attribute the class neither stores nor reads
//! (`validates :terms, acceptance: true`, a form's checkbox) has
//! ActiveModel define it: AcceptanceValidator includes a module that
//! defines `attr_reader` / `attr_writer` for each half that is not an
//! attribute method of the declaring class. [`define_acceptance_attributes`]
//! does the same before the model lowering runs: a model gets the
//! `attr_*` declaration (which its constructor and `update` then assign
//! like any virtual), an STI subclass its reader and writer and the
//! assignment in its constructor and `update` / `update!`.

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::dialect::{LibraryClass, MethodReceiver, Model, Validation, ValidationRule};
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};
use crate::lower::model_to_library::validations::{
    push_active_model_validation_surface, validate_method_def, validation_statements_split,
    Declarer, Finder,
};
use crate::span::Span;

/// Is `call` a validation declaration (`validates`, `validates!`, `validate`,
/// `validates_with`, `validates_<key>_of`, `validates_associated`,
/// `validates_each`) this pass answers for? `check`
/// asks before the pass runs; what it cannot lower, [`diagnose`] reports.
pub(crate) fn claims(call: &Expr) -> bool {
    let ExprNode::Send { recv: None, method, .. } = &*call.node else { return false };
    let m = method.as_str();
    m == "validates"
        || m == "validates!"
        || m == "validate"
        || m == "validates_with"
        || m == "validates_associated"
        || m == "validates_each"
        || (m.starts_with("validates_") && m.ends_with("_of"))
}

/// One subclass's validation declarations, as read.
struct Read {
    /// Each declaration lowered: its index in `unknown_calls`, its span,
    /// what it declares, and whether every rule is modeled.
    declared: Vec<(usize, Span, Vec<Validation>, bool)>,
    /// Declarations that could not be read or lowered at all, with why.
    gaps: Vec<(Span, String, String)>,
}

/// The nearest model ancestor of `lc` and the classes from `lc` up to
/// it (`lc` first, the model last), through intermediate library classes.
fn model_ancestry<'a>(lc: &LibraryClass, app: &'a App) -> Option<(&'a Model, Vec<ClassId>)> {
    let mut chain = vec![lc.name.clone()];
    let mut cursor = lc.parent.clone();
    for _ in 0..8 {
        let parent = cursor?;
        if let Some(model) = app.models.iter().find(|m| m.name == parent) {
            chain.push(model.name.clone());
            return Some((model, chain));
        }
        if chain.contains(&parent) {
            return None;
        }
        let next = app.library_classes.iter().find(|c| c.name == parent)?;
        chain.push(parent);
        cursor = next.parent.clone();
    }
    None
}

fn construct_of(call: &Expr) -> String {
    match &*call.node {
        ExprNode::Send { method, .. } => format!("`{}`", method.as_str()),
        _ => "validation".to_string(),
    }
}

fn read(lc: &LibraryClass, app: &App) -> Read {
    let mut out = Read { declared: Vec::new(), gaps: Vec::new() };
    // Hand-written, that is: the `validate` this pass synthesizes has
    // no source name, and `check` may ask after the pass ran.
    let defines_validate = lc.methods.iter().any(|m| {
        m.name.as_str() == "validate" && m.receiver == MethodReceiver::Instance && !m.name_span.is_synthetic()
    });
    for (i, call) in lc.unknown_calls.iter().enumerate() {
        if !claims(call) {
            continue;
        }
        if defines_validate {
            out.gaps.push((
                call.span,
                construct_of(call),
                format!(
                    "`{}` defines its own `validate`: in Rails that replaces only the `valid?` alias, and the emitted record runs its validations through `validate`",
                    lc.name.0.as_str()
                ),
            ));
            continue;
        }
        let source = (call.span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i));
        let read = source.and_then(|src| {
            crate::ingest::validates::validations_in_source(
                &src.text,
                &src.path,
                call.span.start as usize,
                call.span.end as usize,
            )
        });
        let mut validations = match read {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                out.gaps.push((call.span, construct_of(call), format!("its declaration did not read ({e})")));
                continue;
            }
            None => {
                out.gaps.push((
                    call.span,
                    construct_of(call),
                    "its declaration could not be re-read from source".to_string(),
                ));
                continue;
            }
        };
        // The re-read runs outside ingest's source registry, so its spans
        // name no file; the strict exception class's is the one a
        // target without it reports at.
        for v in &mut validations {
            for rule in &mut v.rules {
                if let ValidationRule::Guarded { options, .. } = rule {
                    if let Some(exception) = &mut options.strict {
                        if exception.span.file.0 == 0 && !exception.span.is_synthetic() {
                            exception.span.file = call.span.file;
                        }
                    }
                }
            }
        }
        // `validate do ... end` / `validates_each ... do`: the block
        // ingest built (and the passes before this one rewrote), not the
        // re-read copy.
        if let ExprNode::Send { block: Some(block), .. } = &*call.node {
            let callable = if construct_of(call) == "`validates_each`" {
                crate::ingest::validates::each_callable(block.clone()).ok().map(|(c, _)| c)
            } else {
                match crate::ingest::validates::block_callable(block.clone()) {
                    Ok(ValidationRule::Block { callable, .. }) => Some(callable),
                    _ => None,
                }
            };
            if let Some(callable) = callable {
                for v in &mut validations {
                    for rule in &mut v.rules {
                        if let Some(body) = rule.block_body_mut() {
                            *body = callable.clone();
                        }
                    }
                }
            }
        }
        let modeled = !validations
            .iter()
            .any(|v| v.rules.iter().any(|r| matches!(r, ValidationRule::Unsupported { .. })));
        out.declared.push((i, call.span, validations, modeled));
    }
    out
}

/// The STI type names a subclass's relation keeps: its own and every
/// library class's below it.
fn type_names(sub: &ClassId, app: &App) -> Vec<String> {
    let mut names = vec![sub.0.as_str().to_string()];
    for lc in &app.library_classes {
        let mut cursor = lc.parent.as_ref();
        for _ in 0..8 {
            let Some(parent) = cursor else { break };
            if parent == sub {
                if !names.iter().any(|n| n == lc.name.0.as_str()) {
                    names.push(lc.name.0.as_str().to_string());
                }
                break;
            }
            cursor = app.library_classes.iter().find(|c| &c.name == parent).and_then(|c| c.parent.as_ref());
        }
    }
    names
}

pub fn apply_sti_subclass_validations(app: &mut App) {
    define_acceptance_attributes(app);
    let sti = crate::lower::sti_bases(app);
    let mut plans: Vec<(usize, Vec<usize>, Vec<crate::dialect::MethodDef>)> = Vec::new();
    for (index, lc) in app.library_classes.iter().enumerate() {
        let Some((model, chain)) = model_ancestry(lc, app) else { continue };
        let read = read(lc, app);
        if read.declared.is_empty() {
            continue;
        }
        let declarer = Declarer {
            name: lc.name.clone(),
            i18n_keys: chain.iter().map(|c| crate::naming::underscore(c.0.as_str())).collect(),
            finder: Finder::Declaring {
                class: lc.name.clone(),
                type_names: if sti.contains_key(&lc.name) { type_names(&lc.name, app) } else { Vec::new() },
            },
        };
        let spanned = read.declared.iter().flat_map(|(_, span, vs, _)| vs.iter().map(move |v| (*span, v)));
        // `prepend: true` checks run ahead of the inherited ones.
        let (front, checks) = validation_statements_split(model, spanned, &declarer);
        let claimed: Vec<usize> =
            read.declared.iter().filter(|(_, _, _, modeled)| *modeled).map(|(i, ..)| *i).collect();
        if front.is_empty() && checks.is_empty() && claimed.is_empty() {
            continue;
        }
        // The inherited checks: every model ancestor has a `validate` (an
        // ActiveRecord one the runtime Base's when it declares none, a
        // tableless one its lowered one, rules or not).
        let tableless = model.parent.is_none();
        let mut stmts = front;
        stmts.push(Expr::new(Span::synthetic(), ExprNode::Super { args: Some(Vec::new()), block: None }));
        stmts.extend(checks);
        let mut methods = vec![validate_method_def(&lc.name, stmts)];
        // A tableless class's `valid?` / `errors` (and the context its
        // `on:` rules read) are synthesized with its validations; the
        // subclass's own may need what the base did not.
        if tableless {
            let mut own = model.clone();
            own.name = lc.name.clone();
            own.body.extend(read.declared.iter().flat_map(|(_, span, vs, _)| {
                vs.iter().map(|v| crate::dialect::ModelBodyItem::Validation {
                    validation: v.clone(),
                    leading_comments: Vec::new(),
                    leading_blank_line: false,
                    span: *span,
                })
            }));
            push_active_model_validation_surface(&mut methods, &own);
        }
        plans.push((index, claimed, methods));
    }
    for (index, claimed, methods) in plans {
        let lc = &mut app.library_classes[index];
        let mut i = 0;
        lc.unknown_calls.retain(|_| {
            let keep = !claimed.contains(&i);
            i += 1;
            keep
        });
        lc.methods.extend(methods);
    }
}

/// The attributes `validations` run `acceptance:` on.
fn accepted_attributes<'v>(validations: impl Iterator<Item = &'v Validation>) -> Vec<Symbol> {
    fn accepts(rule: &ValidationRule) -> bool {
        match rule {
            ValidationRule::Acceptance { .. } => true,
            ValidationRule::Guarded { rule, .. } => accepts(rule),
            _ => false,
        }
    }
    let mut out: Vec<Symbol> = Vec::new();
    for v in validations {
        if v.rules.iter().any(accepts) && !out.contains(&v.attribute) {
            out.push(v.attribute.clone());
        }
    }
    out
}

/// The names `attr_accessor` / `attr_reader` / `attr_writer` declare
/// in a model body, as (name, reader, writer).
fn declared_attrs(model: &Model) -> Vec<(Symbol, bool, bool)> {
    let mut out = Vec::new();
    for item in &model.body {
        let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else { continue };
        let (reader, writer) = match method.as_str() {
            "attr_accessor" => (true, true),
            "attr_reader" => (true, false),
            "attr_writer" => (false, true),
            _ => continue,
        };
        for arg in args {
            if let ExprNode::Lit { value: crate::expr::Literal::Sym { value } } = &*arg.node {
                out.push((value.clone(), reader, writer));
            }
        }
    }
    out
}

/// Is `name` an attribute method of `model` (a column, an Attributes
/// API attribute) or an association, which ActiveModel's acceptance
/// leaves alone?
fn stored(model: &Model, name: &Symbol) -> bool {
    model.attributes.fields.contains_key(name)
        || model.associations().any(|a| a.name() == name)
        || crate::lower::model_to_library::attribute_api_decls(&model.body).iter().any(|(n, _)| n == name)
}

/// [`ValidationRule::Acceptance`]'s virtual attributes (see the module
/// header).
fn define_acceptance_attributes(app: &mut App) {
    // Models: the `attr_*` halves the model does not define.
    for model in &mut app.models {
        let names = accepted_attributes(model.validations());
        if names.is_empty() {
            continue;
        }
        let declared = declared_attrs(model);
        let mut additions = Vec::new();
        for name in names {
            if stored(model, &name) {
                continue;
            }
            let writer = Symbol::from(format!("{}=", name.as_str()));
            let defines = |n: &Symbol| crate::lower::model_to_library::model_defines_instance_method(model, n);
            let reader_missing = !defines(&name) && !declared.iter().any(|(n, r, _)| n == &name && *r);
            let writer_missing = !defines(&writer) && !declared.iter().any(|(n, _, w)| n == &name && *w);
            let macro_name = match (reader_missing, writer_missing) {
                (true, true) => "attr_accessor",
                (true, false) => "attr_reader",
                (false, true) => "attr_writer",
                (false, false) => continue,
            };
            let s = Span::synthetic();
            additions.push(crate::dialect::ModelBodyItem::Unknown {
                expr: Expr::new(
                    s,
                    ExprNode::Send {
                        recv: None,
                        method: Symbol::from(macro_name),
                        args: vec![Expr::new(s, ExprNode::Lit { value: crate::expr::Literal::Sym { value: name } })],
                        block: None,
                        parenthesized: false,
                    },
                ),
                leading_comments: Vec::new(),
                leading_blank_line: false,
            });
        }
        model.body.extend(additions);
    }
    // STI subclasses: the reader and writer, and the constructor's and
    // `update`'s assignment the base's attrs loop does not know.
    let mut plans: Vec<(usize, Vec<Symbol>)> = Vec::new();
    for (index, lc) in app.library_classes.iter().enumerate() {
        let Some((model, _)) = model_ancestry(lc, app) else { continue };
        let read = read(lc, app);
        let names: Vec<Symbol> = accepted_attributes(read.declared.iter().flat_map(|(_, _, vs, _)| vs.iter()))
            .into_iter()
            .filter(|n| !stored(model, n))
            .collect();
        if !names.is_empty() {
            plans.push((index, names));
        }
    }
    for (index, names) in plans {
        let lc = &mut app.library_classes[index];
        for name in names {
            define_virtual_on_subclass(lc, &name);
        }
    }
}

fn instance_method(owner: &ClassId, name: &str, params: Vec<crate::dialect::Param>, body: Expr) -> crate::dialect::MethodDef {
    crate::dialect::MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Instance,
        params,
        body,
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: crate::dialect::AccessorKind::Method,
        is_async: false,
        mutates_self: true,
        block_param: None,
    }
}

fn param(name: &str, default: Option<Expr>) -> crate::dialect::Param {
    crate::dialect::Param {
        name: Symbol::from(name),
        default,
        keyword: false,
        rest: false,
        forwarding: false,
        from_keyword: false,
        from_kwrest: false,
    }
}

/// `name` / `name=` on an STI subclass, unless it defines them, and
/// `self.name = attrs[:name] unless attrs[:name].nil?` (the base's
/// virtual-attribute assignment) in its synthesized constructor and in
/// an `update` / `update!` that then hands `attrs` to the base's.
fn define_virtual_on_subclass(lc: &mut LibraryClass, name: &Symbol) {
    let s = Span::synthetic();
    let node = |n| Expr::new(s, n);
    let var = |n: &str| node(ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from(n) });
    let defines = |lc: &LibraryClass, n: &str| {
        lc.methods.iter().any(|m| m.name.as_str() == n && m.receiver == MethodReceiver::Instance)
    };
    let writer = format!("{}=", name.as_str());
    if !defines(lc, name.as_str()) {
        let body = node(ExprNode::Ivar { name: name.clone() });
        lc.methods.push(instance_method(&lc.name, name.as_str(), Vec::new(), body));
    }
    if !defines(lc, &writer) {
        let body = node(ExprNode::Assign { target: crate::expr::LValue::Ivar { name: name.clone() }, value: var("value") });
        lc.methods.push(instance_method(&lc.name, &writer, vec![param("value", None)], body));
    }
    let lookup = || {
        node(ExprNode::Send {
            recv: Some(var("attrs")),
            method: Symbol::from("[]"),
            args: vec![node(ExprNode::Lit { value: crate::expr::Literal::Sym { value: name.clone() } })],
            block: None,
            parenthesized: false,
        })
    };
    let assign = || {
        let given = node(ExprNode::Send {
            recv: Some(node(ExprNode::Send {
                recv: Some(lookup()),
                method: Symbol::from("nil?"),
                args: Vec::new(),
                block: None,
                parenthesized: false,
            })),
            method: Symbol::from("!"),
            args: Vec::new(),
            block: None,
            parenthesized: false,
        });
        node(ExprNode::If {
            cond: given,
            then_branch: node(ExprNode::Send {
                recv: Some(node(ExprNode::SelfRef)),
                method: Symbol::from(writer.as_str()),
                args: vec![lookup()],
                block: None,
                parenthesized: false,
            }),
            else_branch: node(ExprNode::Lit { value: crate::expr::Literal::Nil }),
        })
    };
    let stmts_of = |body: &Expr| match &*body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![body.clone()],
    };
    let super_attrs = || node(ExprNode::Super { args: Some(vec![var("attrs")]), block: None });
    // The constructor: the one `sti_subclass_callbacks` stamped (or a new
    // one), never a hand-written one (`diagnose` reports that).
    match lc.methods.iter_mut().find(|m| m.name.as_str() == "initialize" && m.receiver == MethodReceiver::Instance) {
        Some(init) if init.name_span.is_synthetic() => {
            let mut stmts = stmts_of(&init.body);
            stmts.push(assign());
            init.body = node(ExprNode::Seq { exprs: stmts });
        }
        Some(_) => {}
        None => {
            let empty = node(ExprNode::Hash { entries: Vec::new(), kwargs: false });
            let mut init = instance_method(
                &lc.name,
                "initialize",
                vec![param("attrs", Some(empty))],
                node(ExprNode::Seq { exprs: vec![super_attrs(), assign()] }),
            );
            init.visibility = crate::dialect::MethodVisibility::Private;
            lc.methods.insert(0, init);
        }
    }
    for update in ["update", "update!"] {
        match lc.methods.iter_mut().find(|m| m.name.as_str() == update && m.receiver == MethodReceiver::Instance) {
            Some(existing) if existing.name_span.is_synthetic() => {
                let mut stmts = stmts_of(&existing.body);
                stmts.insert(0, assign());
                existing.body = node(ExprNode::Seq { exprs: stmts });
            }
            Some(_) => {}
            None => lc.methods.push(instance_method(
                &lc.name,
                update,
                vec![param("attrs", None)],
                node(ExprNode::Seq { exprs: vec![assign(), super_attrs()] }),
            )),
        }
    }
}

/// What `roundhouse check` reports for STI subclass validations: each
/// validator or option the readers do not model, and each declaration
/// this pass cannot lower.
pub(crate) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for lc in &app.library_classes {
        if model_ancestry(lc, app).is_none() {
            continue;
        }
        let read = read(lc, app);
        for (span, construct, detail) in read.gaps {
            out.push(Diagnostic::unsupported(
                span,
                None,
                Symbol::from(construct.as_str()),
                format!("STI subclass `{}`: {detail}; the class does not run it", lc.name.0.as_str()),
            ));
        }
        for (_, span, validations, _) in &read.declared {
            for v in validations {
                for rule in &v.rules {
                    let ValidationRule::Unsupported { construct, detail } = rule else { continue };
                    out.push(Diagnostic::unsupported(
                        *span,
                        None,
                        construct.as_str(),
                        format!(
                            "on `{}` in STI subclass `{}`, {detail}; the emitted class would skip this check",
                            v.attribute.as_str(),
                            lc.name.0.as_str()
                        ),
                    ));
                }
            }
        }
    }
    out
}
