//! Admission of Concern-declared virtual model accessors. Collection
//! retains candidate declarations; only a concrete model inclusion
//! commits to their ivar contract or reports a contextual refusal.

use crate::det::{HashMap, HashSet};

use crate::diagnostic::DiagnosticKind;
use crate::dialect::{MethodReceiver, ModelBodyItem};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::App;

use super::survey::unwrap_or_record;
use super::{IngestError, IngestResult};

/// Retain recognized attr_* declarations, including conditional ones,
/// so refusal is attributed to each includer rather than silently dropped.
pub(super) fn is_candidate(item: &ModelBodyItem) -> bool {
    fn accessor(expr: &Expr) -> bool {
        match &*expr.node {
            ExprNode::Send { recv: None, method, .. } =>
                matches!(method.as_str(), "attr_accessor" | "attr_reader" | "attr_writer"),
            ExprNode::If { cond, then_branch, else_branch } =>
                accessor(cond) || accessor(then_branch) || accessor(else_branch),
            ExprNode::Seq { exprs } => exprs.iter().any(accessor),
            _ => false,
        }
    }
    matches!(item, ModelBodyItem::Unknown { expr, .. } if accessor(expr))
}

/// Computed/splat names retain concern lexical scope, and one-sided
/// accessors need a separate analyzer fix. Neither is admitted here.
pub(super) fn is_supported(item: &ModelBodyItem) -> bool {
    let ModelBodyItem::Unknown { expr, .. } = item else {
        return false;
    };
    let ExprNode::Send {
        recv: None,
        method,
        args,
        block: None,
        ..
    } = &*expr.node
    else {
        return false;
    };
    method.as_str() == "attr_accessor"
        && !args.is_empty()
        && args.iter().all(|arg| {
            matches!(
                &*arg.node,
                ExprNode::Lit {
                    value: Literal::Sym { .. }
                }
            )
        })
}

/// This annotation belongs to the candidate clone, not the module's
/// original body. Dormant blocks must not become global ingest errors.
pub(super) fn decline(item: &mut ModelBodyItem, detail: &str) {
    let ModelBodyItem::Unknown { expr, .. } = item else {
        unreachable!()
    };
    expr.diagnostic.get_or_insert(DiagnosticKind::Unsupported {
        target: None,
        construct: Symbol::from("concern attr_accessor"),
        detail: detail.into(),
    });
}

/// Direct literal visibility calls are handled in declaration order.
/// Conditional/wrapped calls and computed names cannot be replayed;
/// refuse only accessors whose visibility they could affect.
fn uncertain_visibility(expr: &Expr, name: &Symbol, writer: &Symbol, nested: bool) -> bool {
    if let ExprNode::Send {
        recv, method, args, ..
    } = &*expr.node
    {
        if recv
            .as_ref()
            .is_none_or(|recv| matches!(&*recv.node, ExprNode::SelfRef))
            && matches!(method.as_str(), "private" | "protected" | "public")
        {
            if nested && args.is_empty() {
                return true;
            }
            for arg in args {
                let target = match &*arg.node {
                    ExprNode::Lit {
                        value: Literal::Sym { value },
                    } => value.clone(),
                    ExprNode::Lit {
                        value: Literal::Str { value },
                    } => Symbol::from(value.as_str()),
                    _ => return true,
                };
                if nested && (&target == name || &target == writer) {
                    return true;
                }
            }
        }
    }
    let mut uncertain = false;
    expr.node.for_each_child(&mut |child| {
        uncertain |= uncertain_visibility(child, name, writer, true);
    });
    uncertain
}

/// Retained singleton hooks run during the emitted include. Consumed
/// class-method carriers have no hook left here. Only literal included
/// callbacks/defaults after block registration are inert; overriding the
/// rest of the framework protocol can prevent that registration/execution.
fn unconsumed_included_hook(model: &crate::dialect::Model, app: &App) -> Option<crate::ClassId> {
    unconsumed_hook_from(crate::analyze::model_includes(model), app, false)
}

/// [`unconsumed_included_hook`] from an includer's direct includes.
///
/// For a job (`job`), a plain module's `self.included(base)` hook that
/// only registers ActiveJob::Callbacks callbacks on `base`
/// (`job_class_side::callback_registering_hook`: Core's
/// BackgroundQueue::WithFallback, `base.around_enqueue do ... end`) is
/// consumed: the emitted include runs it, as Ruby does, on a class the
/// port gives the callbacks to, and what it does commutes with every
/// statement carried here. It writes the job's enqueue or perform chain
/// and nothing else, and its blocks run when a job is enqueued or
/// performed, not when they are registered; a carried `class_attribute`
/// or `attr_accessor` defines its own reader, writer and value and reads
/// no chain. The two can run in either order with the same result. Where
/// the hook's callback lands in its chain is the include's position,
/// which the emitted body keeps, with every carried callback
/// registration written after its own include
/// (`App::included_block_statements`).
fn unconsumed_hook_from(includes: Vec<ClassId>, app: &App, job: bool) -> Option<crate::ClassId> {
    fn inert(expr: &Expr) -> bool {
        match &*expr.node {
            ExprNode::Lit { .. } => true,
            ExprNode::Seq { exprs } => exprs.iter().all(inert),
            ExprNode::Return { value } => inert(value),
            _ => false,
        }
    }

    let mut pending = includes;
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for class in app.library_classes.iter().filter(|class| class.name == id) {
            for method in &class.methods {
                let intercepts_registration = app.concern_model_items.get(&id)
                    .into_iter().flatten().any(|item| {
                        matches!(item, ModelBodyItem::Unknown { expr, .. } if is_candidate(item)
                            && (method.name_span.file != expr.span.file
                                || method.name_span.start < expr.span.start))
                    });
                let consumed = (!intercepts_registration && extend_only_hook(class, method, app))
                    || job
                    && class.is_module
                    // A Concern's own `included` would stand in for the one
                    // its `included do` calls.
                    && app.concern_model_items.get(&id).is_none_or(|items| items.is_empty())
                    && method.name.as_str() == "included"
                    && method.receiver == crate::dialect::MethodReceiver::Class
                    && !intercepts_registration
                    && crate::lower::job_class_side::callback_registering_hook(class)
                        .is_some_and(|_| method.params.len() == 1);
                if !consumed
                    && super::class_configuration::overrides_framework_api(method)
                    && (method.name.as_str() != "included"
                        || intercepts_registration
                        || !inert(&method.body)
                        || method
                            .params
                            .iter()
                            .chain(method.block_param.iter())
                            .any(|param| param.default.as_ref().is_some_and(|expr| !inert(expr))))
                {
                    return Some(id);
                }
            }
            pending.extend(class.includes.iter().cloned());
        }
    }
    None
}

/// A plain module's `def self.included(base) base.extend(ClassMethods)
/// end` (query_layer-active_record's QueryLayer::ActiveRecord): the
/// emitted include runs it, as Ruby does, at the include's position, and
/// all it does is put an app module's methods on the includer's
/// singleton chain. A carried `class_attribute` or `attr_accessor`
/// defines the includer's own methods, which win over an extended
/// module's in either order, and a carried call dispatches through the
/// chain the emitted includes build. Only an extended module defining
/// none of the macros a carry lowers is admitted, and a Concern (whose
/// own `included` would replace the one its block registers) is not.
fn extend_only_hook(class: &crate::dialect::LibraryClass, method: &crate::dialect::MethodDef, app: &App) -> bool {
    if !class.is_module
        || method.name.as_str() != "included"
        || method.receiver != MethodReceiver::Class
        || method.params.len() != 1
        || method.params[0].keyword
        || method.params[0].default.is_some()
        || method.block_param.is_some()
        || app.concern_model_items.get(&class.name).is_some_and(|items| !items.is_empty())
    {
        return false;
    }
    let base = &method.params[0].name;
    let statements: Vec<&Expr> = match &*method.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&method.body],
    };
    let extends_app_module = |expr: &Expr| {
        let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*expr.node else {
            return false;
        };
        if method.as_str() != "extend"
            || !matches!(&*recv.node, ExprNode::Var { name, .. } if name == base)
            || args.len() != 1
        {
            return false;
        }
        let ExprNode::Const { path } = &*args[0].node else { return false };
        let path = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
        let nested = format!("{}::{path}", class.name.0.as_str());
        let module = |name: &str| app.library_classes.iter().find(|lc| lc.is_module && lc.name.0.as_str() == name);
        let Some(module) = module(&nested).or_else(|| module(&path)) else {
            return false;
        };
        !module.methods.iter().any(|m| {
            m.receiver == MethodReceiver::Instance
                && matches!(
                    m.name.as_str(),
                    "class_attribute" | "attr_accessor" | "attr_reader" | "attr_writer"
                        | "included" | "extended" | "append_features"
                )
        })
    };
    !statements.is_empty() && statements.into_iter().all(extends_app_module)
}

