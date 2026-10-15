//! `include ActionView::Helpers::DateHelper` in an app class: dropped,
//! or refused, once every class exists.
//!
//! No target ships `ActionView::Helpers`, so the include cannot be
//! emitted; what the emitted tree answers instead is each bare call the
//! Ruby emit qualifies to `ActionView::ViewHelpers.<name>` (or a lowering
//! expands). `crate::view_helpers` holds both halves of that contract.
//! Dropping the include is sound only when every helper member the class
//! reaches through it is answered there. Before this pass it was dropped
//! unconditionally, and a call to any other member —
//! `distance_of_time_in_words_to_now`, `t`, `simple_format` — emitted
//! bare, checked clean, and raised NoMethodError when it ran.
//!
//! Who reaches the include: the class's own methods on the side it
//! lands on (instance methods for `include`; class methods for `extend`,
//! which is also what `class << self; include M` records), its
//! subclasses' methods on that side, and, for a module, the instance
//! methods of every class that includes it. A name one of those classes
//! (or a class between it and the includer) defines for itself is its
//! own method, not the helper's, and does not count.
//!
//! Library classes carry the include as an unknown class-body call
//! (`ingest::library_class` records it so); models carry it as the
//! `ModelBodyItem::Unknown` it always was. Either is removed here when
//! answered, or kept with an `Unsupported` diagnostic naming the
//! unanswered members when not.

use crate::app::App;
use crate::det::{HashMap, HashSet};
use crate::dialect::{MethodDef, MethodReceiver, ModelBodyItem};
use crate::diagnostic::DiagnosticKind;
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

use crate::view_helpers::REFUSAL_CONSTRUCT as CONSTRUCT;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Instance,
    Class,
}

/// What one class contributes: its links and, per side, the names it
/// defines and the calls its methods make on self.
#[derive(Default)]
struct Node {
    parent: Option<ClassId>,
    includes: Vec<ClassId>,
    defines: [HashSet<Symbol>; 2],
    /// `(name, written with an explicit self receiver)`.
    calls: [Vec<(Symbol, bool)>; 2],
    /// Instance methods whose body calls `super`.
    supers: HashSet<Symbol>,
    helper_module: bool,
}

fn side_index(side: Side) -> usize {
    match side {
        Side::Instance => 0,
        Side::Class => 1,
    }
}

pub(super) fn settle(app: &mut App) {
    let helper_modules: HashSet<ClassId> = app.helper_method_index.values().cloned().collect();
    let helper_names: HashSet<Symbol> = app.helper_method_index.keys().cloned().collect();
    let mut nodes: HashMap<ClassId, Node> = HashMap::default();
    for lc in &app.library_classes {
        let node = nodes.entry(lc.name.clone()).or_default();
        node.parent = lc.parent.clone();
        node.includes.extend(lc.includes.iter().cloned());
        node.helper_module = helper_modules.contains(&lc.name);
        for m in &lc.methods {
            add_method(node, m);
        }
    }
    for model in &app.models {
        let node = nodes.entry(model.name.clone()).or_default();
        node.parent = model.parent.clone();
        node.includes.extend(crate::analyze::model_includes(model));
        node.helper_module = helper_modules.contains(&model.name);
        node.defines[0].extend(model.attributes.fields.keys().cloned());
        for item in &model.body {
            match item {
                ModelBodyItem::Method { method, .. } => add_method(node, method),
                ModelBodyItem::Association { assoc, .. } => {
                    node.defines[0].insert(assoc.name().clone());
                }
                _ => {}
            }
        }
    }
    let names: HashSet<ClassId> = nodes.keys().cloned().collect();
    let resolve = |written: &ClassId, scope: &ClassId| resolve_name(written, scope, &names);
    // Parent and includer edges, resolved against the app's classes.
    let mut children: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    let mut includers: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    let mut included: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    let mut ordered: Vec<&ClassId> = nodes.keys().collect();
    ordered.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    for name in ordered {
        let node = &nodes[name];
        if let Some(parent) = node.parent.as_ref().and_then(|p| resolve(p, name)) {
            if &parent != name {
                children.entry(parent).or_default().push(name.clone());
            }
        }
        for inc in &node.includes {
            if let Some(module) = resolve(inc, name) {
                includers.entry(module.clone()).or_default().push(name.clone());
                included.entry(name.clone()).or_default().push(module);
            }
        }
    }
    let graph = Graph { nodes: &nodes, children: &children, includers: &includers, included: &included, helper_names: &helper_names };

    for lc in &mut app.library_classes {
        let owner = lc.name.clone();
        let mut kept = Vec::with_capacity(lc.unknown_calls.len());
        let mut retained = Vec::new();
        for mut call in std::mem::take(&mut lc.unknown_calls) {
            if settle_call(&mut call, &owner, &graph, &mut retained) {
                kept.push(call);
            }
        }
        lc.unknown_calls = kept;
        for module in retained.into_iter().rev() {
            if !lc.includes.contains(&module) {
                lc.includes.insert(0, module);
            }
        }
    }
    for model in &mut app.models {
        let owner = model.name.clone();
        model.body.retain_mut(|item| match item {
            ModelBodyItem::Unknown { expr, .. } => settle_call(expr, &owner, &graph, &mut Vec::new()),
            _ => true,
        });
    }
}

