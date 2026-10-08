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
    unconsumed_hook_from(crate::analyze::model_includes(model), app)
}

/// [`unconsumed_included_hook`] from an includer's direct includes.
fn unconsumed_hook_from(includes: Vec<ClassId>, app: &App) -> Option<crate::ClassId> {
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
                if super::class_configuration::overrides_framework_api(method)
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
    let resolver = app.const_resolver.for_sources(&app.sources);
    let mut includes = BTreeMap::<crate::ClassId, Vec<crate::ClassId>>::new();
    let mut modules = HashSet::default();
    let retained: HashSet<_> = app.library_classes.iter().map(|c| &c.name)
        .chain(app.models.iter().map(|m| &m.name))
        .chain(app.controllers.iter().map(|c| &c.name))
        .chain(app.test_modules.iter().map(|t| &t.name))
        .cloned()
        // Test ingest retains inner classes under file-local names; the
        // source resolver correctly addresses their enclosing test class.
        .chain(app.test_modules.iter().flat_map(|t| t.inner_classes.iter().map(|c| {
            crate::ClassId(Symbol::from(format!("{}::{}", t.name.0, c.name.0)))
        })))
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
            let edges = includes.entry(owner.clone()).or_default();
            for statement in flatten_statements(body) {
                let Some(call) = statement.as_call_node() else { continue };
                if call.receiver().is_some() || call.block().is_some() {
                    continue;
                }
                let name = call.name();
                let name = constant_id_str(&name);
                if name != "include" {
                    continue;
                }
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
    for (owner, direct) in &includes {
        // A survey source refusal already diagnosed an omitted class.
        if deferred.contains(owner) || (!retained.contains(owner) && !modules.contains(owner)) {
            continue;
        }
        let mut model = app.models.iter_mut().find(|m| &m.name == owner);
        if let Some(model) = model.as_mut() {
            for item in &mut model.body {
                let ModelBodyItem::Unknown { expr, .. } = item else { continue };
                if spans.contains(&expr.span) && !direct.iter().any(|id| {
                    deferred.contains(id) && app.concern_model_items.get(id).is_some_and(|items| items.iter().any(|item| {
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
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(items) = app.concern_model_items.get(&id) {
                for item in items.iter().filter(|item| is_candidate(item)) {
                    let ModelBodyItem::Unknown { expr, .. } = item else { unreachable!() };
                    // `splice_into_library_includers` supplied or refused it.
                    if model.is_none() && settled.contains(&(owner.clone(), expr.span)) {
                        continue;
                    }
                    let supplied = direct.contains(&id) && model.as_ref().is_some_and(|m| m.body.iter().any(|item| {
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
    // full Core a second complete analysis. Only a name that passes every
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
/// `base.class_eval(&@_included_block)`. Its literal `attr_reader` /
/// `attr_writer` / `attr_accessor` declarations therefore define public
/// ivar accessors on that includer, which is what this supplies.
///
/// The base is whoever performed the include: a Concern defers its own
/// includes to its includer (`@_dependencies`), so they are followed; a
/// plain module performed its includes on itself, so the accessors land
/// on that module (an owner here in its own right) and reach its
/// includers through Ruby's include. A Concern runs once per base.
///
/// Returns every (includer, declaration) this settled, supplied or
/// refused, so the activation check does not report it again; whatever
/// it did not reach is still refused there.
pub(super) fn splice_into_library_includers(
    app: &mut App,
    sources: &[crate::span::SourceFile],
    carriers: &[super::library_class::ConcernClassMethodSpans],
    framework_shadows: &HashSet<ClassId>,
) -> IngestResult<HashSet<(ClassId, Span)>> {
    use crate::dialect::{AccessorKind, MethodVisibility};

    let mut settled = HashSet::default();
    if app.concern_model_items.values().flatten().all(|item| !is_candidate(item)) {
        return Ok(settled);
    }
    // A kept root (`include ::A::B`, where `Owner::A` also exists)
    // names the same top-level module.
    let top = |id: &ClassId| ClassId(Symbol::from(id.0.as_str().trim_start_matches("::")));
    let mut module_includes: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for lc in &app.library_classes {
        module_includes.entry(lc.name.clone()).or_default().extend(lc.includes.iter().map(top));
    }
    let verified =
        super::class_configuration::verified_framework_concerns(carriers, &module_includes, framework_shadows);
    let models: HashSet<&ClassId> = app.models.iter().map(|m| &m.name).collect();
    let mut planned: Vec<(usize, Vec<crate::dialect::MethodDef>)> = Vec::new();
    let mut refusals: Vec<(Span, String)> = Vec::new();
    for (index, lc) in app.library_classes.iter().enumerate() {
        if verified.contains(&lc.name) || models.contains(&lc.name) {
            continue;
        }
        // The Concerns whose blocks run on this base, each once. Their
        // order is immaterial: every one defines the same ivar accessor.
        let mut pending: Vec<ClassId> = lc.includes.iter().rev().map(top).collect();
        let mut seen = HashSet::default();
        let mut reached = Vec::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) || !verified.contains(&id) {
                continue;
            }
            pending.extend(module_includes.get(&id).into_iter().flatten().rev().cloned());
            reached.push(id);
        }
        let candidates: Vec<&Expr> = reached
            .iter()
            .filter_map(|id| app.concern_model_items.get(id))
            .flatten()
            .filter(|item| is_candidate(item))
            .map(|item| {
                let ModelBodyItem::Unknown { expr, .. } = item else { unreachable!() };
                expr
            })
            .filter(|expr| settled.insert((lc.name.clone(), expr.span)))
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let owner = lc.name.0.as_str();
        let hook = unconsumed_hook_from(lc.includes.iter().map(top).collect(), app);
        let mut methods: Vec<crate::dialect::MethodDef> = Vec::new();
        for expr in candidates {
            let refuse = |detail: String| (expr.span, format!("concern attr_accessor on {owner}: {detail}"));
            if let Some(DiagnosticKind::Unsupported { construct, detail, .. }) = &expr.diagnostic
                && detail != SHAPE_DETAIL
            {
                refusals.push((expr.span, format!("{construct} on {owner} {detail}")));
                continue;
            }
            if let Some(hook) = &hook {
                refusals.push(refuse(format!(
                    "cannot be carried alongside an unconsumed included hook or overridden framework API on {}",
                    hook.0
                )));
                continue;
            }
            let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
                refusals.push(refuse("unsupported accessor shape: only a direct literal-Symbol attr_* call is modeled".into()));
                continue;
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
                continue;
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
        if !methods.is_empty() {
            planned.push((index, methods));
        }
    }
    for (index, methods) in planned {
        app.library_classes[index].methods.extend(methods);
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