/// The model splice supplies only direct inclusions. Refuse other activated
/// candidates instead of emitting a clean program without their accessors.
/// Concern-to-Concern dependencies defer execution; plain modules do not.
fn validate_activation(
    app: &mut App,
    spans: &HashSet<crate::span::Span>,
    carriers: &[super::library_class::ConcernClassMethodSpans],
    framework_shadows: &HashSet<crate::ClassId>,
    settled: &HashSet<(ClassId, Span)>,
) -> IngestResult<()> {
    use std::collections::BTreeMap;
    use super::util::{class_name_path, constant_id_str, constant_path_of,
        constant_path_is_rooted,
        find_all_module_declarations_with_scope, find_all_classes_with_scope, flatten_statements,
        module_name_path};
    use crate::span::{FileId, Span};

    if spans.is_empty() {
        return Ok(());
    }
    let resolver = app.const_resolver.for_app(app);
    let mut includes = BTreeMap::<crate::ClassId, Vec<crate::ClassId>>::new();
    let mut block_includes = BTreeMap::<crate::ClassId, Vec<crate::ClassId>>::new();
    let mut modules = HashSet::default();
    let retained: HashSet<_> = app.library_classes.iter().map(|c| &c.name)
        .chain(app.models.iter().map(|m| &m.name))
        .chain(app.controllers.iter().map(|c| &c.name))
        .chain(app.test_modules.iter().map(|t| &t.name))
        // Test-body classes keep their Ruby names (`SupportTest::Support`),
        // which is how the source walk below addresses them.
        .chain(app.test_modules.iter().flat_map(|t| t.inner_classes.iter().map(|c| &c.name)))
        .cloned()
        .collect();
    for (index, source) in app.sources.iter().enumerate().filter(|(_, s)| s.path.ends_with(".rb")) {
        let parsed = ruby_prism::parse(source.text.as_bytes());
        let root = parsed.node();
        let bodies = find_all_classes_with_scope(&root).into_iter()
            .filter_map(|(scope, class)| Some((scope, class_name_path(&class)?, class.body()?, false)))
            .chain(find_all_module_declarations_with_scope(&root).into_iter()
                .filter_map(|(scope, module)| Some((scope, module_name_path(&module)?, module.body()?, true))));
        // Even include-only wrappers omitted from emission carry dependency
        // edges. Keep the existing LibraryClass classification unchanged.
        for (mut scope, name, body, is_module) in bodies {
            scope.extend(name);
            let owner = crate::ClassId(Symbol::from(scope.join("::")));
            if is_module {
                modules.insert(owner.clone());
            }
            includes.entry(owner.clone()).or_default();
            // A Concern's `included do include M end` runs on the base, as
            // a dependency does (SofterDelete::UsingIsDeleted). Kept apart:
            // dependency verification reads only module-level edges.
            let mut statements: Vec<_> = flatten_statements(body).into_iter().map(|s| (s, false)).collect();
            let blocks: Vec<_> = statements.iter().filter_map(|(statement, _)| statement.as_call_node()).collect();
            for call in blocks {
                if is_module
                    && call.receiver().is_none()
                    && constant_id_str(&call.name()) == "included"
                    && call.arguments().is_none()
                    && let Some(block) = call.block().and_then(|b| b.as_block_node())
                    && let Some(block_body) = block.body()
                {
                    statements.extend(flatten_statements(block_body).into_iter().map(|s| (s, true)));
                }
            }
            for (statement, in_block) in statements {
                let Some(call) = statement.as_call_node() else { continue };
                if call.receiver().is_some() || call.block().is_some() {
                    continue;
                }
                let name = call.name();
                let name = constant_id_str(&name);
                if name != "include" {
                    continue;
                }
                let edges = if in_block { &mut block_includes } else { &mut includes }
                    .entry(owner.clone()).or_default();
                let Some(args) = call.arguments() else { continue };
                for arg in args.arguments().iter() {
                    let Some(path) = constant_path_of(&arg) else { continue };
                    let span = Span {
                        file: FileId((index + 1) as u32),
                        start: arg.location().start_offset() as u32,
                        end: arg.location().end_offset() as u32,
                    };
                    let path: Vec<_> = path.into_iter().map(Symbol::from).collect();
                    if arg.as_constant_path_node().is_some_and(|p| constant_path_is_rooted(&p)) {
                        // An explicit root names the exact namespace. Rubydex
                        // does not retain a name-only reference for `::X`.
                        edges.push(crate::ClassId(Symbol::from(path.iter()
                            .map(Symbol::as_str).collect::<Vec<_>>().join("::"))));
                    } else if let Some(id) = resolver.namespace(span, &path) {
                        edges.push(id.clone());
                    }
                }
            }
        }
    }
    // Spelling alone does not prove deferral: use the same source identity,
    // binding barriers and extension ordering as finite configuration.
    let deferred = super::class_configuration::verified_framework_concerns(
        carriers,
        &includes.iter().map(|(id, edges)| (id.clone(), edges.clone())).collect(),
        framework_shadows,
    );
    // An include lifted into a model body from elsewhere (an initializer's
    // `on_load(:user) { class User; include Extension; end }`,
    // `load_hook_includes`) is the model's own include: resolve it where it
    // is written, falling back to the model's lexical scope, which is the
    // reopen's.
    for model in &app.models {
        let mut lifted = Vec::new();
        for item in &model.body {
            let ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else { continue };
            if method.as_str() != "include" {
                continue;
            }
            for arg in args {
                let ExprNode::Const { path } = &*arg.node else { continue };
                if let Some(id) = resolver.namespace(arg.span, path) {
                    lifted.push(id.clone());
                    continue;
                }
                let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
                let mut scope = Some(model.name.0.as_str());
                while let Some(prefix) = scope {
                    let id = crate::ClassId(Symbol::from(format!("{prefix}::{written}")));
                    if modules.contains(&id) {
                        lifted.push(id);
                        break;
                    }
                    scope = prefix.rsplit_once("::").map(|(prefix, _)| prefix);
                }
                if scope.is_none() {
                    let id = crate::ClassId(Symbol::from(written.trim_start_matches("::")));
                    if modules.contains(&id) {
                        lifted.push(id);
                    }
                }
            }
        }
        let edges = includes.entry(model.name.clone()).or_default();
        for id in lifted {
            if !edges.contains(&id) {
                edges.push(id);
            }
        }
    }
    // What a class's superclass chain already includes, transitively. A
    // library class including such a Concern again is a no-op in Ruby (the
    // module is in its ancestors): the accessor is the parent's, and
    // `splice_into_library_includers` rightly supplies nothing here.
    let parents: HashMap<crate::ClassId, crate::ClassId> = app
        .library_classes
        .iter()
        .filter_map(|lc| Some((lc.name.clone(), lc.parent.clone()?)))
        .chain(app.models.iter().filter_map(|m| Some((m.name.clone(), m.parent.clone()?))))
        .collect();
    let inherited = |owner: &crate::ClassId| -> HashSet<crate::ClassId> {
        let mut out = HashSet::default();
        let mut visited = HashSet::default();
        let mut at = parents.get(owner).cloned();
        while let Some(parent) = at.take() {
            if !visited.insert(parent.clone()) {
                break;
            }
            let mut stack: Vec<crate::ClassId> = includes.get(&parent).cloned().unwrap_or_default();
            while let Some(id) = stack.pop() {
                if out.insert(id.clone()) {
                    stack.extend(includes.get(&id).into_iter().flatten().cloned());
                }
            }
            at = parents.get(&parent).cloned();
        }
        out
    };
    // What a Concern hands its includer: each verified Concern it includes
    // at module level becomes a dependency, included into the base first
    // (`append_features` walks `@_dependencies`), and an `include` its
    // block performs runs on the base too. Only verified Concerns are
    // followed: a plain module's own includes ran on that module.
    let concern_edges = |id: &crate::ClassId| {
        includes.get(id).into_iter().chain(block_includes.get(id)).flatten()
    };
    let deferred_reach = |direct: &[crate::ClassId]| -> HashSet<crate::ClassId> {
        let mut out = HashSet::default();
        let mut stack: Vec<_> = direct.iter().filter(|id| deferred.contains(*id)).cloned().collect();
        while let Some(id) = stack.pop() {
            if out.insert(id.clone()) {
                stack.extend(concern_edges(&id).filter(|nested| deferred.contains(*nested)).cloned());
            }
        }
        out
    };
    for (owner, direct) in &includes {
        // A survey source refusal already diagnosed an omitted class.
        if deferred.contains(owner) || (!retained.contains(owner) && !modules.contains(owner)) {
            continue;
        }
        let mut model = app.models.iter_mut().find(|m| &m.name == owner);
        let reach = if model.is_some() { deferred_reach(direct) } else { HashSet::default() };
        if let Some(model) = model.as_mut() {
            for item in &mut model.body {
                let ModelBodyItem::Unknown { expr, .. } = item else { continue };
                if spans.contains(&expr.span) && !reach.iter().any(|id| {
                    app.concern_model_items.get(id).is_some_and(|items| items.iter().any(|item| {
                        matches!(item, ModelBodyItem::Unknown { expr: original, .. } if original.span == expr.span)
                    }))
                }) {
                    // Neither a namesake nor a user-defined framework spelling
                    // establishes the block's accessor contract.
                    decline(item, "is not supplied by a source-resolved, verified Concern include");
                }
            }
        }
        let mut pending = direct.clone();
        let mut seen = HashSet::default();
        let mut reported = HashSet::default();
        let ancestors_have = if model.is_none() { inherited(owner) } else { HashSet::default() };
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) || ancestors_have.contains(&id) {
                continue;
            }
            if let Some(items) = app.concern_model_items.get(&id) {
                for item in items.iter().filter(|item| is_candidate(item)) {
                    let ModelBodyItem::Unknown { expr, .. } = item else { unreachable!() };
                    // `splice_into_library_includers` supplied or refused it.
                    if model.is_none() && settled.contains(&(owner.clone(), expr.span)) {
                        continue;
                    }
                    let supplied = (direct.contains(&id) || reach.contains(&id)) && model.as_ref().is_some_and(|m| m.body.iter().any(|item| {
                        matches!(item, ModelBodyItem::Unknown { expr: carried, .. } if carried.span == expr.span)
                    }));
                    if supplied {
                        continue;
                    }
                    // A gathering Concern's splice also carries its
                    // dependencies' `included` items into the model. This
                    // refusal is that copy's one report: drop the copy, so
                    // the per-model refusals neither report it a second
                    // time nor lower the accessor.
                    if let Some(model) = model.as_mut() {
                        model.body.retain(|item| !matches!(item,
                            ModelBodyItem::Unknown { expr: carried, .. } if carried.span == expr.span));
                    }
                    if reported.insert(expr.span) {
                        unwrap_or_record::<()>(Err(IngestError::Unsupported {
                            file: app.sources[(expr.span.file.0 - 1) as usize].path.clone(),
                            message: format!(
                                "concern attr_accessor on {}: only direct model inclusion and plain library-class inclusion are supported",
                                owner.0
                            ),
                        }))?;
                    }
                }
            }
            // A plain module's own includes ran on that module (itself an
            // owner here); a Concern's are deferred to this includer.
            if let Some(nested) = includes.get(&id)
                && (model.is_some() || deferred.contains(&id))
            {
                pending.extend(nested.iter().cloned());
            }
            if deferred.contains(&id) {
                pending.extend(block_includes.get(&id).into_iter().flatten().cloned());
            }
        }
    }
    Ok(())
}

