//! The columns of a model whose type an app macro can replace at load.
//!
//! Rails' `attribute name, type` (attribute_registration.rb) replaces a
//! column's cast type: reads, writes, hydration and binds then go through
//! the declared type object. A literal `attribute :col, :type` in the body
//! is applied statically (`markers::attribute_api_decls`). This module is
//! about the other case, a class-body call of an app class macro that
//! reaches `attribute` when it runs, as Core's
//! `model_uuid_attributes :id, :shop_id` does through
//! `register_uuid_attribute`. Which columns it retypes, and with which
//! type, is decided when the macro runs (Core's allowlist, its boot
//! configuration), so it is not decided here.
//!
//! What is decided here is which columns get the compiled path that
//! consults a declared type: the literal Symbol or String arguments of
//! such a call (in Array literals too) that name a column of the model's
//! table. The runtime's `Base.attribute` raises for any other name, so a
//! macro that retypes a column passed some other way fails loudly rather
//! than being ignored. The analyzer types these columns gradually and
//! reports `attribute_type_unevaluated` (`analyze::model_attributes`).
//!
//! The walk follows what runs at class-body time: the macro's body, the
//! class methods it calls on `self`, and the blocks of the calls it makes,
//! except blocks that run later or on another object (`define_method`,
//! `lambda`, `proc`, `Module.new`, `Class.new`). The class side is the
//! model's own class methods, its model ancestors' (concern
//! `class_methods` are spliced into them), and the instance methods of the
//! app modules its body or an ancestor's `extend`s.
//!
//! The same walk finds the body calls that reach `query_constraints`
//! (`query_constraint_columns`): Core's `yugabyte_query_constraints
//! :shop_id, :id` calls it, through a local bound to `self`, when its
//! configuration says so. The compiled update, destroy and reload carry
//! the composite WHERE for those columns and take it when the call ran.
//!
//! A direct `serialize :col, coder: Coder` with an app coder
//! (`serialized_column`) retypes its column too: Rails decorates the
//! column's type with Type::Serialized when the call runs, so the column
//! takes the same compiled path and the call runs at class load.
//!
//! So does a direct `attribute :col[, :type], default: value`
//! (`defaulted_attribute`), the primary key included (Core's `attribute
//! :id, :suid, default: -> { Shopify::Uuid.generate_new }`): the
//! compiled `initialize` starts a new record's attribute from the class's
//! declared default (`Model::defaulted_attributes`).

