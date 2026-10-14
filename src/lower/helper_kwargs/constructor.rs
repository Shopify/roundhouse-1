//! Constructor-call normalization, separate from ordinary helper keyword calls.
//!
//! This pass has stricter semantic constraints: moving keywords must preserve
//! Ruby evaluation order and defaults must retain their lexical meaning.

use crate::det::{HashMap, HashSet};

use crate::app::App;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

use super::{Slot, slot_of};

pub(super) struct InstanceCallParams {
    pub(super) slots: HashMap<(String, Symbol), Vec<Slot>>,
    pub(super) custom_new_slots: HashMap<(String, Symbol), Vec<Slot>>,
    custom_new: HashSet<String>,
    pub(super) unknown_new: HashSet<String>,
    pub(super) lexical_refinement_calls: HashSet<crate::span::Span>,
    unqualified_class_ids: HashMap<String, String>,
    ambiguous_names: HashSet<String>,
    model_names: HashSet<String>,
    /// The first `initialize`/`new` (`Owner#name`) whose flattened
    /// keywords the Ruby family does not write back as keywords
    /// (`Param::keywords_written_as_keywords`). With none, a constructor
    /// call the CRuby tree leaves as written binds as Ruby's does.
    ruby_family_positional_constructor: Option<String>,
    /// Classes whose `Dry::Initializer` declarations a refused hierarchy
    /// (`ingest::dry_initializer`) left as unmodeled calls, and their
    /// subclasses: no tree carries those declarations, so a constructor call
    /// on one runs as written only into a class that cannot take it.
    declarations_dropped: HashSet<String>,
}

impl InstanceCallParams {
    pub(super) fn new(
        app: &App,
        constructors: HashMap<
            crate::ident::ClassId,
            crate::analyze::forwarding::ConstructorContract<'_>,
        >,
        names: &super::super::forwarding::ConstructorNames,
    ) -> Self {
        let mut slots: HashMap<(String, Symbol), Vec<Slot>> = HashMap::default();
        for class in &app.library_classes {
            for method in &class.methods {
                if method.receiver != crate::dialect::MethodReceiver::Instance {
                    continue;
                }
                let key = (class.name.0.as_str().to_string(), method.name.clone());
                // A later native definition replaces the earlier flattened
                // ABI, just like class-method keyword normalization.
                slots.remove(&key);
                if method
                    .params
                    .iter()
                    .any(|param| param.rest || param.keyword || param.forwarding)
                    || !method.params.iter().any(|param| param.from_keyword)
                {
                    continue;
                }
                slots.insert(key, method.params.iter().map(slot_of).collect());
            }
        }
        // A model's own instance methods, and a concern's methods under
        // the one class that includes it: campfire's
        // `@message.broadcast_create(html: @message_html)` against
        // `Message::Broadcasts#broadcast_create(html: nil)`, which the
        // receiver types as `Message`. With several includers the
        // receiver still names one, but which module's definition wins
        // is the includer's ancestry, so those are left alone.
        let mut defined: HashSet<(String, Symbol)> = HashSet::default();
        for model in &app.models {
            for method in model.methods() {
                if method.receiver != crate::dialect::MethodReceiver::Instance {
                    continue;
                }
                let key = (model.name.0.as_str().to_string(), method.name.clone());
                defined.insert(key.clone());
                slots.remove(&key);
                if let Some(params) = flattened_keyword_slots(method) {
                    slots.insert(key, params);
                }
            }
        }
        for (module, includer) in app.sole_includer_of_modules() {
            let Some(class) = app.library_classes.iter().find(|c| c.name == module) else {
                continue;
            };
            for method in &class.methods {
                if method.receiver != crate::dialect::MethodReceiver::Instance {
                    continue;
                }
                let key = (includer.0.as_str().to_string(), method.name.clone());
                if defined.contains(&key) || slots.contains_key(&key) {
                    continue;
                }
                if let Some(params) = flattened_keyword_slots(method) {
                    slots.insert(key, params);
                }
            }
        }

        let mut custom_new = HashSet::default();
        let mut custom_new_slots = HashMap::default();
        let mut unknown_new = HashSet::default();
        let unqualified_class_ids = names.unqualified_class_ids.clone();
        for (class_id, contract) in constructors {
            add_constructor_slots(
                &class_id,
                contract,
                &mut slots,
                &mut custom_new_slots,
                &mut custom_new,
                &mut unknown_new,
            );
        }
        Self {
            slots,
            custom_new_slots,
            custom_new,
            unknown_new,
            lexical_refinement_calls: names.lexical_refinement_calls.clone(),
            unqualified_class_ids,
            ambiguous_names: names.ambiguous_names.clone(),
            model_names: names.model_names.clone(),
            ruby_family_positional_constructor: ruby_family_positional_constructor(app),
            declarations_dropped: declarations_dropped(app),
        }
    }
}