/// Ask the canonical synthesizers about occupied methods/storage,
/// rather than maintaining another DSL collision list.
pub(super) fn validate(
    app: &mut App,
    carriers: &[super::library_class::ConcernClassMethodSpans],
    framework_shadows: &HashSet<crate::ClassId>,
    settled: &HashSet<(ClassId, Span)>,
) -> IngestResult<()> {
    // Include splicing retains the original declaration's file/range,
    // just as ConcernClassMethodSpans identifies consumed carriers.
    // This join is confined to ingest, before source-shaped IR returns;
    // it is not an identity protocol across analysis or normalization.
    // The multi-includer regression pins original and cloned spans.
    let spans: HashSet<_> = app
        .concern_model_items
        .values()
        .flatten()
        .filter_map(|item| {
            if !is_candidate(item) {
                return None;
            };
            let ModelBodyItem::Unknown { expr, .. } = item else {
                unreachable!()
            };
            Some(expr.span)
        })
        .collect();
    validate_activation(app, &spans, carriers, framework_shadows, settled)?;
    let candidates: HashSet<_> = app
        .models
        .iter()
        .filter(|model| {
            model.body.iter().any(|item| {
                matches!(item,
            ModelBodyItem::Unknown { expr, .. } if spans.contains(&expr.span))
            })
        })
        .map(|model| model.name.clone())
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    // The surface probe analyzes and lowers a clone of the whole app, on
    // a large app a second complete analysis. Only a name that passes every
    // syntactic check reads it, so a dry pass finds those models and the
    // probe covers just them (or does not run at all).
    let mut probed = HashSet::default();
    let _ = refusals(app, &spans, &mut |model, _, _| {
        probed.insert(model.clone());
        false
    });
    let surfaces = if probed.is_empty() {
        HashMap::default()
    } else {
        crate::timings::phase("concern-accessor-surface", || {
            crate::lower::model_to_library::accessor_surface::occupied_surfaces(app, &probed)
        })
    };
    let refused = refusals(app, &spans, &mut |model, name, writer| {
        surfaces[model]
            .as_ref()
            .is_none_or(|names| names.contains(name) || names.contains(writer))
    });
    let mut rejections = Vec::new();
    for (model_index, refused) in refused {
        let mut rejected = HashSet::default();
        for (span, message) in refused {
            let file = &app.sources[span.file.0 as usize - 1].path;
            unwrap_or_record::<()>(Err(IngestError::Unsupported {
                file: file.clone(),
                message,
            }))?;
            rejected.insert(span);
        }
        rejections.push((model_index, rejected));
    }
    // Delay survey removals until every probe has seen original demand.
    for (index, rejected) in rejections {
        app.models[index].body.retain(|item| !matches!(item, ModelBodyItem::Unknown { expr, .. } if rejected.contains(&expr.span)));
    }
    Ok(())
}

/// Each carried model's refused declarations with their messages, in
/// source order. `occupied(model, reader, writer)` is asked last, only
/// for a name every syntactic check admits.
fn refusals(
    app: &App,
    spans: &HashSet<Span>,
    occupied: &mut dyn FnMut(&ClassId, &Symbol, &Symbol) -> bool,
) -> Vec<(usize, Vec<(Span, String)>)> {
    let mut out = Vec::new();
    for (model_index, model) in app.models.iter().enumerate() {
        let carried = |item: &ModelBodyItem| matches!(item, ModelBodyItem::Unknown { expr, .. } if spans.contains(&expr.span));
        if !model.body.iter().any(carried) {
            continue;
        };
        let hook = unconsumed_included_hook(model, app);
        let mut nonpublic_methods = HashSet::default();
        for item in &model.body {
            match item {
                ModelBodyItem::Unknown { expr, .. } => {
                    if let ExprNode::Send {
                        recv, method, args, ..
                    } = &*expr.node
                    {
                        if carried(item) {
                            // The included block's lexical public scope,
                            // not the model's current bare visibility.
                            for arg in args {
                                if let ExprNode::Lit {
                                    value: Literal::Sym { value },
                                } = &*arg.node
                                {
                                    nonpublic_methods.remove(value);
                                    nonpublic_methods.remove(&Symbol::from(format!("{value}=")));
                                }
                            }
                        } else if recv
                            .as_ref()
                            .is_none_or(|recv| matches!(&*recv.node, ExprNode::SelfRef))
                            && matches!(method.as_str(), "private" | "protected" | "public")
                        {
                            for arg in args {
                                let name = match &*arg.node {
                                    ExprNode::Lit {
                                        value: Literal::Sym { value },
                                    } => value.clone(),
                                    ExprNode::Lit {
                                        value: Literal::Str { value },
                                    } => Symbol::from(value.as_str()),
                                    _ => continue,
                                };
                                if method.as_str() == "public" {
                                    nonpublic_methods.remove(&name);
                                } else {
                                    nonpublic_methods.insert(name);
                                }
                            }
                        }
                    }
                }
                ModelBodyItem::Method { method, .. }
                    if method.receiver == MethodReceiver::Instance =>
                {
                    if method.visibility != crate::dialect::MethodVisibility::Public {
                        nonpublic_methods.insert(method.name.clone());
                    } else {
                        nonpublic_methods.remove(&method.name);
                    }
                }
                _ => {}
            }
        }
        let mut refused = Vec::new();
        for (index, item) in model
            .body
            .iter()
            .enumerate()
            .filter(|(_, item)| carried(item))
        {
            let ModelBodyItem::Unknown { expr, .. } = item else {
                unreachable!()
            };
            if let Some(DiagnosticKind::Unsupported {
                construct, detail, ..
            }) = &expr.diagnostic
            {
                refused.push((expr.span, format!("{construct} on {} {detail}", model.name.0)));
                continue;
            }
            if let Some(hook) = &hook {
                refused.push((
                    expr.span,
                    format!(
                        "concern attr_accessor on {} cannot be carried alongside an unconsumed included hook or overridden framework API on {hook}",
                        model.name.0
                    ),
                ));
                continue;
            }
            let ExprNode::Send { args, .. } = &*expr.node else {
                unreachable!()
            };
            for arg in args {
                let ExprNode::Lit {
                    value: Literal::Sym { value: name },
                } = &*arg.node
                else {
                    unreachable!()
                };
                let writer = Symbol::from(format!("{name}="));
                let earlier = model.body[..index].iter().any(|item| {
                    matches!(item, ModelBodyItem::Method { method, .. } if method.receiver == MethodReceiver::Instance
                        && (method.name == *name || method.name == writer))
                });
                let uncertain = model.body.iter().any(|item| {
                    matches!(item, ModelBodyItem::Unknown { expr, .. }
                        if uncertain_visibility(expr, name, &writer, false))
                });
                if nonpublic_methods.contains(name)
                    || nonpublic_methods.contains(&writer)
                    || earlier
                    || uncertain
                    || occupied(&model.name, name, &writer)
                {
                    refused.push((
                        expr.span,
                        format!(
                            "concern attr_accessor :{name} on {} requires a fresh virtual name on a concrete model with a single definition, without visibility modifiers or earlier method overrides",
                            model.name.0
                        ),
                    ));
                }
            }
        }
        out.push((model_index, refused));
    }
    out
}

/// The model-only shape refusal [`is_supported`] records. Context
/// refusals (with_options, unmodeled block statements) are recorded
/// first, so a candidate carrying only this one is a plain declaration.
pub(super) const SHAPE_DETAIL: &str =
    "unsupported accessor shape: only direct, nonempty, literal-Symbol attr_accessor is modeled";