use crate::App;
use crate::dialect::{LibraryClass, MethodDef, MethodReceiver, Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::Symbol;

/// Calls whose block does not run in the class body.
const LATER_BLOCKS: &[&str] = &["define_method", "define_singleton_method", "lambda", "proc", "new", "instance_exec"];

/// The model's retypable columns, in table order: those its own body's
/// calls name, its model ancestors' (an STI subclass shares the table,
/// and Rails applies a superclass's declarations to it), and those of the
/// STI subclasses that ingest keeps as library classes: they inherit the
/// model's compiled accessors, and their own declarations apply to them
/// alone (the runtime's declared types are per class).
pub(crate) fn retypable_attributes(model: &Model, app: &App) -> Vec<Symbol> {
    let Some(table) = app.schema.tables.get(&model.table.0) else { return Vec::new() };
    let mut names: Vec<String> = Vec::new();
    let mut at = Some(model);
    let mut visited: Vec<&str> = Vec::new();
    while let Some(m) = at {
        if visited.contains(&m.name.0.as_str()) {
            break;
        }
        visited.push(m.name.0.as_str());
        for (_, call_names) in macro_calls(m, app, "attribute", false) {
            names.extend(call_names);
        }
        names.extend(body_statements(m).filter_map(|e| serialized_column(e, table)).map(|c| c.as_str().to_string()));
        at = m.parent.as_ref().and_then(|p| app.models.iter().find(|x| &x.name == p));
    }
    for (_, _, call_names) in sti_subclass_macro_calls(model, app, "attribute") {
        names.extend(call_names);
    }
    names.extend(defaulted_attributes(model, app).iter().map(|c| c.as_str().to_string()));
    table.columns.iter().filter(|c| names.iter().any(|n| n == c.name.as_str())).map(|c| c.name.clone()).collect()
}

/// The columns the direct `attribute ..., default:` calls
/// (`defaulted_attribute`) of the model, its model ancestors and its STI
/// subclasses name, in table order. Each class's declared default
/// applies to it and its subclasses (the runtime's per-class defaults).
pub(crate) fn defaulted_attributes(model: &Model, app: &App) -> Vec<Symbol> {
    let Some(table) = app.schema.tables.get(&model.table.0) else { return Vec::new() };
    let mut names: Vec<Symbol> = Vec::new();
    let mut at = Some(model);
    let mut visited: Vec<&str> = Vec::new();
    while let Some(m) = at {
        if visited.contains(&m.name.0.as_str()) {
            break;
        }
        visited.push(m.name.0.as_str());
        names.extend(body_statements(m).filter_map(|e| defaulted_attribute(e, table)));
        at = m.parent.as_ref().and_then(|p| app.models.iter().find(|x| &x.name == p));
    }
    for lc in app.library_classes.iter().filter(|lc| lc.parent.as_ref() == Some(&model.name)) {
        names.extend(lc.unknown_calls.iter().filter_map(|e| defaulted_attribute(e, table)));
    }
    table.columns.iter().filter(|c| names.contains(&c.name)).map(|c| c.name.clone()).collect()
}

/// A direct `attribute :col[, :type], default: value` body statement
/// over a column of `table`, the type (if
/// any) a Symbol: Rails records the type (the column's own when none is
/// named) and the value a new record's attribute starts from, a Proc
/// called for each record. The column takes the retypable path and the
/// call runs at class load as written (`ModelBodyItem::ClassBody`, or
/// carried by an STI subclass) for the runtime's `attribute`. Other
/// options and a non-Symbol type stay unclaimed. A retyped primary key
/// keys finds, binds and the INSERT through the declared type
/// (`adapter_emit::key_ty`).
pub(crate) fn defaulted_attribute(expr: &Expr, table: &crate::schema::Table) -> Option<Symbol> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return None };
    if method.as_str() != "attribute" || !on_self(recv) {
        return None;
    }
    let (name, options) = match args.as_slice() {
        [name, options] => (name, options),
        [name, ty, options] if matches!(&*ty.node, ExprNode::Lit { value: Literal::Sym { .. } }) => (name, options),
        _ => return None,
    };
    let name = match &*name.node {
        ExprNode::Lit { value: Literal::Sym { value } } => value.as_str().to_string(),
        ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
        _ => return None,
    };
    let ExprNode::Hash { entries, .. } = &*options.node else { return None };
    let [(key, _)] = entries.as_slice() else { return None };
    if !matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "default") {
        return None;
    }
    table.columns.iter().find(|c| c.name.as_str() == name).map(|c| c.name.clone())
}

/// The body calls of the model's STI subclasses (library classes whose
/// superclass is the model) of an app class macro reaching `target`,
/// each with its class and the literal names it passes; and their direct
/// `target name, :type` calls, with no options, each with its name (the
/// runtime's `attribute` ports neither `default:` nor options).
fn sti_subclass_macro_calls<'a>(
    model: &'a Model,
    app: &'a App,
    target: &str,
) -> Vec<(&'a LibraryClass, &'a Expr, Vec<String>)> {
    let mut out = Vec::new();
    for lc in app.library_classes.iter().filter(|lc| lc.parent.as_ref() == Some(&model.name)) {
        let side = ClassSide::of_sti_subclass(lc, model, app);
        for (call, names) in calls_reaching(&side, lc.unknown_calls.iter(), target, false) {
            out.push((lc, call, names));
        }
        for call in &lc.unknown_calls {
            let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { continue };
            if method.as_str() != target || !on_self(recv) {
                continue;
            }
            let [name, ty] = args.as_slice() else { continue };
            let ExprNode::Lit { value: Literal::Sym { .. } } = &*ty.node else { continue };
            let mut names = Vec::new();
            literal_names(name, &mut names);
            if names.len() == 1 && matches!(&*name.node, ExprNode::Lit { .. }) {
                out.push((lc, call, names));
            }
        }
    }
    out
}

