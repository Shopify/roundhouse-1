//! Which database each model runs on, read from the app's own classes
//! (see [`crate::database`]). Never from the manifest: the manifest says
//! which databases exist; the source says who uses them.
//!
//! A model's database is decided by the nearest class in its ancestry
//! (itself first) that says anything about connections:
//!
//! * `connects_to database: { writing: :name, reading: :name }`, as a
//!   literal, unconditional statement of an abstract class body (Rails
//!   raises for a concrete one) → [`ModelDatabase::Named`];
//! * a class-side `connection_pool` / `lease_connection` / `connection`
//!   / `retrieve_connection` / `connection_specification_name` the app
//!   defines, on the class itself, through a concern's `class_methods`
//!   (spliced onto the includer by then), or by `extend`ing a module →
//!   [`ModelDatabase::AppDefined`]: which database a query reaches is
//!   app code;
//! * any other `connects_to` (computed, under a condition, `shards:`)
//!   or an `establish_connection` → [`ModelDatabase::Unresolved`].
//!
//! Otherwise the model runs on the primary. Every outcome the emitted
//! runtime does not carry becomes an [`App::database_refusals`] entry,
//! once per declaring site, counting the models it decides:
//! a database other than the primary (the runtime opens only the
//! primary), an adapter without a runtime, a `connects_to` without a
//! manifest to resolve its names, and each of the shapes above. A
//! role or shard switch (`connected_to`, `connecting_to`,
//! `while_preventing_writes`) anywhere in the app is refused at its call.

use crate::det::HashMap;