/// The classes holding Dry::Initializer declarations no tree carries, and
/// every subclass of one.
fn declarations_dropped(app: &App) -> HashSet<String> {
    let mut dropped: HashSet<String> = app
        .library_classes
        .iter()
        .filter(|lc| lc.unknown_calls.iter().any(crate::ingest::is_initializer_declaration))
        .map(|lc| lc.name.0.as_str().to_string())
        .collect();
    loop {
        let before = dropped.len();
        for lc in &app.library_classes {
            if lc.parent.as_ref().is_some_and(|p| dropped.contains(p.0.as_str())) {
                dropped.insert(lc.name.0.as_str().to_string());
            }
        }
        if dropped.len() == before {
            return dropped;
        }
    }
}

/// The first constructor whose flattened keywords the Ruby family does
/// not write back. Ingest flattens an optional keyword into a
/// positional-with-default (`Param::from_keyword`), which is what makes an
/// unverified `new` lookup unsafe on targets without keyword arguments. A
/// native-keyword tree writes a method's trailing flattened keywords back
/// (`emit_method`), and keeps every unrewritten call's keywords, so the
/// binding is Ruby's own. One `initialize` or `new` anywhere whose
/// flattened keywords stay positional could be the effective constructor
/// of an unverified lookup, so it keeps every such refusal on that tree.
fn ruby_family_positional_constructor(app: &App) -> Option<String> {
    let offending = |owner: &str, method: &crate::dialect::MethodDef| {
        (matches!(method.name.as_str(), "initialize" | "new")
            && !crate::dialect::Param::keywords_written_as_keywords(&method.params))
        .then(|| format!("{owner}#{}", method.name.as_str()))
    };
    app.library_classes
        .iter()
        .chain(app.rails_application.iter())
        .chain(
            app.test_modules
                .iter()
                .flat_map(|module| module.inner_classes.iter()),
        )
        .find_map(|class| {
            class
                .methods
                .iter()
                .find_map(|method| offending(class.name.0.as_str(), method))
        })
        .or_else(|| {
            app.models.iter().find_map(|model| {
                model
                    .methods()
                    .find_map(|method| offending(model.name.0.as_str(), method))
            })
        })
        .or_else(|| {
            app.test_modules.iter().find_map(|module| {
                module
                    .helpers
                    .iter()
                    .find_map(|method| offending(module.name.0.as_str(), method))
            })
        })
}

/// The positional slots of a method whose keywords ingest flattened, or
/// `None` when it took none or kept a parameter this rule cannot move.
fn flattened_keyword_slots(method: &crate::dialect::MethodDef) -> Option<Vec<Slot>> {
    if method.params.iter().any(|param| param.rest || param.keyword || param.forwarding)
        || !method.params.iter().any(|param| param.from_keyword)
    {
        return None;
    }
    Some(method.params.iter().map(slot_of).collect())
}