/// `attribute_type_unevaluated` for each column a body call can retype,
/// at the call.
pub(crate) fn diagnose(app: &App) -> Vec<crate::diagnostic::Diagnostic> {
    use crate::diagnostic::{Diagnostic, DiagnosticKind};
    let mut out = Vec::new();
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else { continue };
        for (call, names) in macro_calls(model, app, "attribute", false) {
            let ExprNode::Send { method, .. } = &*call.node else { continue };
            for col in table.columns.iter().filter(|c| names.iter().any(|n| n == c.name.as_str())) {
                let kind = DiagnosticKind::AttributeTypeUnevaluated {
                    model: Symbol::from(model.name.0.as_str()),
                    attribute: col.name.clone(),
                    call: method.clone(),
                };
                let mut d = Diagnostic {
                    span: call.span,
                    severity: Diagnostic::default_severity(&kind),
                    kind,
                    message: String::new(),
                };
                d.message = Diagnostic::stub_text(&d.kind);
                out.push(d);
            }
        }
        for (lc, call, names) in sti_subclass_macro_calls(model, app, "attribute") {
            let ExprNode::Send { method, .. } = &*call.node else { continue };
            for col in table.columns.iter().filter(|c| names.iter().any(|n| n == c.name.as_str())) {
                let kind = DiagnosticKind::AttributeTypeUnevaluated {
                    model: Symbol::from(lc.name.0.as_str()),
                    attribute: col.name.clone(),
                    call: method.clone(),
                };
                let mut d = Diagnostic {
                    span: call.span,
                    severity: Diagnostic::default_severity(&kind),
                    kind,
                    message: String::new(),
                };
                d.message = Diagnostic::stub_text(&d.kind);
                out.push(d);
            }
        }
    }
    out
}

/// The model's composite primary key (`primary_key` is the list, `id`
/// the tuple of their values; `Model::composite_primary_key`): the
/// nearest list declaration (`self.primary_key = [..]`, which ingest
/// records on the model) of the model or a model ancestor, else the
/// table's composite primary key columns when none of them declares a
/// single key, which Rails then keys the model on.
pub(crate) fn composite_primary_key(model: &Model, app: &App) -> Vec<Symbol> {
    let Some(table) = app.schema.tables.get(&model.table.0) else { return Vec::new() };
    let mut at = Some(model);
    let mut visited: Vec<&str> = Vec::new();
    while let Some(m) = at {
        if visited.contains(&m.name.0.as_str()) {
            break;
        }
        visited.push(m.name.0.as_str());
        if !m.composite_primary_key.is_empty() {
            return m.composite_primary_key.clone();
        }
        if m.primary_key.is_some() {
            return Vec::new();
        }
        at = m.parent.as_ref().and_then(|p| app.models.iter().find(|x| &x.name == p));
    }
    table.primary_key_columns.clone()
}

/// The columns of the `query_constraints` call that decides the model's
/// list: its own body's last such call, direct or through an app class
/// macro (Core's `yugabyte_query_constraints :shop_id, :id`), else the
/// nearest model ancestor's, as Rails' `query_constraints_list` falls back
/// to the base class's. Empty unless every argument is a literal naming a
/// column of the table; the runtime's `query_constraints` then raises for
/// the list it is given rather than keying on the primary key alone.
pub(crate) fn query_constraint_columns(model: &Model, app: &App) -> Vec<Symbol> {
    let Some(table) = app.schema.tables.get(&model.table.0) else { return Vec::new() };
    let mut at = Some(model);
    let mut visited: Vec<&str> = Vec::new();
    while let Some(m) = at {
        if visited.contains(&m.name.0.as_str()) {
            break;
        }
        visited.push(m.name.0.as_str());
        if let Some((call, names)) = macro_calls(m, app, "query_constraints", true).pop() {
            let ExprNode::Send { args, .. } = &*call.node else { return Vec::new() };
            let columns = names.iter().all(|n| table.columns.iter().any(|c| c.name.as_str() == n));
            if names.is_empty() || !columns || !args.iter().all(literal_only) {
                return Vec::new();
            }
            return names.iter().map(|n| Symbol::from(n.as_str())).collect();
        }
        at = m.parent.as_ref().and_then(|p| app.models.iter().find(|x| &x.name == p));
    }
    Vec::new()
}