/// A Concern's `included` block runs in the class body of each plain
/// (non-model) includer: `ActiveSupport::Concern#append_features` is
/// `base.class_eval(&@_included_block)`. What this supplies there:
///
/// - literal `attr_reader` / `attr_writer` / `attr_accessor`: public ivar
///   accessors on the includer;
/// - `class_attribute` (`ingest::class_attribute`): the class-side and
///   instance methods it defines, owned by the includer, and the seed
///   `@__class_attr_x = default`, run on the includer when the block runs
///   (a subclass reads it through its parent until it writes its own,
///   `claim_class_attribute_writes`);
/// - `self.x = v` through such an attribute's writer, run there too;
/// - `include M`: M included into the includer after the Concern (a
///   Concern M's block then runs there as well).
///
/// On an STI subclass (a library class below a model), any other
/// statement of the block (`typed_store`, `serialize`, a validation, a
/// callback) joins the subclass's own body calls, ahead of them, as the
/// block runs where the `include` is: the STI subclass lowering then
/// defines or refuses it as if the subclass body wrote it
/// ([`sti_subclass_statement`]). On any other includer such a statement
/// (a validation, a callback, a gem's class-body DSL) is reported, once
/// per statement: the emitted includer would run without it. So is the whole block of a Concern this cannot
/// verify (`class_configuration::framework_concern_verdicts`), with why.
///
/// The base is whoever performed the include: a Concern defers its own
/// includes to its includer (`@_dependencies`), so they are followed; a
/// plain module performed its includes on itself, so the block runs on
/// that module (an owner here in its own right) and its includers reach
/// what it defined through Ruby's include. A Concern runs once per base,
/// and not at all on a subclass whose superclass already has it
/// (`append_features` returns early when `base < self`).
///
/// Returns every (includer, accessor declaration) this settled, supplied
/// or refused, so the activation check does not report it again;
/// whatever it did not reach is still refused there.
pub(super) fn splice_into_library_includers(
    app: &mut App,
    sources: &[crate::span::SourceFile],
    carriers: &[super::library_class::ConcernClassMethodSpans],
    framework_shadows: &HashSet<ClassId>,
) -> IngestResult<HashSet<(ClassId, Span)>> {
    let mut settled = HashSet::default();
    if app.concern_model_items.is_empty() {
        return Ok(settled);
    }
    // A kept root (`include ::A::B`, where `Owner::A` also exists)
    // names the same top-level module.
    let top = |id: &ClassId| ClassId(Symbol::from(id.0.as_str().trim_start_matches("::")));
    let mut module_includes: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for lc in &app.library_classes {
        module_includes.entry(lc.name.clone()).or_default().extend(lc.includes.iter().map(top));
    }
    let verdicts =
        super::class_configuration::framework_concern_verdicts(carriers, &module_includes, framework_shadows);
    let verified: HashSet<ClassId> =
        verdicts.iter().filter(|(_, verdict)| verdict.is_ok()).map(|(id, _)| id.clone()).collect();
    // A Concern with a block this cannot verify: its block is not run.
    let unverified: HashMap<ClassId, &str> = verdicts
        .iter()
        .filter(|(id, _)| app.concern_model_items.get(*id).is_some_and(|items| !items.is_empty()))
        .filter_map(|(id, verdict)| verdict.err().map(|reason| (id.clone(), reason)))
        .collect();
    let models: HashSet<&ClassId> = app.models.iter().map(|m| &m.name).collect();
    // Every constant the app defines, fully qualified: a carried
    // statement keeps the Concern's lexical scope (`super::app::rehome_carried`).
    let mut defined: HashSet<String> = HashSet::default();
    for lc in &app.library_classes {
        defined.insert(lc.name.0.as_str().to_string());
        for (name, _) in &lc.constants {
            defined.insert(format!("{}::{}", lc.name.0.as_str(), name.as_str()));
        }
    }
    defined.extend(app.models.iter().map(|m| m.name.0.as_str().to_string()));
    // The names an app class answers on its class side: its own
    // `def self.x`, and a module's methods, which an `extend` (or a
    // Concern's `ClassMethods`) puts there. A receiverless call in an
    // `included do` naming none of them is a framework DSL a plain
    // includer may not have on this runtime, and stays refused.
    let class_side: HashSet<&str> = app
        .library_classes
        .iter()
        .flat_map(|lc| {
            lc.methods
                .iter()
                .filter(move |m| lc.is_module || m.receiver == MethodReceiver::Class)
                .map(|m| m.name.as_str())
        })
        .collect();
    // Or a gem class above it answers it (`QueryLayer::Response.field`,
    // by the gem's RBI), or a gem module it includes mixes it in.
    let gem_class_side = |lc: &crate::dialect::LibraryClass, method: &str| -> bool {
        let boundary = &app.gem_boundary;
        let name = Symbol::from(method);
        if lc.includes.iter().any(|inc| boundary.loaded_mixed_in_class_methods(&top(inc)).contains(&name)) {
            return true;
        }
        let mut scope = lc.name.0.as_str().to_string();
        let mut at = lc.parent.clone();
        for _ in 0..32 {
            let Some(parent) = at.take() else { return false };
            let resolved = boundary.resolve_written(&scope, &parent).unwrap_or_else(|| top(&parent));
            if let Some(class) = boundary.loaded_reopen(&resolved) {
                let owner = resolved.0.as_str();
                if class.class_methods.contains_key(&name)
                    || class.extends.iter().filter_map(|e| boundary.resolve_written(owner, e))
                        .any(|e| boundary.loaded_instance_surface(&e).contains(&name))
                {
                    return true;
                }
                scope = owner.to_string();
                at = class.parent.clone();
            } else {
                let Some(app_class) = app.library_classes.iter().find(|c| c.name == resolved) else { return false };
                scope = app_class.name.0.as_str().to_string();
                at = app_class.parent.clone();
            }
        }
        false
    };
    // A test case's class side: ActiveSupport::Testing::SetupAndTeardown's
    // `setup`/`teardown` (the runner's RoundhouseTestCallbacks port) on
    // ActiveSupport::TestCase and what inherits it, and Module#prepend
    // there. Core's ActiveRecordTestHelper, which test/support's
    // active_support_test_case.rb includes into ActiveSupport::TestCase,
    // registers `setup(:default_pod_selection)` in its `included do`:
    // left out, every job a test enqueued had no pod (`Unspecified
    // pod_id`).
    let test_case_side = |lc: &crate::dialect::LibraryClass, method: &str| -> bool {
        if !matches!(method, "setup" | "teardown" | "prepend") {
            return false;
        }
        let mut at = Some(lc);
        for _ in 0..32 {
            let Some(class) = at else { return false };
            if top(&class.name).0.as_str() == "ActiveSupport::TestCase" {
                return true;
            }
            let Some(parent) = class.parent.as_ref() else { return false };
            if top(parent).0.as_str() == "ActiveSupport::TestCase" {
                return true;
            }
            at = app.library_classes.iter().find(|c| c.name == top(parent));
        }
        false
    };
    let block_includes = |concern: &ClassId| -> Vec<ClassId> {
        app.concern_model_items
            .get(concern)
            .into_iter()
            .flatten()
            .flat_map(|item| included_modules(item, concern, &defined))
            .collect()
    };

    // The Concerns whose blocks run on each base, in the order Rails runs
    // them: a Concern's dependencies first, then its own block, then what
    // that block includes. Each with the base's direct include it hangs
    // off, which is where a block's `include` lands. Second, the
    // unverified Concerns with a block that it reached the same way.
    type Reached = (Vec<(usize, ClassId)>, Vec<ClassId>);
    let reach = |lc: &crate::dialect::LibraryClass, inherited: &HashSet<ClassId>| -> Reached {
        struct Walk<'a> {
            verified: &'a HashSet<ClassId>,
            unverified: &'a HashMap<ClassId, &'a str>,
            module_includes: &'a HashMap<ClassId, Vec<ClassId>>,
            block_includes: &'a dyn Fn(&ClassId) -> Vec<ClassId>,
            inherited: &'a HashSet<ClassId>,
            seen: HashSet<ClassId>,
            out: Reached,
        }
        impl Walk<'_> {
            fn visit(&mut self, id: ClassId, root: usize) {
                if self.inherited.contains(&id) || !self.seen.insert(id.clone()) {
                    return;
                }
                if !self.verified.contains(&id) {
                    if self.unverified.contains_key(&id) {
                        self.out.1.push(id);
                    }
                    return;
                }
                for dependency in self.module_includes.get(&id).cloned().into_iter().flatten() {
                    self.visit(dependency, root);
                }
                self.out.0.push((root, id.clone()));
                for included in (self.block_includes)(&id) {
                    self.visit(included, root);
                }
            }
        }
        let mut walk = Walk {
            verified: &verified,
            unverified: &unverified,
            module_includes: &module_includes,
            block_includes: &block_includes,
            inherited,
            seen: HashSet::default(),
            out: (Vec::new(), Vec::new()),
        };
        for (root, id) in lc.includes.iter().map(top).enumerate() {
            walk.visit(id, root);
        }
        walk.out
    };
    let bases: Vec<usize> = (0..app.library_classes.len())
        .filter(|&i| {
            let name = &app.library_classes[i].name;
            !verified.contains(name) && !models.contains(name)
        })
        .collect();
    let raw: HashMap<ClassId, HashSet<ClassId>> = bases
        .iter()
        .map(|&i| {
            let lc = &app.library_classes[i];
            (lc.name.clone(), reach(lc, &HashSet::default()).0.into_iter().map(|(_, id)| id).collect())
        })
        .collect();
    let by_name: HashMap<&ClassId, &crate::dialect::LibraryClass> =
        app.library_classes.iter().rev().map(|lc| (&lc.name, lc)).collect();
    // What the superclass chain already ran, and the class_attributes it
    // seeded.
    let ancestry = |lc: &crate::dialect::LibraryClass| -> (HashSet<ClassId>, HashSet<Symbol>) {
        let mut concerns = HashSet::default();
        let mut attributes = HashSet::default();
        let mut seen = HashSet::default();
        let mut at = lc.parent.clone();
        while let Some(parent) = at.take() {
            if !seen.insert(parent.clone()) {
                break;
            }
            concerns.extend(raw.get(&parent).into_iter().flatten().cloned());
            let Some(class) = by_name.get(&parent) else { break };
            attributes.extend(seeded_names(class));
            for (_, concern) in reach(class, &HashSet::default()).0 {
                attributes.extend(super::class_attribute::declared_names(
                    app.concern_model_items.get(&concern).map_or(&[][..], Vec::as_slice),
                ));
            }
            at = class.parent.clone();
        }
        (concerns, attributes)
    };

    let jobs = crate::lower::job_class_side::job_class_names(&app.library_classes);
    // A class whose superclass chain reaches a model: an STI subclass,
    // which Rails gives the model DSL its body (and an included block)
    // declares.
    let below_model = |lc: &crate::dialect::LibraryClass| -> bool {
        let mut at = lc.parent.as_ref().map(top);
        for _ in 0..8 {
            let Some(parent) = at else { return false };
            if models.contains(&parent) {
                return true;
            }
            at = by_name.get(&parent).and_then(|c| c.parent.as_ref()).map(top);
        }
        false
    };
    // The Concerns whose block a class other than a library class or a
    // model runs (a controller, a test's class, a module): their block
    // `def`s stay on the Concern (`carry_block_defs`).
    let mut hosted_elsewhere: HashSet<ClassId> = HashSet::default();
    for &index in &bases {
        let lc = &app.library_classes[index];
        let (inherited, _) = ancestry(lc);
        let (reached, blocked) = reach(lc, &inherited);
        if lc.is_module {
            hosted_elsewhere.extend(reached.into_iter().map(|(_, id)| id));
        }
        hosted_elsewhere.extend(blocked);
    }
    let mut stack: Vec<ClassId> = app
        .controllers
        .iter()
        .flat_map(crate::analyze::controller_includes)
        .chain(app.test_modules.iter().flat_map(|t| t.inner_classes.iter()).flat_map(|c| c.includes.iter().cloned()))
        .map(|id| top(&id))
        .collect();
    while let Some(id) = stack.pop() {
        if hosted_elsewhere.insert(id.clone()) {
            stack.extend(module_includes.get(&id).into_iter().flatten().cloned());
            stack.extend(block_includes(&id));
        }
    }
    let mut moved: HashSet<ClassId> = HashSet::default();
    let mut planned: Vec<Plan> = Vec::new();
    let mut refusals: Vec<(Span, String)> = Vec::new();
    let mut dropped: HashMap<Span, (String, ClassId, Vec<ClassId>)> = HashMap::default();
    let mut unrun: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for &index in &bases {
        let lc = &app.library_classes[index];
        let (inherited, mut attributes) = ancestry(lc);
        let (reached, blocked) = reach(lc, &inherited);
        for concern in blocked {
            let entry = unrun.entry(concern).or_default();
            if !entry.contains(&lc.name) {
                entry.push(lc.name.clone());
            }
        }
        if reached.is_empty() {
            continue;
        }
        attributes.extend(seeded_names(lc));
        let owner = lc.name.0.as_str();
        let job = !lc.is_module && jobs.contains(owner);
        let hook = unconsumed_hook_from(lc.includes.iter().map(top).collect(), app, job);
        let mut plan = Plan {
            index,
            methods: Vec::new(),
            initializers: Vec::new(),
            includes: Vec::new(),
            statements: Vec::new(),
            class_attributes: Vec::new(),
            carried: Vec::new(),
        };
        let sti = !lc.is_module && below_model(lc);
        let mut accessors: Vec<&Expr> = Vec::new();
        // The class-side names a carried statement gave the includer
        // (an `extend`, the callbacks a `define_model_callbacks`
        // defines), which a later statement of this or a later
        // Concern's block calls, as Ruby runs them in order.
        let mut gained: HashSet<String> = HashSet::default();
        for (root, concern) in &reached {
            let Some(items) = app.concern_model_items.get(concern) else { continue };
            let declared = super::class_attribute::declared_names(items);
            attributes.extend(declared.iter().cloned());
            // An attribute the includer defines or re-scopes itself is
            // ordered against the `include` in a body this IR no longer
            // keeps in sequence: refused, not guessed.
            let mut contested: Vec<&Symbol> = Vec::new();
            for name in &declared {
                let clash = items.iter().find_map(|item| match item {
                    ModelBodyItem::Method { method, .. } if generated_for(method, name) => lc
                        .methods
                        .iter()
                        .find(|own| own.receiver == method.receiver && own.name == method.name)
                        .map(|own| own.name.clone())
                        .or_else(|| {
                            lc.visibility_overrides
                                .iter()
                                .find(|o| o.receiver == method.receiver && o.name == method.name)
                                .map(|o| o.name.clone())
                        }),
                    _ => None,
                });
                if let Some(clash) = clash {
                    refusals.push((
                        seed_span(items, name),
                        format!(
                            "class_attribute :{name} on {owner}: `{clash}` is also defined or given a visibility by the includer, which the include's position orders"
                        ),
                    ));
                    contested.push(name);
                }
            }
            // The modules this block has included so far, in its order.
            let mut block_included: Vec<ClassId> = Vec::new();
            for item in items {
                if is_candidate(item) {
                    let ModelBodyItem::Unknown { expr, .. } = item else { unreachable!() };
                    if settled.insert((lc.name.clone(), expr.span)) {
                        accessors.push(expr);
                    }
                    continue;
                }
                if !included_modules(item, concern, &defined).is_empty() {
                    for module in included_modules(item, concern, &defined) {
                        block_included.push(module.clone());
                        if !lc.includes.iter().any(|own| top(own) == module)
                            && !plan.includes.iter().any(|(_, planned)| *planned == module)
                        {
                            plan.includes.push((*root, module));
                        }
                    }
                    continue;
                }
                // A job's callback or handler registration
                // (`before_perform`, `around_enqueue`, `set_callback`,
                // `rescue_from`, ...): run on the includer, which the
                // ActiveJob::Callbacks port gives the class methods
                // (`lower::job_class_side`), after the include it hangs off.
                if job
                    && let ModelBodyItem::Unknown { expr, .. } = item
                    && crate::lower::job_class_side::is_callbacks_port_call(expr)
                {
                    let root_id = top(&lc.includes[*root]);
                    let class_attributes: &[Symbol] = &declared;
                    if let ModelBodyItem::Unknown { expr, .. } =
                        super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                    {
                        plan.statements.push((root_id, expr));
                    }
                    continue;
                }
                // An `extend` of an app module or an ActiveModel port
                // module (activeresource's `extend ActiveModel::Callbacks`):
                // run on the includer as written, after the include.
                if !job
                    && !lc.is_module
                    && hook.is_none()
                    && let Some(surface) = extended_surface(item, concern, &defined, app)
                {
                    gained.extend(surface);
                    let root_id = top(&lc.includes[*root]);
                    let class_attributes: &[Symbol] = &declared;
                    if let ModelBodyItem::Unknown { expr, .. } =
                        super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                    {
                        plan.statements.push((root_id, expr));
                    }
                    continue;
                }
                // Any other call on the includer (`field :x, ...`,
                // `filter_param :tag, ... do ... end`): Ruby class_evals
                // the block on it, so it runs as written right after the
                // include, ordered against the block's class_attribute
                // seeds as written (`emit::ruby::library`). A macro a
                // loaded gem module the block itself included earlier
                // mixes in (Core's Apps::Extension: `include
                // SmartProperties`, then `property :uuid`) exists only
                // once that include has run, so it is written after that
                // include rather than the includer's.
                let gem_anchor = |method: &str| -> Option<ClassId> {
                    let name = Symbol::from(method);
                    block_included
                        .iter()
                        .rev()
                        .find(|m| app.gem_boundary.loaded_mixed_in_class_methods(&top(m)).contains(&name))
                        .map(top)
                };
                if !job
                    && !lc.is_module
                    && hook.is_none()
                    && let ModelBodyItem::Unknown { expr, .. } = item
                    && let ExprNode::Send { recv, method, .. } = &*expr.node
                    && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
                    && (class_side.contains(method.as_str())
                        || gained.contains(method.as_str())
                        || gem_class_side(lc, method.as_str())
                        || gem_anchor(method.as_str()).is_some()
                        || test_case_side(lc, method.as_str()))
                {
                    if method.as_str() == "define_model_callbacks" {
                        gained.extend(model_callback_names(expr));
                    }
                    let root_id = gem_anchor(method.as_str()).unwrap_or_else(|| top(&lc.includes[*root]));
                    let class_attributes: &[Symbol] = &declared;
                    if let ModelBodyItem::Unknown { expr, .. } =
                        super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                    {
                        plan.statements.push((root_id, expr));
                    }
                    continue;
                }
                // `unless method_defined?(:to_h) define_method :to_h do
                // ... end end` (Apps::Extension): Ruby asks the includer,
                // with what the block included so far, so it runs as
                // written after the last of those includes.
                if !job
                    && !lc.is_module
                    && hook.is_none()
                    && let ModelBodyItem::Unknown { expr, .. } = item
                    && guarded_define_method(expr)
                {
                    let root_id = block_included.last().map(top).unwrap_or_else(|| top(&lc.includes[*root]));
                    let class_attributes: &[Symbol] = &declared;
                    if let ModelBodyItem::Unknown { expr, .. } =
                        super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                    {
                        plan.statements.push((root_id, expr));
                    }
                    continue;
                }
                let attribute = match item {
                    ModelBodyItem::Method { method, .. } => declared.iter().find(|n| generated_for(method, n)),
                    ModelBodyItem::ClassBody { expr, .. } => {
                        declared.iter().find(|n| super::class_attribute::seeded_name(expr) == Some(n.as_str()))
                    }
                    ModelBodyItem::Unknown { expr, .. } => super::class_attribute::self_write(expr)
                        .and_then(|name| attributes.iter().find(|n| n.as_str() == name)),
                    _ => None,
                };
                let Some(attribute) = attribute else {
                    // A `cattr_*`'s accessors are reported with its seed,
                    // by the declaration itself. On a plain includer
                    // (`Module#cattr_accessor` is on every class) the
                    // accessors and the `@@x` seed run as written, and a
                    // later `self.x = ...` of the block writes through them.
                    let plain = !job && !lc.is_module && hook.is_none() && !sti;
                    if let ModelBodyItem::Method { method, .. } = item
                        && super::cattr::generated(method)
                    {
                        if plain
                            && !lc.methods.iter().any(|own| own.receiver == method.receiver && own.name == method.name)
                        {
                            gained.insert(method.name.as_str().to_string());
                            let class_attributes: &[Symbol] = &declared;
                            if let ModelBodyItem::Method { method, .. } =
                                super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                            {
                                plan.methods.retain(|m| !(m.name == method.name && m.receiver == method.receiver));
                                let key = (method.receiver, method.name.clone());
                                for g in &mut plan.class_attributes {
                                    g.methods.retain(|k| *k != key);
                                }
                                let group = attribute_group(&mut plan, top(&lc.includes[*root]));
                                plan.class_attributes[group].methods.push(key);
                                plan.methods.push(method);
                            }
                        }
                        continue;
                    }
                    if plain
                        && let ModelBodyItem::ClassBody { expr, .. } = item
                        && super::cattr::seeded_storage(expr).is_some()
                    {
                        let class_attributes: &[Symbol] = &declared;
                        if let ModelBodyItem::ClassBody { expr, .. } =
                            super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                        {
                            if !expr.span.is_synthetic() {
                                let group = attribute_group(&mut plan, top(&lc.includes[*root]));
                                plan.class_attributes[group].seeds.push(expr.span);
                            }
                            plan.initializers.push(expr);
                        }
                        continue;
                    }
                    if sti && let Some(expr) = sti_subclass_statement(item, concern, &lc.name, &declared, &defined, sources) {
                        plan.carried.push(expr);
                        continue;
                    }
                    // A local the block writes (`parent_query = superclass`):
                    // the statements below it read it, so it is written
                    // where it stands, at the includer's class body, as
                    // the block's own would have it.
                    if plain
                        && let ModelBodyItem::Unknown { expr, .. } = item
                        && matches!(&*expr.node, ExprNode::Assign { target: crate::expr::LValue::Var { .. }, .. })
                    {
                        let root_id = block_included.last().map(top).unwrap_or_else(|| top(&lc.includes[*root]));
                        let class_attributes: &[Symbol] = &declared;
                        if let ModelBodyItem::Unknown { expr, .. } =
                            super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined)
                        {
                            plan.statements.push((root_id, expr));
                        }
                        continue;
                    }
                    let (span, name) = dropped_statement(item, included_call(app, concern), sources);
                    let entry = dropped.entry(span).or_insert_with(|| (name, concern.clone(), Vec::new()));
                    if !entry.2.contains(&lc.name) {
                        entry.2.push(lc.name.clone());
                    }
                    continue;
                };
                if contested.contains(&attribute) {
                    continue;
                }
                let span = item.span();
                let span = if span.is_synthetic() { seed_span(items, attribute) } else { span };
                if lc.is_module {
                    refusals.push((
                        span,
                        format!(
                            "class_attribute :{attribute} in `included do` of {}: the block runs on the module {owner}, which has no `class_attribute` (Ruby raises NoMethodError)",
                            concern.0
                        ),
                    ));
                    contested.push(attribute);
                    continue;
                }
                if let Some(hook) = &hook {
                    refusals.push((
                        span,
                        format!(
                            "class_attribute :{attribute} on {owner}: cannot be carried alongside an unconsumed included hook or overridden framework API on {}",
                            hook.0
                        ),
                    ));
                    contested.push(attribute);
                    continue;
                }
                let class_attributes: &[Symbol] = &declared;
                // Grouped under the include it runs after, where the
                // emitted body writes it (`App::included_class_attributes`).
                let group = attribute_group(&mut plan, top(&lc.includes[*root]));
                match super::app::rehome_carried(item.clone(), concern, &lc.name, class_attributes, &defined) {
                    ModelBodyItem::Method { method, .. } => {
                        plan.methods.retain(|m: &crate::dialect::MethodDef| {
                            !(m.name == method.name && m.receiver == method.receiver)
                        });
                        let key = (method.receiver, method.name.clone());
                        for g in &mut plan.class_attributes {
                            g.methods.retain(|k| *k != key);
                        }
                        plan.class_attributes[group].methods.push(key);
                        plan.methods.push(method);
                    }
                    ModelBodyItem::ClassBody { expr, .. } | ModelBodyItem::Unknown { expr, .. } => {
                        // Found again by its span, so only a real one.
                        if !expr.span.is_synthetic() {
                            plan.class_attributes[group].seeds.push(expr.span);
                        }
                        plan.initializers.push(expr)
                    }
                    _ => unreachable!(),
                }
            }
            // The block's `def`s: Ruby class_evals them onto the
            // includer, above every module the block includes, which
            // the Concern's own methods sit below. Carried onto the
            // includer when the block includes one, so `super` and
            // dispatch follow Ruby's order (Apps::Extension's
            // `initialize(app_id:, **config)` ahead of SmartProperties').
            // An own method of the includer wins, as one it writes after
            // the include does in Ruby.
            if !block_included.is_empty() && !lc.is_module && !hosted_elsewhere.contains(concern) {
                let owns = |m: &crate::dialect::MethodDef| {
                    lc.methods.iter().any(|own| own.receiver == m.receiver && own.name == m.name)
                };
                carry_block_defs(app, concern, &lc.name, owns, &defined, &mut plan.methods);
                moved.insert(concern.clone());
            }
        }
        for expr in accessors {
            supply_accessor(lc, owner, expr, hook.as_ref(), &mut plan.methods, &mut refusals);
        }
        if !plan.methods.is_empty()
            || !plan.initializers.is_empty()
            || !plan.includes.is_empty()
            || !plan.statements.is_empty()
            || !plan.carried.is_empty()
        {
            planned.push(plan);
        }
    }
    // A model runs the same blocks (`analyze` splices their includes
    // after its own `include`), so it takes their `def`s too, once, at
    // the topmost model whose chain includes the Concern.
    let model_parents: HashMap<&ClassId, &ClassId> =
        app.models.iter().filter_map(|m| Some((&m.name, m.parent.as_ref()?))).collect();
    let model_reach = |model: &crate::dialect::Model| -> Vec<ClassId> {
        let mut out = Vec::new();
        let mut stack: Vec<ClassId> = crate::analyze::model_includes(model).iter().map(top).rev().collect();
        let mut seen = HashSet::default();
        while let Some(id) = stack.pop() {
            if !verified.contains(&id) || !seen.insert(id.clone()) {
                continue;
            }
            stack.extend(module_includes.get(&id).into_iter().flatten().cloned());
            stack.extend(block_includes(&id));
            out.push(id);
        }
        out
    };
    let mut model_methods: Vec<(usize, Vec<crate::dialect::MethodDef>)> = Vec::new();
    for (index, model) in app.models.iter().enumerate() {
        let mut inherited: HashSet<ClassId> = HashSet::default();
        let mut at = model_parents.get(&model.name).map(|p| top(p));
        for _ in 0..32 {
            let Some(parent) = at.take() else { break };
            let Some(parent_model) = app.models.iter().find(|m| m.name == parent) else { break };
            inherited.extend(model_reach(parent_model));
            at = model_parents.get(&parent_model.name).map(|p| top(p));
        }
        let mut methods = Vec::new();
        for concern in model_reach(model) {
            if inherited.contains(&concern) || block_includes(&concern).is_empty() || hosted_elsewhere.contains(&concern) {
                continue;
            }
            let owns = |m: &crate::dialect::MethodDef| {
                model.body.iter().any(|item| {
                    matches!(item, ModelBodyItem::Method { method: own, .. } if own.receiver == m.receiver && own.name == m.name)
                })
            };
            carry_block_defs(app, &concern, &model.name, owns, &defined, &mut methods);
            moved.insert(concern);
        }
        if !methods.is_empty() {
            model_methods.push((index, methods));
        }
    }
    for (index, methods) in model_methods {
        app.models[index].body.extend(methods.into_iter().map(|method| ModelBodyItem::Method {
            method,
            leading_comments: Vec::new(),
            leading_blank_line: true,
        }));
    }
    // A carried block `def` is the includer's; the Concern keeps none.
    for concern in &moved {
        let span = included_call(app, concern);
        for c in app.library_classes.iter_mut().filter(|c| c.name == *concern) {
            c.methods.retain(|m| !block_def(m, span));
        }
    }
    for plan in planned {
        if !plan.statements.is_empty() {
            let name = app.library_classes[plan.index].name.clone();
            app.included_block_statements.entry(name).or_default().extend(plan.statements);
        }
        if !plan.class_attributes.is_empty() {
            let name = app.library_classes[plan.index].name.clone();
            app.included_class_attributes.entry(name).or_default().extend(plan.class_attributes);
        }
        let lc = &mut app.library_classes[plan.index];
        lc.methods.extend(plan.methods);
        lc.unknown_calls.splice(0..0, plan.carried);
        // Run where the `include` ran, which the emitted body writes
        // ahead of every class-body statement.
        lc.class_ivar_initializers.splice(0..0, plan.initializers);
        let mut offset = 0;
        let mut last_root = None;
        for (root, module) in plan.includes {
            if last_root != Some(root) {
                offset = 0;
                last_root = Some(root);
            }
            offset += 1;
            let at = (root + offset).min(lc.includes.len());
            lc.includes.insert(at, module);
        }
    }
    let mut dropped: Vec<(Span, (String, ClassId, Vec<ClassId>))> = dropped.into_iter().collect();
    dropped.sort_by_key(|(span, _)| (span.file.0, span.start, span.end));
    for (span, (name, concern, includers)) in dropped {
        refusals.push((
            span,
            format!(
                "{name} in `included do` of {} is not modeled on a plain (non-model) includer and is dropped: {} would run without it",
                concern.0,
                some_of(&includers)
            ),
        ));
    }
    let mut unrun: Vec<(ClassId, Vec<ClassId>)> = unrun.into_iter().collect();
    unrun.sort_by(|a, b| a.0.0.as_str().cmp(b.0.0.as_str()));
    for (concern, includers) in unrun {
        refusals.push((
            included_call(app, &concern),
            format!(
                "`included do` of {} is not run on a plain (non-model) includer: {}; {} would run without it",
                concern.0,
                unverified[&concern],
                some_of(&includers)
            ),
        ));
    }
    refusals.sort_by_key(|(span, _)| (span.file.0, span.start));
    // Before the drain `app.sources` is empty; the snapshot has the
    // same FileIds.
    for (span, message) in refusals {
        let file = span.file.0.checked_sub(1).and_then(|i| sources.get(i as usize))
            .map_or_else(String::new, |source| source.path.clone());
        unwrap_or_record::<()>(Err(IngestError::Unsupported { file, message }))?;
    }
    Ok(settled)
}