pub(super) fn lexical_refinement_calls(app: &App) -> HashSet<crate::span::Span> {
    fn using_spans(expr: &Expr, owner: &str, activations: &mut Vec<(String, crate::span::Span)>) {
        if matches!(&*expr.node, ExprNode::Send { method, .. } if method.as_str() == "using") {
            activations.push((owner.to_string(), expr.span));
        }
        expr.node
            .for_each_child(&mut |child| using_spans(child, owner, activations));
    }
    fn collect_new_calls(
        expr: &Expr,
        refinements: &[crate::span::Span],
        calls: &mut HashSet<crate::span::Span>,
    ) {
        if matches!(&*expr.node, ExprNode::Send { method, .. } if method.as_str() == "new")
            && refinements
                .iter()
                .any(|using| using.file == expr.span.file && using.start < expr.span.start)
        {
            calls.insert(expr.span);
        }
        expr.node
            .for_each_child(&mut |child| collect_new_calls(child, refinements, calls));
    }

    fn refinements_for(
        owner: &str,
        activations: &[(String, crate::span::Span)],
    ) -> Vec<crate::span::Span> {
        activations
            .iter()
            .filter(|(scope, _)| {
                scope.is_empty()
                    || owner == scope
                    || owner
                        .strip_prefix(scope)
                        .is_some_and(|tail| tail.starts_with("::"))
            })
            .map(|(_, span)| *span)
            .collect()
    }

    let mut activations = Vec::new();
    for class in &app.library_classes {
        let owner = class.name.0.as_str();
        for expr in &class.unknown_calls {
            using_spans(expr, owner, &mut activations);
        }
        for (_, expr) in &class.constants {
            using_spans(expr, owner, &mut activations);
        }
    }
    for model in &app.models {
        for item in &model.body {
            if let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item {
                using_spans(expr, model.name.0.as_str(), &mut activations);
            }
        }
    }
    for (index, source) in app.sources.iter().enumerate() {
        if !source.path.ends_with(".rb") || !source.text.contains("using") {
            continue;
        }
        let parsed = ruby_prism::parse(source.text.as_bytes());
        let Some(program) = parsed.node().as_program_node() else {
            continue;
        };
        struct TopLevelUsing {
            spans: Vec<(u32, u32)>,
        }
        impl<'pr> ruby_prism::Visit<'pr> for TopLevelUsing {
            fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
                if call.receiver().is_none() && call.name().as_slice() == b"using" {
                    let location = call.location();
                    self.spans
                        .push((location.start_offset() as u32, location.end_offset() as u32));
                }
                ruby_prism::visit_call_node(self, call);
            }

            fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}

            fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}

            fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}

            fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
        }
        let mut using = TopLevelUsing { spans: Vec::new() };
        ruby_prism::Visit::visit(&mut using, &program.as_node());
        for (start, end) in using.spans {
            activations.push((
                String::new(),
                crate::span::Span {
                    file: crate::span::FileId(index as u32 + 1),
                    start,
                    end,
                },
            ));
        }
    }

    let mut spans = HashSet::default();
    for class in &app.library_classes {
        let refinements = refinements_for(class.name.0.as_str(), &activations);
        if !refinements.is_empty() {
            for expr in &class.unknown_calls {
                collect_new_calls(expr, &refinements, &mut spans);
            }
            for (_, expr) in &class.constants {
                collect_new_calls(expr, &refinements, &mut spans);
            }
            for expr in &class.class_ivar_initializers {
                collect_new_calls(expr, &refinements, &mut spans);
            }
            for method in &class.methods {
                collect_new_calls(&method.body, &refinements, &mut spans);
                for default in method
                    .params
                    .iter()
                    .filter_map(|param| param.default.as_ref())
                {
                    collect_new_calls(default, &refinements, &mut spans);
                }
            }
        }
    }
    for model in &app.models {
        let refinements = refinements_for(model.name.0.as_str(), &activations);
        if !refinements.is_empty() {
            for method in model.methods() {
                collect_new_calls(&method.body, &refinements, &mut spans);
                for default in method
                    .params
                    .iter()
                    .filter_map(|param| param.default.as_ref())
                {
                    collect_new_calls(default, &refinements, &mut spans);
                }
            }
        }
    }
    spans
}

fn add_constructor_slots(
    class_id: &crate::ident::ClassId,
    contract: crate::analyze::forwarding::ConstructorContract<'_>,
    slots: &mut HashMap<(String, Symbol), Vec<Slot>>,
    custom_new_slots: &mut HashMap<(String, Symbol), Vec<Slot>>,
    custom_new: &mut HashSet<String>,
    unknown_new: &mut HashSet<String>,
) {
    let class_name = class_id.0.as_str().to_string();
    let method = match contract {
        crate::analyze::forwarding::ConstructorContract::CustomNew(method) => {
            custom_new.insert(class_name.clone());
            if let Some(method) = method
                && !method
                    .params
                    .iter()
                    .any(|param| param.rest || param.keyword || param.forwarding)
                && method.params.iter().any(|param| param.from_keyword)
            {
                custom_new_slots.insert(
                    (class_name, Symbol::from("new")),
                    method.params.iter().map(slot_of).collect(),
                );
            }
            return;
        }
        crate::analyze::forwarding::ConstructorContract::UnknownLookup => {
            unknown_new.insert(class_name);
            return;
        }
        crate::analyze::forwarding::ConstructorContract::Initialize(method) => method,
    };
    if method
        .params
        .iter()
        .any(|param| param.rest || param.keyword || param.forwarding)
        || !method.params.iter().any(|param| param.from_keyword)
    {
        return;
    }
    slots.insert(
        (class_name, Symbol::from("initialize")),
        method.params.iter().map(slot_of).collect(),
    );
}