/// A direct `query_constraints :a, :b` body statement naming exactly the
/// compiled `columns` in order: Rails runs it at class load, so the
/// model carries it as written (`ModelBodyItem::ClassBody`) and the
/// runtime's `query_constraints` sets the list the compiled update,
/// destroy and reload consult.
pub(crate) fn carries_query_constraints(expr: &Expr, columns: &[Symbol]) -> bool {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return false };
    if method.as_str() != "query_constraints" || !on_self(recv) || !args.iter().all(literal_only) {
        return false;
    }
    let mut names = Vec::new();
    for arg in args {
        literal_names(arg, &mut names);
    }
    !columns.is_empty() && names.iter().map(String::as_str).eq(columns.iter().map(Symbol::as_str))
}

/// A direct `serialize :col, coder: Coder[, type: Class][, comparable:
/// true|false]` body statement over a string, text or binary (bytea)
/// column of `table`,
/// with an app's coder (not `JSON` or `YAML`: `lower::serialize` lowers
/// the JSON one, and Rails' own coders are not ported). Rails builds the
/// column serializer from the coder when the call runs and decorates the
/// column's type with Type::Serialized, so the column takes the
/// retypable path and the model carries the call as written
/// (`ModelBodyItem::ClassBody`) for the runtime's `serialize`. A json
/// column, `default:` and other options stay unclaimed.
pub(crate) fn serialized_column(expr: &Expr, table: &crate::schema::Table) -> Option<Symbol> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return None };
    if method.as_str() != "serialize" || !on_self(recv) {
        return None;
    }
    let [name, options] = args.as_slice() else { return None };
    let name = match &*name.node {
        ExprNode::Lit { value: Literal::Sym { value } } => value.as_str().to_string(),
        ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
        _ => return None,
    };
    let ExprNode::Hash { entries, .. } = &*options.node else { return None };
    let mut coder = false;
    for (key, value) in entries {
        let ExprNode::Lit { value: Literal::Sym { value: key } } = &*key.node else { return None };
        let fits = match key.as_str() {
            "coder" => {
                coder = true;
                !matches!(&*value.node, ExprNode::Const { path } if path.last().is_some_and(|l| matches!(l.as_str(), "JSON" | "YAML")))
            }
            "type" => matches!(&*value.node, ExprNode::Const { .. }),
            "comparable" => matches!(&*value.node, ExprNode::Lit { value: Literal::Bool { .. } }),
            _ => false,
        };
        if !fits {
            return None;
        }
    }
    let column = table.columns.iter().find(|c| c.name.as_str() == name)?;
    (coder && matches!(column.col_type, crate::schema::ColumnType::String { .. } | crate::schema::ColumnType::Text | crate::schema::ColumnType::Binary))
        .then(|| column.name.clone())
}

