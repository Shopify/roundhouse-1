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
                let consumed = job
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
/// Any other statement of the block (a validation, a callback, a gem's
/// class-body DSL) is reported, once per statement: the emitted includer
/// would run without it. So is the whole block of a Concern this cannot
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
        };
        let mut accessors: Vec<&Expr> = Vec::new();
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
                let after = top(&lc.includes[*root]);
                let group = match plan.class_attributes.iter().position(|g| g.after == after) {
                    Some(group) => group,
                    None => {
                        plan.class_attributes.push(crate::app::IncludedClassAttributes {
                            after,
                            methods: Vec::new(),
                            seeds: Vec::new(),
                        });
                        plan.class_attributes.len() - 1
                    }
                };
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
        }
        for expr in accessors {
            supply_accessor(lc, owner, expr, hook.as_ref(), &mut plan.methods, &mut refusals);
        }
        if !plan.methods.is_empty()
            || !plan.initializers.is_empty()
            || !plan.includes.is_empty()
            || !plan.statements.is_empty()
        {
            planned.push(plan);
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
fn included_modules(item: &ModelBodyItem, concern: &ClassId, defined: &HashSet<String>) -> Vec<ClassId> {
    let ModelBodyItem::Unknown { expr, .. } = item else { return Vec::new() };
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else { return Vec::new() };
    if method.as_str() != "include" || args.is_empty() {
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