pub(super) fn refuse_keyword_splats(
    expr: &mut Expr,
    params: &InstanceCallParams,
    diagnostics: &mut Vec<crate::diagnostic::Diagnostic>,
) {
    expr.node
        .for_each_child_mut(&mut |child| refuse_keyword_splats(child, params, diagnostics));
    let ExprNode::Send {
        recv: Some(recv),
        method,
        args,
        ..
    } = &*expr.node
    else {
        return;
    };
    if method.as_str() != "new" || !matches!(&*recv.node, ExprNode::Const { .. }) {
        return;
    }
    let Some(crate::ty::Ty::Class { id, .. }) = &recv.ty else {
        return;
    };
    let class_name = params
        .unqualified_class_ids
        .get(id.0.as_str())
        .map_or(id.0.as_str(), String::as_str);
    if let Some(reason) =
        constructor_refusal_reason(params, expr.span, id.0.as_str(), class_name, args)
    {
        refuse_constructor(expr, params, &class_name.to_string(), reason, diagnostics);
    }
}

pub(super) fn rewrite_instance_call_node(
    expr: &mut Expr,
    params: &InstanceCallParams,
    diagnostics: &mut Vec<crate::diagnostic::Diagnostic>,
) {
    rewrite_instance_call(expr, params, diagnostics);
}

fn rewrite_instance_call(
    expr: &mut Expr,
    params: &InstanceCallParams,
    diagnostics: &mut Vec<crate::diagnostic::Diagnostic>,
) {
    if matches!(
        &expr.diagnostic,
        Some(crate::diagnostic::DiagnosticKind::Unsupported { construct, .. })
            if construct.as_str() == crate::diagnostic::CONSTRUCTOR_KEYWORD_ARGUMENTS
    ) {
        return;
    }
    if params.slots.is_empty()
        && params.unknown_new.is_empty()
        && params.ambiguous_names.is_empty()
        && params.lexical_refinement_calls.is_empty()
    {
        return;
    }
    let ExprNode::Send {
        recv: Some(recv),
        method,
        args,
        ..
    } = &mut *expr.node
    else {
        return;
    };
    let Some(crate::ty::Ty::Class { id, .. }) = &recv.ty else {
        return;
    };
    let is_constructor = method.as_str() == "new" && matches!(&*recv.node, ExprNode::Const { .. });
    // Owned: `refuse_constructor` marks `expr` while the name is read.
    let id_name = id.0.as_str().to_string();
    let class_id = if is_constructor {
        params.unqualified_class_ids.get(&id_name).map_or(id_name.as_str(), String::as_str)
    } else {
        id_name.as_str()
    };
    if is_constructor
        && let Some(reason) =
            constructor_refusal_reason(params, expr.span, &id_name, class_id, args)
    {
        refuse_constructor(expr, params, class_id, reason, diagnostics);
        return;
    }
    if is_constructor {
        if params.custom_new.contains(class_id) {
            return;
        }
    }
    if !is_constructor && super::SEND_METHODS.contains(&method.as_str()) {
        let class_id = class_id.to_string();
        super::owned::refuse_send(
            expr.span,
            args,
            |name| params.slots.contains_key(&(class_id.clone(), name.clone())),
            diagnostics,
        );
        return;
    }
    let method = if is_constructor {
        Symbol::from("initialize")
    } else {
        method.clone()
    };
    let Some(slots) = params.slots.get(&(class_id.to_string(), method)) else {
        return;
    };
    if is_constructor {
        if respell_constructor(args, slots).is_err() {
            refuse_constructor(
                expr,
                params,
                &class_id.to_string(),
                "cannot safely move these keywords into initialize's positional slots without changing Ruby argument binding or evaluation semantics",
                diagnostics,
            );
        }
    } else if let Err(reason) = super::respell(args, slots, true) {
        super::refuse(expr.span, reason, diagnostics);
    }
}