/// The spans of the body calls the stamped columns answer, every
/// argument a literal: the model's own app class macro calls that reach
/// `attribute` naming only `retypable_attributes`, or `query_constraints`
/// naming the compiled list. Such a call runs as written at class load
/// (the inherited-macro replay).
///
/// For an STI subclass (a library class to ingest whose superclass is
/// the model) both kinds, and its Ruby emit carries them as written
/// (`emit::ruby::modelled_class_body_calls`): a retyping call declares
/// the types on the subclass alone, as in Rails, and a
/// `query_constraints` one sets the model's list on it.
///
/// `model_to_library::unclaimed_unknowns` and the STI subclass check skip
/// these by span. Needs the model's `retypable_attributes` and
/// `query_constraints` stamped. The model's own `serialize` calls with an
/// app coder (`serialized_column`) are claimed too, and the direct
/// `attribute ..., default:` calls (`defaulted_attribute`) of the model
/// and its STI subclasses.
pub(crate) fn claimed_body_calls(model: &Model, app: &App) -> Vec<crate::span::Span> {
    let mut out = Vec::new();
    if let Some(table) = app.schema.tables.get(&model.table.0) {
        out.extend(body_statements(model).filter(|e| serialized_column(e, table).is_some()).map(|e| e.span));
        out.extend(body_statements(model).filter(|e| defaulted_attribute(e, table).is_some()).map(|e| e.span));
        for lc in app.library_classes.iter().filter(|lc| lc.parent.as_ref() == Some(&model.name)) {
            out.extend(lc.unknown_calls.iter().filter(|e| defaulted_attribute(e, table).is_some()).map(|e| e.span));
        }
    }
    let side = ClassSide::of(model, app);
    let body: Vec<&Expr> = body_statements(model).collect();
    for (call, names) in calls_reaching(&side, body.iter().copied(), "attribute", false) {
        if literal_call(call)
            && !names.is_empty()
            && names.iter().all(|n| model.retypable_attributes.iter().any(|c| c.as_str() == n))
        {
            out.push(call.span);
        }
    }
    for (_, call, names) in sti_subclass_macro_calls(model, app, "attribute") {
        if literal_call(call)
            && !names.is_empty()
            && names.iter().all(|n| model.retypable_attributes.iter().any(|c| c.as_str() == n))
        {
            out.push(call.span);
        }
    }
    out.extend(keyed_calls(model, &side, body));
    for lc in app.library_classes.iter().filter(|lc| lc.parent.as_ref() == Some(&model.name)) {
        out.extend(keyed_calls(model, &ClassSide::of_sti_subclass(lc, model, app), lc.unknown_calls.iter().collect()));
    }
    out
}

/// The calls in `body` reaching `query_constraints` with the model's
/// compiled list, every argument a literal.
fn keyed_calls<'a>(model: &Model, side: &ClassSide<'a>, body: Vec<&'a Expr>) -> Vec<crate::span::Span> {
    if model.query_constraints.is_empty() {
        return Vec::new();
    }
    calls_reaching(side, body.into_iter(), "query_constraints", false)
        .into_iter()
        .filter(|(call, names)| {
            literal_call(call) && names.iter().map(String::as_str).eq(model.query_constraints.iter().map(Symbol::as_str))
        })
        .map(|(call, _)| call.span)
        .collect()
}

fn literal_call(call: &Expr) -> bool {
    matches!(&*call.node, ExprNode::Send { args, .. } if args.iter().all(literal_only))
}

/// The model's own body calls of an app class macro that reaches `target`
/// on the class when it runs, each with the literal names it passes, in
/// body order; with `direct`, calls of `target` itself too.
fn macro_calls<'a>(model: &'a Model, app: &'a App, target: &str, direct: bool) -> Vec<(&'a Expr, Vec<String>)> {
    calls_reaching(&ClassSide::of(model, app), body_statements(model), target, direct)
}

/// [`macro_calls`] over the body statements `body` of the class `side`
/// describes.
fn calls_reaching<'a>(
    side: &ClassSide<'a>,
    body: impl Iterator<Item = &'a Expr>,
    target: &str,
    direct: bool,
) -> Vec<(&'a Expr, Vec<String>)> {
    let mut out = Vec::new();
    for call in body {
        let ExprNode::Send { recv, method, args, .. } = &*call.node else { continue };
        if !on_self(recv) {
            continue;
        }
        let reaches = if method.as_str() == target {
            direct
        } else {
            let Some(def) = side.class_method(method.as_str()) else { continue };
            let mut seen = vec![method.as_str().to_string()];
            side.reaches(&def.body, target, &self_aliases(&def.body), &mut seen)
        };
        if reaches {
            let mut names = Vec::new();
            for arg in args {
                literal_names(arg, &mut names);
            }
            out.push((call, names));
        }
    }
    out
}