/// The Concerns with an `included do` block that ingest supplies to every
/// class it runs on, or reports there: each such class is a model or a
/// plain library class or module, and there is at least one. A block on
/// a controller, a test's class or no static includer at all (an include
/// this cannot see) is not, and its emitted module still warns.
///
/// Who a block runs on follows `ActiveSupport::Concern`: a Concern's own
/// includes are deferred to its includers, a plain module's are not.
pub(super) fn carried_included_blocks(
    app: &App,
    carriers: &[super::library_class::ConcernClassMethodSpans],
) -> Vec<ClassId> {
    let concerns: HashSet<&ClassId> =
        carriers.iter().filter(|c| !c.concern_extensions.is_empty()).map(|c| &c.owner).collect();
    let mut defined: HashSet<String> = HashSet::default();
    defined.extend(app.library_classes.iter().map(|lc| lc.name.0.as_str().to_string()));
    let top = |id: &ClassId| ClassId(Symbol::from(id.0.as_str().trim_start_matches("::")));
    let mut deferred: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for lc in app.library_classes.iter().filter(|lc| concerns.contains(&lc.name)) {
        let entry = deferred.entry(lc.name.clone()).or_default();
        entry.extend(lc.includes.iter().map(top));
        for item in app.concern_model_items.get(&lc.name).into_iter().flatten() {
            entry.extend(included_modules(item, &lc.name, &defined));
        }
    }
    // Host kind: true when ingest carries a block onto it.
    let mut hosts: Vec<(Vec<ClassId>, bool)> = Vec::new();
    hosts.extend(app.models.iter().map(|m| (crate::analyze::model_includes(m), true)));
    hosts.extend(app.controllers.iter().map(|c| (crate::analyze::controller_includes(c), false)));
    hosts.extend(
        app.library_classes
            .iter()
            .filter(|lc| !concerns.contains(&lc.name))
            .map(|lc| (lc.includes.clone(), true)),
    );
    hosts.extend(
        app.test_modules
            .iter()
            .flat_map(|t| t.inner_classes.iter())
            .map(|lc| (lc.includes.clone(), false)),
    );
    hosts.extend(app.test_modules.iter().map(|t| {
        let carried = t.includes.iter().all(|id| app.test_carried_included_blocks.contains(&top(id)) || !concerns.contains(&top(id)));
        (t.includes.clone(), carried)
    }));
    let mut covered: HashMap<ClassId, bool> = HashMap::default();
    for (includes, carries) in hosts {
        let mut stack: Vec<ClassId> = includes.iter().map(top).collect();
        let mut seen = HashSet::default();
        while let Some(id) = stack.pop() {
            if !concerns.contains(&id) || !seen.insert(id.clone()) {
                continue;
            }
            *covered.entry(id.clone()).or_insert(true) &= carries;
            stack.extend(deferred.get(&id).into_iter().flatten().cloned());
        }
    }
    let mut out: Vec<ClassId> = covered
        .into_iter()
        .filter(|(id, carries)| *carries && app.concern_model_items.get(id).is_some_and(|items| !items.is_empty()))
        .map(|(id, _)| id)
        .collect();
    out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    out
}