use crate::database::{DatabaseRefusal, ModelDatabase};
use crate::dialect::{MethodReceiver, Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::App;

/// Class-side methods through which Rails finds a class's connection.
/// An app that defines one routes its queries itself.
const CONNECTION_LOOKUPS: &[&str] = &[
    "connection_pool",
    "lease_connection",
    "connection",
    "with_connection",
    "retrieve_connection",
    "connection_specification_name",
];

/// Calls that switch the role or shard a block runs on.
const SWITCHES: &[&str] = &[
    "connected_to",
    "connected_to_many",
    "connecting_to",
    "while_preventing_writes",
    "prohibit_shard_swapping",
];

/// What one class body itself says about its connection.
#[derive(Clone)]
enum Own {
    Named { writing: String, reading: Option<String>, span: Span },
    AppDefined { method: Symbol, defined_by: ClassId, span: Span },
    Unresolved { construct: &'static str, reason: String, span: Span },
}

fn on_self(recv: &Option<Expr>) -> bool {
    recv.as_ref().is_none_or(|r| matches!(*r.node, ExprNode::SelfRef))
}

fn sym(e: &Expr) -> Option<String> {
    match &*e.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        _ => None,
    }
}

/// `{ writing: :a, reading: :b }` with symbol keys and values only.
fn roles(e: &Expr) -> Option<(Option<String>, Option<String>)> {
    let ExprNode::Hash { entries, .. } = &*e.node else { return None };
    let (mut writing, mut reading) = (None, None);
    for (k, v) in entries {
        let slot = match sym(k)?.as_str() {
            "writing" => &mut writing,
            "reading" => &mut reading,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(sym(v)?);
    }
    Some((writing, reading))
}

/// A top-level `connects_to` statement, classified.
fn connects_to(model: &Model, args: &[Expr], block: &Option<Expr>, span: Span) -> Own {
    let class = model.name.0.as_str();
    let unresolved = |reason: String| Own::Unresolved { construct: "connects_to", reason, span };
    if block.is_some() {
        return unresolved(format!("`connects_to` on `{class}` passes a block"));
    }
    let [ExprNode::Hash { entries, .. }] = args.iter().map(|a| &*a.node).collect::<Vec<_>>()[..] else {
        return unresolved(format!(
            "`connects_to` on `{class}` is not a literal `database: {{ writing: :name, reading: :name }}`"
        ));
    };
    let mut database = None;
    for (k, v) in entries {
        match sym(k).as_deref() {
            Some("shards") => {
                return unresolved(format!(
                    "`connects_to shards:` on `{class}` declares horizontal shards; shard routing is not modeled"
                ));
            }
            Some("database") if database.is_none() => database = Some(v),
            _ => {
                return unresolved(format!(
                    "`connects_to` on `{class}` is not a literal `database: {{ writing: :name, reading: :name }}`"
                ));
            }
        }
    }
    let Some((Some(writing), reading)) = database.and_then(roles) else {
        return unresolved(format!(
            "`connects_to` on `{class}` is not a literal `database: {{ writing: :name, reading: :name }}`"
        ));
    };
    if !crate::lower::model_to_library::markers::is_abstract_class(model) {
        return unresolved(format!(
            "`connects_to` on `{class}`, which is not an abstract class; Rails raises NotImplementedError"
        ));
    }
    Own::Named { writing, reading, span }
}

/// Every call to one of `names` under `expr` (receiver ignored).
fn calls_named(expr: &Expr, names: &[&str], out: &mut Vec<(Span, String)>) {
    if let ExprNode::Send { method, .. } = &*expr.node {
        if names.contains(&method.as_str()) {
            out.push((expr.span, method.as_str().to_string()));
        }
    }
    expr.node.for_each_child(&mut |child| calls_named(child, names, out));
}

/// The first thing `model`'s own body says about its connection, plus
/// the refusals for anything else it says.
fn own_decision(
    model: &Model,
    modules: &HashMap<&str, &crate::dialect::LibraryClass>,
    spliced_from: Option<&HashMap<Symbol, ClassId>>,
) -> Option<Own> {
    let mut found: Vec<Own> = Vec::new();
    for item in &model.body {
        match item {
            ModelBodyItem::Unknown { expr, .. } => {
                if let ExprNode::Send { recv, method, args, block, .. } = &*expr.node {
                    if on_self(recv) {
                        match method.as_str() {
                            "connects_to" => {
                                found.push(connects_to(model, args, block, expr.span));
                                continue;
                            }
                            "establish_connection" => {
                                found.push(Own::Unresolved {
                                    construct: "establish_connection",
                                    reason: format!(
                                        "`establish_connection` on `{}` sets a connection the manifest does not describe; not modeled",
                                        model.name.0.as_str()
                                    ),
                                    span: expr.span,
                                });
                                continue;
                            }
                            "extend" => {
                                for arg in args {
                                    let ExprNode::Const { path } = &*arg.node else { continue };
                                    let name = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
                                    let Some(module) = modules.get(name.as_str()) else { continue };
                                    if let Some(m) = module.methods.iter().find(|m| {
                                        m.receiver == MethodReceiver::Instance && CONNECTION_LOOKUPS.contains(&m.name.as_str())
                                    }) {
                                        found.push(Own::AppDefined {
                                            method: m.name.clone(),
                                            defined_by: module.name.clone(),
                                            span: m.name_span,
                                        });
                                    }
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                // A connecting call nested in a statement (under `if`,
                // in a block) runs conditionally or at another time.
                let mut nested = Vec::new();
                calls_named(expr, &["connects_to", "establish_connection"], &mut nested);
                for (span, method) in nested {
                    found.push(Own::Unresolved {
                        construct: if method == "connects_to" { "connects_to" } else { "establish_connection" },
                        reason: format!(
                            "`{method}` on `{}` runs under a condition or inside another statement; only an unconditional class-body statement is modeled",
                            model.name.0.as_str()
                        ),
                        span,
                    });
                }
            }
            ModelBodyItem::Method { method, .. }
                if method.receiver == MethodReceiver::Class
                    && CONNECTION_LOOKUPS.contains(&method.name.as_str()) =>
            {
                let defined_by = spliced_from
                    .and_then(|m| m.get(&method.name))
                    .cloned()
                    .unwrap_or_else(|| model.name.clone());
                found.push(Own::AppDefined { method: method.name.clone(), defined_by, span: method.name_span });
            }
            _ => {}
        }
    }
    // A class body that says two things is unresolved, at the second.
    match found.len() {
        0 => None,
        1 => found.pop(),
        _ => {
            let first = found.remove(0);
            let second_span = match &found[0] {
                Own::Named { span, .. } | Own::AppDefined { span, .. } | Own::Unresolved { span, .. } => *span,
            };
            match first {
                Own::Unresolved { .. } => Some(first),
                _ => Some(Own::Unresolved {
                    construct: "connects_to",
                    reason: format!(
                        "`{}` declares its connection more than once; which declaration wins is not modeled",
                        model.name.0.as_str()
                    ),
                    span: second_span,
                }),
            }
        }
    }
}

/// The model a written superclass names, from `child`'s namespace out.
fn resolve_parent<'a>(child: &str, parent: &str, by_name: &HashMap<&'a str, usize>) -> Option<usize> {
    let parent = parent.trim_start_matches("::");
    let mut scope: Vec<&str> = child.split("::").collect();
    scope.pop();
    loop {
        let candidate =
            if scope.is_empty() { parent.to_string() } else { format!("{}::{parent}", scope.join("::")) };
        if let Some(i) = by_name.get(candidate.as_str()) {
            return Some(*i);
        }
        if scope.pop().is_none() {
            return None;
        }
    }
}

/// Record `app.model_databases` and the routing refusals. Runs after
/// every model, concern splice and library class exists.
pub(super) fn resolve(app: &mut App) {
    let modules: HashMap<&str, &crate::dialect::LibraryClass> =
        app.library_classes.iter().filter(|c| c.is_module).map(|c| (c.name.0.as_str(), c)).collect();
    let by_name: HashMap<&str, usize> =
        app.models.iter().enumerate().map(|(i, m)| (m.name.0.as_str(), i)).collect();
    let own: Vec<Option<Own>> = app
        .models
        .iter()
        .map(|m| own_decision(m, &modules, app.concern_spliced_class_methods.get(&m.name)))
        .collect();
    let parents: Vec<Option<usize>> = app
        .models
        .iter()
        .map(|m| m.parent.as_ref().and_then(|p| resolve_parent(m.name.0.as_str(), p.0.as_str(), &by_name)))
        .collect();

    // Nearest deciding class in each model's ancestry.
    let deciding: Vec<Option<usize>> = (0..app.models.len())
        .map(|i| {
            let mut at = Some(i);
            let mut steps = 0;
            while let Some(j) = at {
                if own[j].is_some() {
                    return Some(j);
                }
                at = parents[j];
                steps += 1;
                if steps > app.models.len() {
                    return None;
                }
            }
            None
        })
        .collect();

    let mut refusals: Vec<DatabaseRefusal> = Vec::new();
    // Refusal per deciding site (span), with the models it decides.
    let mut by_site: Vec<(Span, &'static str, String, Vec<String>)> = Vec::new();
    let mut note = |span: Span, construct: &'static str, detail: String, model: &str| {
        match by_site.iter_mut().find(|(s, c, d, _)| *s == span && *c == construct && *d == detail) {
            Some((_, _, _, models)) => models.push(model.to_string()),
            None => by_site.push((span, construct, detail, vec![model.to_string()])),
        }
    };
    let have_manifest = !app.databases.is_empty();
    let mut owners = indexmap::IndexMap::new();
    for (i, model) in app.models.iter().enumerate() {
        let name = model.name.0.as_str();
        let ownership = match deciding[i].and_then(|j| own[j].clone().map(|o| (j, o))) {
            None => ModelDatabase::Primary,
            Some((j, Own::Named { writing, reading, span })) => {
                let declared_by = app.models[j].name.clone();
                let class = declared_by.0.as_str();
                if !have_manifest && app.database_yml.is_some() {
                    // Boot resolves `config/database.yml` and checks each
                    // named role (`DatabaseConfiguration::OWNERSHIP`); the
                    // runtime opens only the primary, so a writing role
                    // other than Rails' `primary` cannot be it.
                    if writing != "primary" {
                        note(
                            span,
                            "connects_to",
                            format!(
                                "models under `{class}` write to database `{writing}`: the emitted runtime opens only the primary configuration (resolved from `config/database.yml` at boot), and a second database is not opened yet"
                            ),
                            name,
                        );
                    }
                } else if !have_manifest {
                    note(
                        span,
                        "connects_to",
                        format!(
                            "`connects_to` on `{class}` names databases, and `roundhouse.yml` declares no `database_configurations` manifest (or `database_adapter` for boot-time `config/database.yml` resolution) to resolve them"
                        ),
                        name,
                    );
                } else {
                    for role in std::iter::once(&writing).chain(reading.iter()) {
                        match app.databases.iter().find(|d| d.name == *role) {
                            None => note(
                                span,
                                "connects_to",
                                format!("`connects_to` on `{class}` names database `{role}`, which the manifest does not configure"),
                                name,
                            ),
                            Some(db) if role == &writing && !db.primary => {
                                let why = db.adapter.runtime_refusal().unwrap_or_else(|| {
                                    format!(
                                        "the emitted runtime opens only the primary database; a second {} database is not opened yet",
                                        db.adapter.name()
                                    )
                                });
                                note(
                                    span,
                                    "connects_to",
                                    format!("models under `{class}` are owned by database `{role}` ({}): {why}", db.adapter.name()),
                                    name,
                                );
                            }
                            Some(_) => {}
                        }
                    }
                }
                ModelDatabase::Named { writing, reading, declared_by, span }
            }
            Some((j, Own::AppDefined { method, defined_by, span })) => {
                let class = app.models[j].name.0.as_str();
                note(
                    span,
                    "connection_override",
                    format!(
                        "`{}.{}` is app-defined (in `{}`): app-defined connection routing is not modeled, so the database its queries reach cannot be resolved",
                        class,
                        method.as_str(),
                        defined_by.0.as_str()
                    ),
                    name,
                );
                ModelDatabase::AppDefined { method, defined_by, span }
            }
            Some((_, Own::Unresolved { construct, reason, span })) => {
                note(span, construct, reason.clone(), name);
                ModelDatabase::Unresolved { reason, span }
            }
        };
        owners.insert(model.name.clone(), ownership);
    }
    for (span, construct, detail, models) in by_site {
        let shown: Vec<&str> = models.iter().take(4).map(String::as_str).collect();
        let more = if models.len() > 4 { format!(", and {} more", models.len() - 4) } else { String::new() };
        refusals.push(DatabaseRefusal {
            span,
            construct: construct.to_string(),
            detail: format!("{detail} ({} model(s): {}{more})", models.len(), shown.join(", ")),
            warning: false,
        });
    }

    // Role and shard switches, wherever the app makes one.
    let mut switches = Vec::new();
    let mut walk = |e: &Expr| calls_named(e, SWITCHES, &mut switches);
    for model in &app.models {
        for item in &model.body {
            match item {
                ModelBodyItem::Method { method, .. } => walk(&method.body),
                ModelBodyItem::Unknown { expr, .. } => walk(expr),
                ModelBodyItem::Scope { scope, .. } => walk(&scope.body),
                _ => {}
            }
        }
    }
    for controller in &app.controllers {
        for item in &controller.body {
            match item {
                crate::dialect::ControllerBodyItem::Action { action, .. } => walk(&action.body),
                crate::dialect::ControllerBodyItem::ClassMethod { method, .. } => walk(&method.body),
                _ => {}
            }
        }
    }
    for class in app.library_classes.iter().chain(app.rails_application.iter()) {
        class.methods.iter().for_each(|m| walk(&m.body));
    }
    for module in &app.test_modules {
        module.tests.iter().for_each(|t| walk(&t.body));
        module.setup.iter().for_each(&mut walk);
    }
    switches.sort_by_key(|(s, _)| (s.file.0, s.start));
    switches.dedup_by_key(|(s, _)| *s);
    for (span, method) in switches {
        refusals.push(DatabaseRefusal {
            span,
            construct: method.clone(),
            detail: format!("`{method}` switches the role or shard a block runs on; role and shard switching is not modeled"),
            warning: false,
        });
    }

    let any_routing = owners.values().any(|o| !matches!(o, ModelDatabase::Primary));
    if have_manifest || any_routing {
        app.model_databases = owners;
    }
    app.database_refusals.extend(refusals);
}