struct Graph<'a> {
    nodes: &'a HashMap<ClassId, Node>,
    children: &'a HashMap<ClassId, Vec<ClassId>>,
    includers: &'a HashMap<ClassId, Vec<ClassId>>,
    /// The app modules each class includes, resolved.
    included: &'a HashMap<ClassId, Vec<ClassId>>,
    helper_names: &'a HashSet<Symbol>,
}

fn add_method(node: &mut Node, m: &MethodDef) {
    let side = match m.receiver {
        MethodReceiver::Instance => 0,
        MethodReceiver::Class => 1,
    };
    node.defines[side].insert(m.name.clone());
    collect_self_calls(&m.body, &mut node.calls[side]);
    if side == 0 && calls_super(&m.body) {
        node.supers.insert(m.name.clone());
    }
}

fn calls_super(e: &Expr) -> bool {
    if matches!(&*e.node, ExprNode::Super { .. }) {
        return true;
    }
    let mut found = false;
    e.node.for_each_child(&mut |c| found = found || calls_super(c));
    found
}

fn collect_self_calls(e: &Expr, out: &mut Vec<(Symbol, bool)>) {
    if let ExprNode::Send { recv, method, .. } = &*e.node {
        match recv {
            None => out.push((method.clone(), false)),
            Some(r) if matches!(&*r.node, ExprNode::SelfRef) => out.push((method.clone(), true)),
            _ => {}
        }
    }
    e.node.for_each_child(&mut |c| collect_self_calls(c, out));
}

/// `written` as Ruby binds it from inside `scope`: the innermost
/// `Scope::…::written` the app declares, else `written` itself when the
/// app declares that.
fn resolve_name(written: &ClassId, scope: &ClassId, names: &HashSet<ClassId>) -> Option<ClassId> {
    let text = written.0.as_str();
    if let Some(top) = text.strip_prefix("::") {
        let id = ClassId(Symbol::from(top));
        return names.contains(&id).then_some(id);
    }
    let mut segs: Vec<&str> = scope.0.as_str().split("::").collect();
    while !segs.is_empty() {
        let id = ClassId(Symbol::from(format!("{}::{text}", segs.join("::"))));
        if names.contains(&id) {
            return Some(id);
        }
        segs.pop();
    }
    let id = ClassId(Symbol::from(text));
    names.contains(&id).then_some(id)
}

/// Settle one class-body call of `owner`. Returns whether it stays.
fn settle_call(call: &mut Expr, owner: &ClassId, graph: &Graph<'_>, retained: &mut Vec<ClassId>) -> bool {
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &mut *call.node else { return true };
    let side = match method.as_str() {
        "include" => Side::Instance,
        "extend" => Side::Class,
        _ => return true,
    };
    let markers: Vec<String> = args.iter().filter_map(marker_path).collect();
    if markers.is_empty() || call.diagnostic.is_some() {
        return true;
    }
    let mut refusals = Vec::new();
    for module in &markers {
        if let Some(detail) = refusal(owner, module, side, graph) {
            refusals.push(detail);
        }
    }
    if !refusals.is_empty() {
        call.diagnostic = Some(DiagnosticKind::Unsupported {
            target: None,
            construct: Symbol::from(CONSTRUCT),
            detail: refusals.join("; "),
        });
        return true;
    }
    // Answered: the include contributes nothing the emitted tree needs,
    // unless a module the owner includes reaches the helper's own method
    // with `super` (`ShopifyI18n::ActionViewSoftDegrade#translate`):
    // nothing is called on the owner for the scan above to see, yet the
    // `super` has no method to land on without the module in the chain.
    if side == Side::Instance {
        for module in &markers {
            if crate::view_helpers::runtime_module(module) && super_reaches(owner, module, graph) {
                retained.push(ClassId(Symbol::from(format!("::{module}"))));
            }
        }
    }
    args.retain(|arg| marker_path(arg).is_none());
    !args.is_empty()
}