/// The `included do … end` call of a Concern's body.
fn included_call(app: &App, concern: &ClassId) -> Span {
    app.library_classes
        .iter()
        .filter(|c| c.name == *concern)
        .flat_map(|c| c.unknown_calls.iter())
        .find(|e| {
            matches!(&*e.node, ExprNode::Send { recv: None, method, block: Some(_), .. }
                if method.as_str() == "included")
        })
        .map_or_else(Span::synthetic, |e| e.span)
}


/// An instance `def` written in the Concern's `included do` (`span`).
fn block_def(method: &crate::dialect::MethodDef, span: Span) -> bool {
    method.receiver == MethodReceiver::Instance
        && !method.name_span.is_synthetic()
        && !span.is_synthetic()
        && method.name_span.file == span.file
        && span.start <= method.name_span.start
        && method.name_span.end <= span.end
}

/// The Concern's block `def`s as the includer's own methods
/// (`splice_into_library_includers`). A constant keeps the block's
/// lexical scope: one the Concern's enclosing namespaces define is
/// qualified, and the Concern's own resolve through the includer's
/// ancestry, as `super::app::rehome_carried` does for statements.
fn carry_block_defs(
    app: &App,
    concern: &ClassId,
    owner: &ClassId,
    owns: impl Fn(&crate::dialect::MethodDef) -> bool,
    defined: &HashSet<String>,
    methods: &mut Vec<crate::dialect::MethodDef>,
) {
    fn qualify(e: &mut Expr, scopes: &[String], defined: &HashSet<String>) {
        if let ExprNode::Const { path } = &mut *e.node
            && let Some(first) = path.first()
            && !first.as_str().is_empty()
        {
            let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
            if let Some(scope) = scopes.iter().find(|scope| defined.contains(&format!("{scope}::{written}"))) {
                let mut full: Vec<Symbol> = scope.split("::").map(Symbol::from).collect();
                full.append(path);
                *path = full;
            }
            return;
        }
        e.node.for_each_child_mut(&mut |child| qualify(child, scopes, defined));
    }
    let span = included_call(app, concern);
    let segments: Vec<&str> = concern.0.as_str().split("::").collect();
    let scopes: Vec<String> = (1..segments.len()).rev().map(|n| segments[..n].join("::")).collect();
    for c in app.library_classes.iter().filter(|c| c.name == *concern) {
        for method in c.methods.iter().filter(|m| block_def(m, span)) {
            if owns(method) {
                continue;
            }
            let mut method = method.clone();
            qualify(&mut method.body, &scopes, defined);
            method.enclosing_class = Some(owner.0.clone());
            methods.retain(|m| !(m.receiver == method.receiver && m.name == method.name));
            methods.push(method);
        }
    }
}