pub(super) fn rewrite_custom_new_call(
    expr: &mut Expr,
    params: &InstanceCallParams,
    diagnostics: &mut Vec<crate::diagnostic::Diagnostic>,
) -> bool {
    let ExprNode::Send {
        recv: Some(recv),
        method,
        args,
        ..
    } = &mut *expr.node
    else {
        return false;
    };
    if method.as_str() != "new" || !matches!(&*recv.node, ExprNode::Const { .. }) {
        return false;
    }
    let Some(crate::ty::Ty::Class { id, .. }) = &recv.ty else {
        return false;
    };
    let class_id = params
        .unqualified_class_ids
        .get(id.0.as_str())
        .map_or(id.0.as_str(), String::as_str)
        .to_string();
    let Some(slots) = params
        .custom_new_slots
        .get(&(class_id.to_string(), method.clone()))
    else {
        return false;
    };
    if respell_constructor(args, slots).is_err() {
        refuse_constructor(
            expr,
            params,
            &class_id,
            "cannot safely move these keywords into custom `new` positional slots without changing Ruby argument binding or evaluation semantics",
            diagnostics,
        );
    }
    true
}

fn has_keyword_arguments(args: &[Expr]) -> bool {
    args.iter().any(|arg| match &*arg.node {
        ExprNode::KeywordSplat { .. } => true,
        ExprNode::Hash {
            entries,
            kwargs: true,
        } => !entries.is_empty(),
        _ => false,
    })
}

fn constructor_refusal_reason(
    params: &InstanceCallParams,
    span: crate::span::Span,
    unqualified_name: &str,
    class_name: &str,
    args: &[Expr],
) -> Option<&'static str> {
    if !has_keyword_arguments(args) {
        return None;
    }
    if params.ambiguous_names.contains(unqualified_name) && !params.model_names.contains(class_name)
    {
        return Some(
            "cannot verify constructor lookup because this unqualified class name is ambiguous",
        );
    }
    if params.lexical_refinement_calls.contains(&span) {
        return Some("a lexical refinement may replace this class's `new` method");
    }
    if params.model_names.contains(class_name)
        && !params
            .slots
            .contains_key(&(class_name.to_string(), Symbol::from("initialize")))
    {
        return None;
    }
    let has_keyword_splat = args
        .iter()
        .any(|arg| matches!(&*arg.node, ExprNode::KeywordSplat { .. }));
    if params.custom_new.contains(class_name) {
        return (has_keyword_splat
            && params
                .custom_new_slots
                .contains_key(&(class_name.to_string(), Symbol::from("new"))))
        .then_some("cannot safely rebind keyword splats to custom `new` positional slots");
    }
    if params.unknown_new.contains(class_name) {
        return Some(
            "cannot verify the effective `new` method because this class has unmodeled constructor lookup",
        );
    }
    if has_keyword_splat
        && params
            .slots
            .contains_key(&(class_name.to_string(), Symbol::from("initialize")))
    {
        return Some("cannot safely rebind keyword splats to initialize's positional slots");
    }
    None
}

/// What a refusal says when only ingest's flattening caused it ([`runs_as_written`]).
pub(crate) const AS_WRITTEN_NOTE: &str = "the CRuby tree, which writes flattened keywords back, runs the call as written";

/// Whether `d` is a constructor-keyword refusal the CRuby and JRuby trees
/// run as written ([`refuse_constructor`]'s `CONSTRUCTOR_KEYWORDS_AS_WRITTEN`):
/// an error only for the targets that keep the flattened keywords positional.
pub(crate) fn runs_as_written(d: &crate::diagnostic::Diagnostic) -> bool {
    matches!(&d.kind, crate::diagnostic::DiagnosticKind::Unsupported { construct, detail, .. }
        if construct.as_str() == crate::diagnostic::CONSTRUCTOR_KEYWORD_ARGUMENTS && detail.ends_with(AS_WRITTEN_NOTE))
}