fn on_self(recv: &Option<Expr>) -> bool {
    recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
}

/// `self`, or a local a method body bound to it (`untyped_self = self
/// #: as untyped`, then `untyped_self.query_constraints(*cols)`).
fn on_class(recv: &Option<Expr>, aliases: &[Symbol]) -> bool {
    on_self(recv)
        || recv.as_ref().is_some_and(|r| matches!(&*r.node, ExprNode::Var { name, .. } if aliases.contains(name)))
}

/// The locals a method body assigns `self` to.
fn self_aliases(body: &Expr) -> Vec<Symbol> {
    fn walk(e: &Expr, out: &mut Vec<Symbol>) {
        if let ExprNode::Assign { target: LValue::Var { name, .. }, value } = &*e.node {
            let value = match &*value.node {
                ExprNode::Cast { value, .. } => value,
                _ => value,
            };
            if matches!(&*value.node, ExprNode::SelfRef) {
                out.push(name.clone());
            }
        }
        e.node.for_each_child(&mut |child| walk(child, out));
    }
    let mut out = Vec::new();
    walk(body, &mut out);
    out
}

/// A Symbol or String literal, or an Array or splat of them.
fn literal_only(arg: &Expr) -> bool {
    match &*arg.node {
        ExprNode::Lit { value: Literal::Sym { .. } | Literal::Str { .. } } => true,
        ExprNode::Array { elements, .. } => elements.iter().all(literal_only),
        ExprNode::Splat { value } => literal_only(value),
        _ => false,
    }
}

fn body_statements(model: &Model) -> impl Iterator<Item = &Expr> {
    model.body.iter().filter_map(|item| match item {
        ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => Some(expr),
        _ => None,
    })
}

fn literal_names(arg: &Expr, out: &mut Vec<String>) {
    match &*arg.node {
        ExprNode::Lit { value: Literal::Sym { value } } => out.push(value.as_str().to_string()),
        ExprNode::Lit { value: Literal::Str { value } } => out.push(value.clone()),
        ExprNode::Array { elements, .. } => elements.iter().for_each(|e| literal_names(e, out)),
        ExprNode::Splat { value } => literal_names(value, out),
        _ => {}
    }
}

struct ClassSide<'a> {
    app: &'a App,
    own: Vec<&'a MethodDef>,
    chain: Vec<&'a Model>,
    extended: Vec<&'a LibraryClass>,
}

impl<'a> ClassSide<'a> {
    fn of(model: &'a Model, app: &'a App) -> Self {
        let own = model.methods().filter(|d| d.receiver == MethodReceiver::Class).collect();
        Self::over(own, model, model.parent.as_ref(), app)
    }

    /// An STI subclass (a library class whose superclass is `parent`):
    /// its own class methods, then `parent`'s chain.
    fn of_sti_subclass(lc: &'a LibraryClass, parent: &'a Model, app: &'a App) -> Self {
        let own = lc.methods.iter().filter(|d| d.receiver == MethodReceiver::Class).collect();
        let mut side = Self::over(own, parent, Some(&parent.name), app);
        for call in &lc.unknown_calls {
            let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { continue };
            if method.as_str() == "extend" && on_self(recv) {
                side.extended.extend(args.iter().filter_map(|a| module_named(app, lc.name.0.as_str(), a)));
            }
        }
        side
    }