/// `if`/`unless` over the includer's `method_defined?(:name)` (or its
/// private/public/protected spellings) whose branches only
/// `define_method(:literal) do ... end` on it.
fn guarded_define_method(expr: &Expr) -> bool {
    fn receiverless(expr: &Expr) -> Option<(&str, &[Expr], bool)> {
        let ExprNode::Send { recv, method, args, block, .. } = &*expr.node else { return None };
        recv.as_ref()
            .is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
            .then(|| (method.as_str(), args.as_slice(), block.is_some()))
    }
    fn literal(args: &[Expr]) -> bool {
        matches!(args, [arg] if matches!(&*arg.node, ExprNode::Lit { value: Literal::Sym { .. } | Literal::Str { .. } }))
    }
    fn defines(expr: &Expr) -> bool {
        match &*expr.node {
            ExprNode::Seq { exprs } => exprs.iter().all(defines),
            ExprNode::Lit { value: Literal::Nil } => true,
            _ => receiverless(expr).is_some_and(|(method, args, block)| method == "define_method" && block && literal(args)),
        }
    }
    let ExprNode::If { cond, then_branch, else_branch } = &*expr.node else { return false };
    let guard = receiverless(cond).is_some_and(|(method, args, block)| {
        !block
            && literal(args)
            && matches!(
                method,
                "method_defined?" | "public_method_defined?" | "private_method_defined?" | "protected_method_defined?"
            )
    });
    guard && defines(then_branch) && defines(else_branch)
}