/// Whether a module `owner` includes (or `owner` itself) has an instance
/// method named like a member of `module` that calls `super`.
fn super_reaches(owner: &ClassId, module: &str, graph: &Graph<'_>) -> bool {
    let Some(members) = crate::view_helpers::members(module) else { return false };
    let reaches = |c: &ClassId| {
        graph.nodes.get(c).is_some_and(|n| n.supers.iter().any(|name| members.contains(&name.as_str())))
    };
    reaches(owner) || graph.included.get(owner).into_iter().flatten().any(reaches)
}

fn marker_path(arg: &Expr) -> Option<String> {
    let ExprNode::Const { path } = &*arg.node else { return None };
    let segs: Vec<&str> = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
    crate::view_helpers::is_marker_include(&segs).then(|| segs.join("::"))
}

/// Why `owner`'s include (or extend) of `module` cannot be dropped, or
/// `None` when it can.
fn refusal(owner: &ClassId, module: &str, side: Side, graph: &Graph<'_>) -> Option<String> {
    let verb = match side {
        Side::Instance => "includes",
        Side::Class => "extends",
    };
    if crate::view_helpers::members(module).is_none() {
        return Some(format!(
            "{} {verb} {module}, which is not a helper module the pinned actionview defines; \
             no target ships the ActionView::Helpers namespace, so the include cannot be emitted",
            owner.0
        ));
    }
    let mut unanswered: Vec<(String, ClassId)> = Vec::new();
    for (caller, shadow) in callers(owner, side, graph) {
        let Some(node) = graph.nodes.get(&caller) else { continue };
        let calls = node.calls[side_index(side)]
            .iter()
            .filter(|(name, _)| !shadow.contains(name))
            .map(|(name, explicit)| (name.as_str(), *explicit));
        let Ok(names) = crate::view_helpers::unanswered_members(module, calls) else { continue };
        for name in names {
            if !unanswered.iter().any(|(n, _)| n == name) {
                unanswered.push((name.to_string(), caller.clone()));
            }
        }
    }
    if unanswered.is_empty() {
        return None;
    }
    let list: Vec<String> = unanswered
        .iter()
        .map(|(name, caller)| if caller == owner { format!("`{name}`") } else { format!("`{name}` (in {})", caller.0) })
        .collect();
    Some(format!(
        "{} {verb} {module} and calls {} through it, which ActionView::ViewHelpers does not port; \
         no target ships the ActionView::Helpers namespace, and without the include each call raises NoMethodError",
        owner.0,
        list.join(", ")
    ))
}

/// Every class whose `side` methods run with `owner`'s include in their
/// ancestry, each with the names that shadow the helper there: its own,
/// and those of every class between it and `owner` (inclusive).
fn callers(owner: &ClassId, side: Side, graph: &Graph<'_>) -> Vec<(ClassId, HashSet<Symbol>)> {
    let i = side_index(side);
    let own = |c: &ClassId| -> HashSet<Symbol> {
        let mut names = HashSet::default();
        if let Some(node) = graph.nodes.get(c) {
            names.extend(node.defines[i].iter().cloned());
            // A module this class includes is ahead of the helper or
            // behind it depending on include order; counted as ahead.
            // (Its instance methods; they never reach the class side.)
            if side == Side::Instance {
                for inc in graph.included.get(c).into_iter().flatten() {
                    if let Some(m) = graph.nodes.get(inc) {
                        names.extend(m.defines[0].iter().cloned());
                    }
                }
            }
            // A helper module's bare call to another app helper is
            // routed to that helper by the emit.
            if node.helper_module {
                names.extend(graph.helper_names.iter().cloned());
            }
        }
        names
    };
    let mut out = Vec::new();
    let mut seen: HashSet<ClassId> = HashSet::default();
    let mut stack: Vec<(ClassId, HashSet<Symbol>)> = vec![(owner.clone(), own(owner))];
    // A module's includers reach it on their instance side.
    if side == Side::Instance && graph.nodes.contains_key(owner) {
        let mut includer_stack: Vec<ClassId> = graph.includers.get(owner).cloned().unwrap_or_default();
        let mut seen_includers: HashSet<ClassId> = HashSet::default();
        while let Some(inc) = includer_stack.pop() {
            if !seen_includers.insert(inc.clone()) {
                continue;
            }
            let mut shadow = own(&inc);
            shadow.extend(own(owner));
            stack.push((inc.clone(), shadow));
            includer_stack.extend(graph.includers.get(&inc).cloned().unwrap_or_default());
        }
    }
    while let Some((class, shadow)) = stack.pop() {
        if !seen.insert(class.clone()) {
            continue;
        }
        for child in graph.children.get(&class).into_iter().flatten() {
            let mut child_shadow = own(child);
            child_shadow.extend(shadow.iter().cloned());
            stack.push((child.clone(), child_shadow));
        }
        out.push((class, shadow));
    }
    out.sort_by(|a, b| a.0 .0.as_str().cmp(b.0 .0.as_str()));
    out
}