    /// `own` class methods over the model chain from `parent`; the
    /// modules `model`'s body and the chain's `extend`.
    fn over(own: Vec<&'a MethodDef>, model: &'a Model, parent: Option<&'a crate::ident::ClassId>, app: &'a App) -> Self {
        let mut chain: Vec<&Model> = Vec::new();
        let mut next = parent;
        while let Some(name) = next {
            match app.models.iter().find(|m| &m.name == name) {
                Some(m) if !chain.iter().any(|seen| seen.name == m.name) => {
                    chain.push(m);
                    next = m.parent.as_ref();
                }
                _ => break,
            }
        }
        let mut extended = Vec::new();
        for m in std::iter::once(model).chain(chain.iter().copied()) {
            for call in body_statements(m) {
                let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { continue };
                if method.as_str() != "extend" || !on_self(recv) {
                    continue;
                }
                extended.extend(args.iter().filter_map(|a| module_named(app, m.name.0.as_str(), a)));
            }
        }
        Self { app, own, chain, extended }
    }

    fn class_method(&self, name: &str) -> Option<&'a MethodDef> {
        self.own
            .iter()
            .rev()
            .find(|d| d.name.as_str() == name)
            .copied()
            .or_else(|| {
                self.chain.iter().find_map(|m| {
                    m.methods().filter(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == name).last()
                })
            })
            .or_else(|| self.extended.iter().find_map(|m| self.module_method(m, name, &mut Vec::new())))
    }

    fn module_method(&self, m: &'a LibraryClass, name: &str, seen: &mut Vec<String>) -> Option<&'a MethodDef> {
        if seen.iter().any(|s| s == m.name.0.as_str()) {
            return None;
        }
        seen.push(m.name.0.as_str().to_string());
        m.methods.iter().filter(|d| d.receiver == MethodReceiver::Instance && d.name.as_str() == name).last().or_else(|| {
            m.includes.iter().find_map(|inc| {
                let inc = inc.0.as_str().trim_start_matches("::");
                let included =
                    self.app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == inc)?;
                self.module_method(included, name, seen)
            })
        })
    }

    /// Does running `expr` in the class body call `target` on the class?
    /// `aliases` are the locals of the enclosing method bound to `self`.
    fn reaches(&self, expr: &Expr, target: &str, aliases: &[Symbol], seen: &mut Vec<String>) -> bool {
        if let ExprNode::Send { recv, method, args, block, .. } = &*expr.node {
            let name = method.as_str();
            if on_class(recv, aliases) {
                if name == target {
                    return true;
                }
                if !seen.iter().any(|s| s == name) {
                    if let Some(def) = self.class_method(name) {
                        seen.push(name.to_string());
                        if self.reaches(&def.body, target, &self_aliases(&def.body), seen) {
                            return true;
                        }
                    }
                }
            }
            let later = LATER_BLOCKS.contains(&name);
            let block_runs = |b: &Expr, seen: &mut Vec<String>| match &*b.node {
                ExprNode::Lambda { body, .. } => self.reaches(body, target, aliases, seen),
                _ => self.reaches(b, target, aliases, seen),
            };
            return recv.iter().chain(args.iter()).any(|e| self.reaches(e, target, aliases, seen))
                || (!later && block.iter().any(|b| block_runs(b, seen)));
        }
        // A lambda literal's body runs when it is called, not here.
        if matches!(&*expr.node, ExprNode::Lambda { .. }) {
            return false;
        }
        let mut found = false;
        expr.node.for_each_child(&mut |child| {
            if !found && self.reaches(child, target, aliases, seen) {
                found = true;
            }
        });
        found
    }
}

/// The app module (not a gem's) a constant names, resolved from `owner`'s
/// namespace outwards.
fn module_named<'a>(app: &'a App, owner: &str, constant: &Expr) -> Option<&'a LibraryClass> {
    let ExprNode::Const { path } = &*constant.node else { return None };
    let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
    let written = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
    let find = |name: &str| app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == name);
    if rooted {
        return find(&written);
    }
    let scope: Vec<&str> = owner.split("::").collect();
    (0..scope.len()).rev().find_map(|n| {
        if n == 0 { find(&written) } else { find(&format!("{}::{written}", scope[..n].join("::"))) }
    })
}