/// Up to three names, then how many more.
fn some_of(classes: &[ClassId]) -> String {
    let shown: Vec<&str> = classes.iter().take(3).map(|c| c.0.as_str()).collect();
    match classes.len() - shown.len() {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// One base's share of [`splice_into_library_includers`].
struct Plan {
    index: usize,
    methods: Vec<crate::dialect::MethodDef>,
    initializers: Vec<Expr>,
    /// (index of the base's own include it follows, module).
    includes: Vec<(usize, ClassId)>,
    /// (the base's own include it runs after, statement): a job's
    /// callback registrations (`App::included_block_statements`).
    statements: Vec<(ClassId, Expr)>,
    /// The carried `class_attribute` defs and seeds, by the include they
    /// run after (`App::included_class_attributes`).
    class_attributes: Vec<crate::app::IncludedClassAttributes>,
    /// An STI subclass's model DSL statements, in block order, ahead of
    /// its own body calls ([`sti_subclass_statement`]).
    carried: Vec<Expr>,
}

/// The group of `plan.class_attributes` that runs after the include of
/// `after`, made on first use.
fn attribute_group(plan: &mut Plan, after: ClassId) -> usize {
    match plan.class_attributes.iter().position(|g| g.after == after) {
        Some(group) => group,
        None => {
            plan.class_attributes.push(crate::app::IncludedClassAttributes {
                after,
                methods: Vec::new(),
                seeds: Vec::new(),
            });
            plan.class_attributes.len() - 1
        }
    }
}

/// An `included do` statement as a body call of the STI subclass
/// `includer`, its constants resolved from the Concern's scope
/// (`app::rehome_carried`): the statement itself or, for one ingest
/// recognized as a declaration (a validation, a callback, an
/// association or a scope, which keep no expression), its source read
/// again at its span. Not a method the block defines or a macro
/// generated.
fn sti_subclass_statement(
    item: &ModelBodyItem,
    concern: &ClassId,
    includer: &ClassId,
    class_attributes: &[Symbol],
    defined: &HashSet<String>,
    sources: &[crate::span::SourceFile],
) -> Option<Expr> {
    let expr = match item {
        ModelBodyItem::Method { .. } => return None,
        ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => expr.clone(),
        _ => {
            let span = item.span();
            if span.is_synthetic() {
                return None;
            }
            let source = span.file.0.checked_sub(1).and_then(|i| sources.get(i as usize))?;
            let parsed = ruby_prism::parse(source.text.as_bytes());
            let root = parsed.node();
            let node = super::define_method_loops::node_at(&root, span.start as usize, span.end as usize, |n| {
                n.as_call_node().is_some()
            })?;
            super::expr::ingest_expr(&node, &source.path).ok()?
        }
    };
    let item = ModelBodyItem::Unknown { expr, leading_comments: Vec::new(), leading_blank_line: false };
    match super::app::rehome_carried(item, concern, includer, class_attributes, defined) {
        ModelBodyItem::Unknown { expr, .. } => Some(expr),
        _ => None,
    }
}

/// The `class_attribute` names a library class body seeds.
fn seeded_names(lc: &crate::dialect::LibraryClass) -> impl Iterator<Item = Symbol> + '_ {
    lc.class_ivar_initializers.iter().filter_map(|e| super::class_attribute::seeded_name(e).map(Symbol::from))
}

/// One of the defs a `class_attribute :name` generates (synthesized).
fn generated_for(method: &crate::dialect::MethodDef, name: &Symbol) -> bool {
    method.name_span.is_synthetic() && method.name.as_str().trim_end_matches(['=', '?']) == name.as_str()
}

/// The declaration a `class_attribute :name` came from: its seed's span.
fn seed_span(items: &[ModelBodyItem], name: &Symbol) -> Span {
    items
        .iter()
        .find_map(|item| match item {
            ModelBodyItem::ClassBody { expr, .. }
                if super::class_attribute::seeded_name(expr) == Some(name.as_str()) =>
            {
                Some(expr.span)
            }
            _ => None,
        })
        .unwrap_or_else(Span::synthetic)
}

/// The modules a carried `include M, …` names, each resolved in the
/// Concern's lexical scope (the block's), innermost first.
pub(super) fn included_modules(item: &ModelBodyItem, concern: &ClassId, defined: &HashSet<String>) -> Vec<ClassId> {
    written_modules(item, "include", concern, defined)
}

/// The class methods a carried `extend M, …` gives the includer, when
/// each M is an app module (its instance methods) or a module of the
/// ActiveModel::Validations port (`runtime_reads::extended_class_methods`:
/// ActiveModel::Callbacks' `define_model_callbacks`, and
/// ActiveSupport::Callbacks' `define_callbacks` and `set_callback`).
/// `None` for any other `extend`, which stays refused by name.
fn extended_surface(item: &ModelBodyItem, concern: &ClassId, defined: &HashSet<String>, app: &App) -> Option<Vec<String>> {
    let modules = written_modules(item, "extend", concern, defined);
    if modules.is_empty() {
        return None;
    }
    let mut surface = Vec::new();
    for module in modules {
        if let Some(lc) = app.library_classes.iter().find(|lc| lc.is_module && lc.name == module) {
            surface.extend(
                lc.methods.iter().filter(|m| m.receiver == MethodReceiver::Instance).map(|m| m.name.as_str().to_string()),
            );
        } else if !defined.contains(module.0.as_str()) {
            surface.extend(crate::analyze::runtime_reads::extended_class_methods(module.0.as_str())?.iter().cloned());
        } else {
            return None;
        }
    }
    Some(surface)
}

/// The class methods a `define_model_callbacks :save, …` defines
/// (activemodel's callbacks.rb): `before_save`, `around_save` and
/// `after_save` for each literal name, or the kinds its `only:` names.
fn model_callback_names(call: &Expr) -> Vec<String> {
    let ExprNode::Send { args, .. } = &*call.node else { return Vec::new() };
    let sym = |e: &Expr| match &*e.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        _ => None,
    };
    let mut kinds = vec!["before".to_string(), "around".to_string(), "after".to_string()];
    let mut names = Vec::new();
    for arg in args {
        if let ExprNode::Hash { entries, .. } = &*arg.node {
            for (key, value) in entries {
                if sym(key).as_deref() != Some("only") {
                    continue;
                }
                kinds = match &*value.node {
                    ExprNode::Array { elements, .. } => elements.iter().filter_map(sym).collect(),
                    _ => sym(value).into_iter().collect(),
                };
            }
        } else if let Some(name) = sym(arg) {
            names.push(name);
        }
    }
    names.iter().flat_map(|name| kinds.iter().map(move |kind| format!("{kind}_{name}"))).collect()
}

/// The modules a carried `include M, …` or `extend M, …` (`verb`) names,
/// each resolved in the Concern's lexical scope (the block's), innermost
/// first.
fn written_modules(item: &ModelBodyItem, verb: &str, concern: &ClassId, defined: &HashSet<String>) -> Vec<ClassId> {
    let ModelBodyItem::Unknown { expr, .. } = item else { return Vec::new() };
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else { return Vec::new() };
    if method.as_str() != verb || args.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for arg in args {
        let ExprNode::Const { path } = &*arg.node else { return Vec::new() };
        let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
        if let Some(rooted) = written.strip_prefix("::") {
            out.push(ClassId(Symbol::from(rooted)));
            continue;
        }
        let segments: Vec<&str> = concern.0.as_str().split("::").collect();
        let resolved = (0..=segments.len())
            .rev()
            .map(|n| if n == 0 { written.clone() } else { format!("{}::{written}", segments[..n].join("::")) })
            .find(|candidate| defined.contains(candidate))
            .unwrap_or(written);
        out.push(ClassId(Symbol::from(resolved)));
    }
    out
}

/// Where an `included do` statement is reported, and what it is called
/// there: its leading name, read off the source (the typed items keep no
/// expression). A method a macro generated has no span of its own; it is
/// reported once, at the `included` call.
fn dropped_statement(item: &ModelBodyItem, included: Span, sources: &[crate::span::SourceFile]) -> (Span, String) {
    let span = match item {
        ModelBodyItem::Method { method, .. } if method.name_span.is_synthetic() || method.body.span.is_synthetic() => {
            return (included, format!("the generated method `{}` (and any others its macro defines)", method.name));
        }
        ModelBodyItem::Scope { .. } => return (item.span(), "`scope`".to_string()),
        _ => item.span(),
    };
    let text = span
        .file
        .0
        .checked_sub(1)
        .and_then(|i| sources.get(i as usize))
        .and_then(|source| source.text.get(span.start as usize..span.end as usize))
        .unwrap_or("");
    let name: String = text
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '?' | '!' | ':' | '='))
        .collect();
    let name = name.trim_end_matches(':');
    (span, if name.is_empty() { "a statement".to_string() } else { format!("`{name}`") })
}

/// A literal `attr_*` declaration's accessors on `lc`, or its refusal.
fn supply_accessor(
    lc: &crate::dialect::LibraryClass,
    owner: &str,
    expr: &Expr,
    hook: Option<&ClassId>,
    methods: &mut Vec<crate::dialect::MethodDef>,
    refusals: &mut Vec<(Span, String)>,
) {
    use crate::dialect::{AccessorKind, MethodVisibility};

    let refuse = |detail: String| (expr.span, format!("concern attr_accessor on {owner}: {detail}"));
    if let Some(DiagnosticKind::Unsupported { construct, detail, .. }) = &expr.diagnostic
        && detail != SHAPE_DETAIL
    {
        refusals.push((expr.span, format!("{construct} on {owner} {detail}")));
        return;
    }
    if let Some(hook) = hook {
        refusals.push(refuse(format!(
            "cannot be carried alongside an unconsumed included hook or overridden framework API on {}",
            hook.0
        )));
        return;
    }
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
        refusals.push(refuse("unsupported accessor shape: only a direct literal-Symbol attr_* call is modeled".into()));
        return;
    };
    let names: Option<Vec<&Symbol>> = args
        .iter()
        .map(|arg| match &*arg.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value),
            _ => None,
        })
        .collect();
    let Some(names) = names.filter(|names| !names.is_empty()) else {
        refusals.push(refuse("unsupported accessor shape: only a direct literal-Symbol attr_* call is modeled".into()));
        return;
    };
    let (reader, writer) = match method.as_str() {
        "attr_reader" => (true, false),
        "attr_writer" => (false, true),
        _ => (true, true),
    };
    for name in names {
        let mut defs = Vec::new();
        if reader {
            defs.push(super::library_class::synth_attr_reader(&lc.name, name, MethodReceiver::Instance));
        }
        if writer {
            defs.push(super::library_class::synth_attr_writer(&lc.name, name, MethodReceiver::Instance));
        }
        for def in defs {
            if methods.iter().any(|m| m.name == def.name) {
                continue;
            }
            // The includer's own definition of the name, or a
            // visibility statement naming it, is ordered against
            // the `include` in a body this IR no longer keeps in
            // sequence. Only an identical public accessor is
            // indifferent to that order.
            let own = lc.methods.iter().find(|m| m.receiver == MethodReceiver::Instance && m.name == def.name);
            let restated = own.is_some_and(|m| {
                m.visibility == MethodVisibility::Public
                    && m.kind == def.kind
                    && matches!(m.kind, AccessorKind::AttributeReader | AccessorKind::AttributeWriter)
                    && m.body == def.body
            });
            let overridden = lc
                .visibility_overrides
                .iter()
                .any(|o| o.receiver == MethodReceiver::Instance && o.name == def.name);
            if overridden || (own.is_some() && !restated) {
                refusals.push(refuse(format!(
                    "`{}` is also defined or given a visibility by the includer, which the include's position orders",
                    def.name.as_str()
                )));
                continue;
            }
            if !restated {
                methods.push(def);
            }
        }
    }
}