/// The refusal holds on every target that keeps ingest's flattened
/// keywords positional. Where its only cause is that flattening, and the
/// Ruby family writes every flattened constructor keyword back, the call
/// is marked `CONSTRUCTOR_KEYWORDS_AS_WRITTEN`: the CRuby tree writes it as
/// the source did. A lexical refinement's `new` is not modeled as a method,
/// so that refusal holds on every tree.
fn refuse_constructor(
    expr: &mut Expr,
    params: &InstanceCallParams,
    class_name: &str,
    detail: &str,
    diagnostics: &mut Vec<crate::diagnostic::Diagnostic>,
) {
    let refinement = params.lexical_refinement_calls.contains(&expr.span);
    let as_written = params.ruby_family_positional_constructor.is_none()
        && !refinement
        && !params.declarations_dropped.contains(class_name);
    let detail = match &params.ruby_family_positional_constructor {
        _ if as_written => format!(
            "{detail}; {AS_WRITTEN_NOTE}"
        ),
        Some(positional) if !refinement => format!(
            "{detail}; refused on the CRuby tree too: `{positional}` keeps flattened keywords positional there"
        ),
        _ => detail.to_string(),
    };
    let diagnostic = crate::diagnostic::Diagnostic::unsupported(
        expr.span,
        None,
        crate::diagnostic::CONSTRUCTOR_KEYWORD_ARGUMENTS,
        detail,
    );
    expr.diagnostic = Some(diagnostic.kind.clone());
    if as_written {
        expr.decisions |= crate::expr::CONSTRUCTOR_KEYWORDS_AS_WRITTEN;
    }
    diagnostics.push(diagnostic);
}

/// Constructors need stricter mapping than ordinary keyword lowering:
/// preserve source evaluation order, duplicate-key evaluation, and splats;
/// refuse any shape that cannot be represented by positional arguments.
fn respell_constructor(args: &mut Vec<Expr>, slots: &[Slot]) -> Result<(), ()> {
    let Some(Expr { node, .. }) = args.last() else {
        return Ok(());
    };
    let ExprNode::Hash {
        entries,
        kwargs: true,
    } = &**node
    else {
        return Ok(());
    };
    if entries.is_empty() {
        return Ok(());
    }
    let filled = args.len() - 1;
    if args[..filled]
        .iter()
        .any(|arg| matches!(&*arg.node, ExprNode::Splat { .. }))
    {
        return Err(());
    }

    let mut supplied = Vec::with_capacity(entries.len());
    let mut positions = HashSet::default();
    let mut previous_pos = None;
    for (key, value) in entries {
        let ExprNode::Lit {
            value: Literal::Sym { value: name },
        } = &*key.node
        else {
            return Err(());
        };
        let Some(pos) = slots.iter().position(|slot| slot.name == *name) else {
            return Err(());
        };
        if !slots[pos].from_keyword
            || pos < filled
            || !positions.insert(pos)
            || previous_pos.is_some_and(|previous| pos < previous)
        {
            return Err(());
        }
        previous_pos = Some(pos);
        let mut value = value.clone();
        // The Ruby family writes the flattened keyword back, and with it
        // this argument (`lower::keyword_marks`).
        value.mark_source_keyword(slots[pos].name.clone());
        supplied.push((pos, value));
    }

    let last_pos = previous_pos.expect("non-empty entries have a positional slot");
    let mut moved = Vec::with_capacity(last_pos - filled + 1);
    let mut supplied = supplied.into_iter().peekable();
    for pos in filled..=last_pos {
        if supplied
            .peek()
            .is_some_and(|(provided, _)| *provided == pos)
        {
            moved.push(supplied.next().expect("peeked supplied value").1);
            continue;
        }
        let Some(default) = slots.get(pos).and_then(|slot| slot.default.as_ref()) else {
            return Err(());
        };
        if !is_context_free_constructor_default(default) {
            return Err(());
        }
        let mut fill = default.clone();
        fill.mark_keyword_default_fill(slots[pos].name.clone());
        moved.push(fill);
    }
    args.pop();
    args.extend(moved);
    Ok(())
}

/// A constructor default is evaluated in the class's lexical scope. Moving
/// even a constant expression to the caller can resolve a different constant.
fn is_context_free_constructor_default(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::Lit {
            value: Literal::Regex { .. },
        } => false,
        ExprNode::Lit { .. } => true,
        ExprNode::Array { elements, .. } => {
            elements.iter().all(is_context_free_constructor_default)
        }
        ExprNode::Hash { entries, .. } => entries.iter().all(|(key, value)| {
            is_context_free_constructor_default(key) && is_context_free_constructor_default(value)
        }),
        _ => false,
    }
}
