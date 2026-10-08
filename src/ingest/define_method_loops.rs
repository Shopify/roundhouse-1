//! Methods a class body defines with `define_method` over names it knows
//! statically, expanded into the methods Ruby defines.
//!
//! Three shapes from Core, each a class-body statement:
//!
//! ```ruby
//! # FeatureSet::Core: a frozen literal constant, iterated in instance_eval
//! instance_eval do
//!   FEATURES.each do |name, type|
//!     define_method(name) { self[name] }
//!     define_method("#{name}=") { |value| self[name] = value }
//!     next unless type == :boolean
//!     define_method("#{name}?") { !!self[name] }
//!   end
//! end
//!
//! # SessionAccessorConcern (class_methods), called with literal symbols
//! def session_accessor(*names)
//!   Array.wrap(names).each do |name|
//!     define_method("#{name}=") { |value| @session[key(name)] = value }
//!     define_method(name.to_s) { @session[key(name)] }
//!   end
//! end
//!
//! # ApiClient::Identification (extended), called with a literal symbol
//! def define_named_app(defined_name)
//!   @named_api_client_ids ||= []
//!   identified_id = const_get("#{defined_name.upcase}_ID")
//!   @named_api_client_ids << identified_id
//!   define_method("#{defined_name}?") { id == identified_id }
//!   define_singleton_method(defined_name) { ApiClient.find_by(id: identified_id) }
//! end
//! ```
//!
//! Nothing in the source writes `def storefront` or `ApiClient.pos`, so
//! every call to one resolved to no method, and the emitted class lacked
//! it: a library class drops the class-body statement, a model drops the
//! macro call.
//!
//! The statement is evaluated here, at ingest, by a small partial
//! evaluator over the source: the collection a loop walks must be a
//! literal (array, hash, a frozen literal constant of the lexical scope,
//! `Array.wrap` of one, or a macro's literal arguments); locals must hold
//! literals, or a `const_get` of a constant the class body writes, before
//! the call, as an Integer, Symbol, nil or boolean literal; a
//! `next if`/`next unless` and an `if` must decide statically. Each
//! `define_method(name) { |params| body }` becomes the `def` Ruby makes:
//! the evaluated name, the block's required parameters as the method's
//! (define_method's arity is the lambda's), public visibility (what a
//! macro frame or an `instance_eval` block gives define_method; the
//! `initialize` family stays private, as Ruby makes it), and the block's
//! body with each captured local replaced by its literal (a Symbol,
//! Integer, nil or boolean: the same object the closure held; a captured
//! String, Array or Hash is refused, since a literal would be a new
//! object). `define_singleton_method` gives a class method. A model
//! macro's other class-body effects (`@ids ||= []`, `@ids << id`) stay
//! as class-body statements with the same substitution.
//!
//! Anything else stays a gap, explicitly: the statement keeps an
//! `Unsupported` diagnostic (construct `define_method_expansion`) saying
//! why. In particular: a computed collection or name, a non-literal
//! local, a body that writes or shadows a captured local or reads one
//! the evaluator does not know, `super` without arguments, `yield`,
//! `block_given?`, `binding`, a constant whose lexical binding differs
//! between the macro's module and the class, parameters other than
//! required positionals, a name the class (or another expansion)
//! already defines, and a library class's non-definition side effects.
//! The diagnostic is reported where the emitted class drops the
//! statement; a library class that replays its body (a gem's base) or a
//! macro of an `extend`ed module (the emit runs those natively) keeps
//! the statement as before, unreported.

use crate::App;
use crate::det::{HashMap, HashSet};
use crate::diagnostic::DiagnosticKind;
use crate::dialect::{MethodDef, MethodReceiver, MethodVisibility, ModelBodyItem, Param};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::{SourceFile, Span};

use super::util::{class_name_path, constant_id_str, module_name_path};

pub(crate) const REFUSAL_CONSTRUCT: &str = "define_method_expansion";

/// Blocks whose `define_method` calls define on the receiver as a class
/// body would, public whatever the surrounding default visibility.
const EVAL_BLOCKS: &[&str] = &[
    "instance_eval",
    "instance_exec",
    "class_eval",
    "class_exec",
    "module_eval",
    "module_exec",
];

#[derive(Clone, Copy)]
enum Owner {
    Library(usize),
    Model(usize),
}

struct Expansion {
    methods: Vec<MethodDef>,
    residual: Vec<Expr>,
}

pub(super) fn expand(app: &mut App, sources: &[SourceFile]) {
    let constants = app_constants(app);
    let mut library: Vec<(usize, usize, Result<Expansion, String>)> = Vec::new();
    // Calls the emitted class replays when they stay: a library class
    // runs a macro of a module it `extend`s natively
    // (`emit::ruby::library`'s `app_extension_calls`).
    let mut replayed: HashSet<(usize, usize)> = HashSet::default();
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.origin.is_some() {
            continue;
        }
        for (j, call) in lc.unknown_calls.iter().enumerate() {
            if let Some((outcome, extended)) =
                plan(app, sources, &constants, Owner::Library(i), call)
            {
                if extended {
                    replayed.insert((i, j));
                }
                library.push((i, j, outcome));
            }
        }
    }
    let mut models: Vec<(usize, usize, Result<Expansion, String>)> = Vec::new();
    for (i, model) in app.models.iter().enumerate() {
        for (j, item) in model.body.iter().enumerate() {
            let ModelBodyItem::Unknown { expr, .. } = item else {
                continue;
            };
            if let Some((outcome, _)) = plan(app, sources, &constants, Owner::Model(i), expr) {
                models.push((i, j, outcome));
            }
        }
    }
    settle_collisions(app, &mut library, Owner::Library);
    settle_collisions(app, &mut models, Owner::Model);

    let dropped: Vec<bool> = (0..app.library_classes.len())
        .map(|i| body_dropped(app, i))
        .collect();
    // Apply in reverse index order so earlier indices stay valid.
    library.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
    for (i, j, outcome) in library {
        let lc = &mut app.library_classes[i];
        match outcome {
            Ok(expansion) => {
                lc.unknown_calls.remove(j);
                lc.methods.extend(expansion.methods);
            }
            Err(reason) => {
                if dropped[i] && !replayed.contains(&(i, j)) {
                    refuse(&mut lc.unknown_calls[j], reason);
                }
            }
        }
    }
    models.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
    for (i, j, outcome) in models {
        let model = &mut app.models[i];
        match outcome {
            Ok(expansion) => {
                let ModelBodyItem::Unknown {
                    mut leading_comments,
                    mut leading_blank_line,
                    ..
                } = model.body.remove(j)
                else {
                    unreachable!("planned from an Unknown item")
                };
                let mut items = Vec::new();
                for method in expansion.methods {
                    items.push(ModelBodyItem::Method {
                        method,
                        leading_comments: std::mem::take(&mut leading_comments),
                        leading_blank_line: std::mem::take(&mut leading_blank_line),
                    });
                }
                for expr in expansion.residual {
                    items.push(ModelBodyItem::ClassBody {
                        expr,
                        leading_comments: std::mem::take(&mut leading_comments),
                        leading_blank_line: std::mem::take(&mut leading_blank_line),
                    });
                }
                model.body.splice(j..j, items);
            }
            Err(reason) => {
                if let ModelBodyItem::Unknown { expr, .. } = &mut model.body[j] {
                    refuse(expr, reason);
                }
            }
        }
    }
}

/// Whether the emitted library class drops a class-body statement it
/// does not model: its superclass chain ends in no class or in an app
/// class or model, and (a module) it includes only app modules. Any
/// other class replays its class body verbatim when the emitted file
/// loads (the gem's class is its receiver), so the statement runs there
/// and an unexpanded one is no gap to report.
fn body_dropped(app: &App, i: usize) -> bool {
    let app_class = |name: &str| {
        let name = name.trim_start_matches("::");
        app.models.iter().any(|m| m.name.0.as_str() == name)
            || app
                .library_classes
                .iter()
                .any(|c| c.origin.is_none() && c.name.0.as_str().trim_start_matches("::") == name)
    };
    let lc = &app.library_classes[i];
    if lc.is_module {
        return lc.includes.iter().all(|inc| app_class(inc.0.as_str()));
    }
    let mut seen: Vec<&str> = vec![lc.name.0.as_str()];
    let mut parent = lc.parent.as_ref();
    while let Some(p) = parent {
        if !app_class(p.0.as_str()) {
            return false;
        }
        if app.models.iter().any(|m| m.name == *p) || seen.contains(&p.0.as_str()) {
            return true;
        }
        seen.push(p.0.as_str());
        parent = app
            .library_classes
            .iter()
            .filter(|c| !c.is_module && c.name == *p)
            .find_map(|c| c.parent.as_ref());
    }
    true
}

fn refuse(call: &mut Expr, reason: String) {
    call.diagnostic.get_or_insert(DiagnosticKind::Unsupported {
        target: None,
        construct: Symbol::from(REFUSAL_CONSTRUCT),
        detail: reason,
    });
}

/// Ruby keeps the last definition of a name; an expansion that would
/// redefine one the class has (or another expansion makes) is refused,
/// both sides, rather than guessing which one the emitted class keeps.
fn settle_collisions(
    app: &App,
    planned: &mut [(usize, usize, Result<Expansion, String>)],
    owner: fn(usize) -> Owner,
) {
    // Keyed by the class's name: a reopened class is several entries.
    let mut counts: HashMap<(String, Symbol, bool), usize> = HashMap::default();
    for (i, _, outcome) in planned.iter() {
        if let Ok(expansion) = outcome {
            let class = owner_name(app, owner(*i)).0.to_string();
            for m in &expansion.methods {
                *counts
                    .entry((
                        class.clone(),
                        m.name.clone(),
                        m.receiver == MethodReceiver::Class,
                    ))
                    .or_default() += 1;
            }
        }
    }
    for (i, _, outcome) in planned.iter_mut() {
        let Ok(expansion) = outcome else { continue };
        let taken = existing_names(app, owner(*i));
        let name = owner_name(app, owner(*i)).0.to_string();
        let clash = expansion.methods.iter().find(|m| {
            let class = m.receiver == MethodReceiver::Class;
            counts[&(name.clone(), m.name.clone(), class)] > 1
                || taken.contains(&(m.name.clone(), class))
        });
        if let Some(m) = clash {
            let side = if m.receiver == MethodReceiver::Class {
                "class method"
            } else {
                "method"
            };
            *outcome = Err(format!(
                "it defines the {side} `{}`, which the class also defines elsewhere; Ruby keeps whichever runs last",
                m.name
            ));
        }
    }
}

fn existing_names(app: &App, owner: Owner) -> HashSet<(Symbol, bool)> {
    let mut out = HashSet::default();
    match owner {
        Owner::Library(i) => {
            let name = &app.library_classes[i].name;
            for lc in app.library_classes.iter().filter(|c| &c.name == name) {
                for m in &lc.methods {
                    out.insert((m.name.clone(), m.receiver == MethodReceiver::Class));
                }
            }
        }
        Owner::Model(i) => {
            let model = &app.models[i];
            for m in model.methods() {
                out.insert((m.name.clone(), m.receiver == MethodReceiver::Class));
            }
            for item in &model.body {
                match item {
                    ModelBodyItem::Association { assoc, .. } => {
                        out.insert((assoc.name().clone(), false));
                    }
                    ModelBodyItem::Scope { scope, .. } => {
                        out.insert((scope.name.clone(), true));
                    }
                    _ => {}
                }
            }
            for field in model.attributes.fields.keys() {
                out.insert((field.clone(), false));
                out.insert((Symbol::from(format!("{field}=")), false));
                out.insert((Symbol::from(format!("{field}?")), false));
            }
        }
    }
    out
}

/// Every constant the app declares, fully qualified: classes, modules,
/// models and the constants their bodies write.
fn app_constants(app: &App) -> HashSet<String> {
    let mut out = HashSet::default();
    for lc in &app.library_classes {
        out.insert(lc.name.0.as_str().trim_start_matches("::").to_string());
        for (name, _) in &lc.constants {
            out.insert(format!(
                "{}::{name}",
                lc.name.0.as_str().trim_start_matches("::")
            ));
        }
    }
    for model in &app.models {
        out.insert(model.name.0.as_str().to_string());
        for item in &model.body {
            if let ModelBodyItem::Unknown { expr, .. } = item
                && let ExprNode::Assign {
                    target: LValue::Const { path },
                    ..
                } = &*expr.node
                && let [name] = path.as_slice()
            {
                out.insert(format!("{}::{name}", model.name.0.as_str()));
            }
        }
    }
    out
}

fn owner_name(app: &App, owner: Owner) -> &ClassId {
    match owner {
        Owner::Library(i) => &app.library_classes[i].name,
        Owner::Model(i) => &app.models[i].name,
    }
}

fn source_of<'a>(sources: &'a [SourceFile], span: Span) -> Option<&'a SourceFile> {
    span.file
        .0
        .checked_sub(1)
        .and_then(|i| sources.get(i as usize))
}

/// `None` when `call` is not a define_method statement at all; otherwise
/// the expansion, or why there is none, and whether the call reaches
/// its macro through a module the class `extend`s.
fn plan(
    app: &App,
    sources: &[SourceFile],
    constants: &HashSet<String>,
    owner: Owner,
    call: &Expr,
) -> Option<(Result<Expansion, String>, bool)> {
    let ExprNode::Send {
        recv,
        method,
        args,
        block,
        ..
    } = &*call.node
    else {
        return None;
    };
    if recv
        .as_ref()
        .is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef))
    {
        return None;
    }
    if EVAL_BLOCKS.contains(&method.as_str()) {
        let block = block.as_ref()?;
        if !args.is_empty() || !defines(block) {
            return None;
        }
        return Some((plan_eval(app, sources, constants, owner, call), false));
    }
    if block.is_some() {
        return None;
    }
    if app
        .class_eval_macro_expansions
        .get(owner_name(app, owner))
        .is_some_and(|expansions| expansions.iter().any(|e| e.call == call.span))
    {
        return None;
    }
    let (def, via) = macro_for(app, owner, method, call.span)?;
    if !defines(&def.body) {
        return None;
    }
    let extended = via != owner_name(app, owner).0.as_str();
    Some((
        plan_macro(app, sources, constants, owner, call, def, via),
        extended,
    ))
}

/// Does `expr` contain a `define_method` / `define_singleton_method`?
fn defines(expr: &Expr) -> bool {
    if let ExprNode::Send { method, .. } = &*expr.node
        && matches!(method.as_str(), "define_method" | "define_singleton_method")
    {
        return true;
    }
    let mut found = false;
    expr.node
        .for_each_child(&mut |child| found = found || defines(child));
    found
}

/// The app macro a bare class-body call `name` reaches, and where it
/// lives: the class's own class-side method (a concern's `class_methods`
/// arrive spliced as such), or an instance method of a module the class
/// `extend`s before the call. A model's concern macros are
/// `model_macros`' to expand. A `def self.x` written after the call in
/// the same file is not there yet when the class body calls it.
fn macro_for<'a>(
    app: &'a App,
    owner: Owner,
    name: &Symbol,
    at: Span,
) -> Option<(&'a MethodDef, String)> {
    let before = |span: Span| span.file == at.file && span.end <= at.start;
    let (scope, extends): (&ClassId, Vec<&Expr>) = match owner {
        Owner::Library(i) => {
            let lc = &app.library_classes[i];
            let own: Vec<&MethodDef> = lc
                .methods
                .iter()
                .filter(|m| m.receiver == MethodReceiver::Class && &m.name == name)
                .collect();
            match own.as_slice() {
                [def] if def.name_span.file == at.file && !before(def.name_span) => return None,
                [def] => return Some((def, lc.name.0.to_string())),
                [] => {}
                _ => return None,
            }
            (
                &lc.name,
                lc.unknown_calls.iter().filter(|c| before(c.span)).collect(),
            )
        }
        Owner::Model(i) => {
            let model = &app.models[i];
            if model
                .methods()
                .any(|m| m.receiver == MethodReceiver::Class && &m.name == name)
            {
                return None;
            }
            let extends = model
                .body
                .iter()
                .filter_map(|item| match item {
                    ModelBodyItem::Unknown { expr, .. } if before(expr.span) => Some(expr),
                    _ => None,
                })
                .collect();
            (&model.name, extends)
        }
    };
    // Ruby looks in the most recent `extend` first.
    for call in extends.iter().rev() {
        let ExprNode::Send {
            recv: None,
            method,
            args,
            ..
        } = &*call.node
        else {
            continue;
        };
        if method.as_str() != "extend" {
            continue;
        }
        for arg in args.iter().rev() {
            let ExprNode::Const { path } = &*arg.node else {
                continue;
            };
            let Some(module) = resolve_module(app, scope, path) else {
                continue;
            };
            if let Some(def) = module_method(app, module, name, &mut Vec::new()) {
                return Some((def, module.name.0.to_string()));
            }
        }
    }
    None
}

fn resolve_module<'a>(
    app: &'a App,
    scope: &ClassId,
    path: &[Symbol],
) -> Option<&'a crate::dialect::LibraryClass> {
    let written = path
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("::");
    let written = written.trim_start_matches("::");
    let segments: Vec<&str> = scope.0.as_str().split("::").collect();
    (0..=segments.len()).rev().find_map(|n| {
        let name = if n == 0 {
            written.to_string()
        } else {
            format!("{}::{written}", segments[..n].join("::"))
        };
        app.library_classes
            .iter()
            .find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == name)
    })
}

fn module_method<'a>(
    app: &'a App,
    module: &'a crate::dialect::LibraryClass,
    name: &Symbol,
    seen: &mut Vec<&'a ClassId>,
) -> Option<&'a MethodDef> {
    if seen.contains(&&module.name) {
        return None;
    }
    seen.push(&module.name);
    let own: Vec<&MethodDef> = module
        .methods
        .iter()
        .filter(|m| m.receiver == MethodReceiver::Instance && &m.name == name)
        .collect();
    if let [def] = own.as_slice() {
        return Some(def);
    }
    module.includes.iter().rev().find_map(|inc| {
        let path: Vec<Symbol> = inc.0.as_str().split("::").map(Symbol::from).collect();
        resolve_module(app, &module.name, &path)
            .and_then(|inner| module_method(app, inner, name, seen))
    })
}

// ---- Locating source --------------------------------------------------

/// A node found by its location, with the lexical nesting around it
/// (outermost first, each entry the full name of a class/module body)
/// and the location of the innermost class/module node holding it.
struct Located {
    nesting: Vec<String>,
    container: Option<(usize, usize)>,
}

struct Locate {
    start: usize,
    end: usize,
    def: bool,
    stack: Vec<(String, (usize, usize))>,
    found: Option<Located>,
}

impl Locate {
    fn push(&mut self, written: Option<Vec<String>>, loc: (usize, usize)) {
        let written = written.unwrap_or_default().join("::");
        let full = match self.stack.last() {
            Some((outer, _)) if !outer.is_empty() => format!("{outer}::{written}"),
            _ => written,
        };
        self.stack.push((full, loc));
    }
    fn hit(&mut self) {
        self.found = Some(Located {
            nesting: self.stack.iter().map(|(name, _)| name.clone()).collect(),
            container: self.stack.last().map(|(_, loc)| *loc),
        });
    }
}

impl<'pr> ruby_prism::Visit<'pr> for Locate {
    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        let loc = node.location();
        self.push(
            class_name_path(node),
            (loc.start_offset(), loc.end_offset()),
        );
        ruby_prism::visit_class_node(self, node);
        self.stack.pop();
    }
    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        let loc = node.location();
        self.push(
            module_name_path(node),
            (loc.start_offset(), loc.end_offset()),
        );
        ruby_prism::visit_module_node(self, node);
        self.stack.pop();
    }
    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        // `class << self`: a scope no app constant is declared in.
        let loc = node.location();
        let outer = self
            .stack
            .last()
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        self.stack.push((
            format!("{outer}::<singleton>"),
            (loc.start_offset(), loc.end_offset()),
        ));
        ruby_prism::visit_singleton_class_node(self, node);
        self.stack.pop();
    }
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let loc = node.location();
        if !self.def
            && self.found.is_none()
            && loc.start_offset() == self.start
            && loc.end_offset() == self.end
        {
            self.hit();
            return;
        }
        ruby_prism::visit_call_node(self, node);
    }
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if self.def && self.found.is_none() && node.name_loc().start_offset() == self.start {
            self.hit();
            return;
        }
        ruby_prism::visit_def_node(self, node);
    }
}

fn locate(root: &ruby_prism::Node<'_>, start: usize, end: usize, def: bool) -> Option<Located> {
    let mut locate = Locate {
        start,
        end,
        def,
        stack: Vec::new(),
        found: None,
    };
    ruby_prism::Visit::visit(&mut locate, root);
    locate.found
}

/// The first node under `root` at exactly `start..end` satisfying `pick`.
fn node_at<'pr>(
    root: &ruby_prism::Node<'pr>,
    start: usize,
    end: usize,
    pick: fn(&ruby_prism::Node<'pr>) -> bool,
) -> Option<ruby_prism::Node<'pr>> {
    struct Find<'pr> {
        start: usize,
        end: usize,
        pick: fn(&ruby_prism::Node<'pr>) -> bool,
        found: Option<ruby_prism::Node<'pr>>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Find<'pr> {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(node);
        }
        fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(node);
        }
    }
    impl<'pr> Find<'pr> {
        fn check(&mut self, node: ruby_prism::Node<'pr>) {
            let loc = node.location();
            if self.found.is_none()
                && loc.start_offset() == self.start
                && loc.end_offset() == self.end
                && (self.pick)(&node)
            {
                self.found = Some(node);
            }
        }
    }
    let mut find = Find {
        start,
        end,
        pick,
        found: None,
    };
    ruby_prism::Visit::visit(&mut find, root);
    find.found
}

/// The IR node a source location was ingested to, anywhere under `root`.
fn ir_at<'a>(root: &'a Expr, start: usize, end: usize) -> Option<&'a Expr> {
    if root.span.start as usize == start && root.span.end as usize == end {
        return Some(root);
    }
    let mut found = None;
    root.node.for_each_child(&mut |child| {
        if found.is_none() {
            found = ir_at(child, start, end);
        }
    });
    found
}

// ---- Planning ---------------------------------------------------------

struct Scope<'a> {
    app: &'a App,
    constants: &'a HashSet<String>,
    owner: Owner,
    /// Where the statements being evaluated were written.
    source: &'a SourceFile,
    nesting: Vec<String>,
    /// The class body holding the call site, and where in it the call is.
    site_source: &'a SourceFile,
    site_container: Option<(usize, usize)>,
    site_offset: usize,
    site_nesting: Vec<String>,
    /// The IR of the evaluated statements (the call, or the macro body).
    ir: &'a Expr,
    out: Expansion,
}

fn plan_eval(
    app: &App,
    sources: &[SourceFile],
    constants: &HashSet<String>,
    owner: Owner,
    call: &Expr,
) -> Result<Expansion, String> {
    let method = match &*call.node {
        ExprNode::Send { method, .. } => method.clone(),
        _ => unreachable!(),
    };
    let source = source_of(sources, call.span).ok_or("its source is not available")?;
    let parsed = ruby_prism::parse(source.text.as_bytes());
    let root = parsed.node();
    let (start, end) = (call.span.start as usize, call.span.end as usize);
    let located = locate(&root, start, end, false).ok_or("its source could not be located")?;
    let node = node_at(&root, start, end, |n| n.as_call_node().is_some())
        .ok_or("its source could not be located")?;
    let node = node.as_call_node().unwrap();
    let block = node
        .block()
        .and_then(|b| b.as_block_node())
        .ok_or(format!("`{method}` takes no literal block"))?;
    if block.parameters().is_some() {
        return Err(format!("the `{method}` block takes parameters"));
    }
    let mut scope = Scope {
        app,
        constants,
        owner,
        source,
        nesting: located.nesting.clone(),
        site_source: source,
        site_container: located.container,
        site_offset: start,
        site_nesting: located.nesting,
        ir: call,
        out: Expansion {
            methods: Vec::new(),
            residual: Vec::new(),
        },
    };
    let statements: Vec<ruby_prism::Node<'_>> = block
        .body()
        .map(super::util::flatten_statements)
        .unwrap_or_default();
    let mut env = Env::default();
    match scope.statements(&statements, &mut env, false)? {
        Flow::Normal => {}
        Flow::Next => return Err(format!("`next` leaves the `{method}` block")),
    }
    finish(scope)
}

fn plan_macro(
    app: &App,
    sources: &[SourceFile],
    constants: &HashSet<String>,
    owner: Owner,
    call: &Expr,
    def: &MethodDef,
    via: String,
) -> Result<Expansion, String> {
    let method = match &*call.node {
        ExprNode::Send { method, .. } => method.clone(),
        _ => unreachable!(),
    };
    let site_source = source_of(sources, call.span).ok_or("its source is not available")?;
    let site_parsed = ruby_prism::parse(site_source.text.as_bytes());
    let site_root = site_parsed.node();
    let (start, end) = (call.span.start as usize, call.span.end as usize);
    let site = locate(&site_root, start, end, false).ok_or("its source could not be located")?;
    let site_node = node_at(&site_root, start, end, |n| n.as_call_node().is_some())
        .and_then(|n| n.as_call_node())
        .ok_or("its source could not be located")?;

    let source = source_of(sources, def.name_span)
        .ok_or_else(|| format!("the source of `{via}#{method}` is not available"))?;
    let parsed = ruby_prism::parse(source.text.as_bytes());
    let root = parsed.node();
    let at = def.name_span.start as usize;
    let located = locate(&root, at, at, true)
        .ok_or_else(|| format!("the source of `{via}#{method}` could not be located"))?;
    let def_node = find_def(&root, at)
        .and_then(|n| n.as_def_node())
        .ok_or_else(|| format!("the source of `{via}#{method}` could not be located"))?;

    let mut scope = Scope {
        app,
        constants,
        owner,
        source,
        nesting: located.nesting,
        site_source,
        site_container: site.container,
        site_offset: start,
        site_nesting: site.nesting,
        ir: &def.body,
        out: Expansion {
            methods: Vec::new(),
            residual: Vec::new(),
        },
    };
    let mut env = scope.bind(&def_node, &site_node, &method)?;
    let statements: Vec<ruby_prism::Node<'_>> = def_node
        .body()
        .map(super::util::flatten_statements)
        .unwrap_or_default();
    match scope.statements(&statements, &mut env, false)? {
        Flow::Normal => {}
        Flow::Next => return Err(format!("`next` leaves `{method}`")),
    }
    finish(scope).map_err(|e| format!("`{method}` (`{via}#{method}`): {e}"))
}

fn find_def<'pr>(root: &ruby_prism::Node<'pr>, name_start: usize) -> Option<ruby_prism::Node<'pr>> {
    struct Find<'pr> {
        at: usize,
        found: Option<ruby_prism::Node<'pr>>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Find<'pr> {
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            if self.found.is_none() && node.name_loc().start_offset() == self.at {
                self.found = Some(node.as_node());
                return;
            }
            ruby_prism::visit_def_node(self, node);
        }
    }
    let mut find = Find {
        at: name_start,
        found: None,
    };
    ruby_prism::Visit::visit(&mut find, root);
    find.found
}

fn finish(scope: Scope<'_>) -> Result<Expansion, String> {
    if scope.out.methods.is_empty() {
        return Err("it defines no method".into());
    }
    Ok(scope.out)
}

// ---- Values -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Nil,
    Bool(bool),
    Int(i64),
    Sym(Symbol),
    Str(String),
    Array(Vec<Val>),
    Hash(Vec<(Val, Val)>),
}

impl Val {
    fn truthy(&self) -> Result<bool, String> {
        Ok(!matches!(self, Val::Nil | Val::Bool(false)))
    }

    fn to_s(&self) -> Result<String, String> {
        match self {
            Val::Nil => Ok(String::new()),
            Val::Bool(b) => Ok(b.to_string()),
            Val::Int(i) => Ok(i.to_string()),
            Val::Sym(s) => Ok(s.as_str().to_string()),
            Val::Str(s) => Ok(s.clone()),
            other => Err(format!(
                "{} is not interpolated statically",
                other.describe()
            )),
        }
    }

    fn describe(&self) -> String {
        match self {
            Val::Array(_) => "an array".into(),
            Val::Hash(_) => "a hash".into(),
            other => format!("{other:?}"),
        }
    }

    /// The literal a method body may read in place of the captured local:
    /// only immediates (the same object either way) and constants.
    fn substitute(&self, span: Span) -> Option<Expr> {
        let node = match self {
            Val::Nil => ExprNode::Lit {
                value: Literal::Nil,
            },
            Val::Bool(value) => ExprNode::Lit {
                value: Literal::Bool { value: *value },
            },
            Val::Int(value) => ExprNode::Lit {
                value: Literal::Int { value: *value },
            },
            Val::Sym(value) => ExprNode::Lit {
                value: Literal::Sym {
                    value: value.clone(),
                },
            },
            Val::Str(_) | Val::Array(_) | Val::Hash(_) => return None,
        };
        Some(Expr::new(span, node))
    }
}

#[derive(Clone, Default)]
struct Env {
    locals: Vec<(Symbol, Val)>,
}

impl Env {
    fn get(&self, name: &str) -> Option<&Val> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, v)| v)
    }
    fn set(&mut self, name: Symbol, value: Val) {
        if let Some(slot) = self.locals.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            self.locals.push((name, value));
        }
    }
}

enum Flow {
    Normal,
    Next,
}

fn text(source: &SourceFile, node: &ruby_prism::Node<'_>) -> String {
    let loc = node.location();
    source
        .text
        .get(loc.start_offset()..loc.end_offset())
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn utf8(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| "a non-UTF-8 literal".to_string())
}

impl<'a> Scope<'a> {
    /// The macro's parameters bound to the call's literal arguments.
    fn bind(
        &self,
        def: &ruby_prism::DefNode<'_>,
        call: &ruby_prism::CallNode<'_>,
        method: &Symbol,
    ) -> Result<Env, String> {
        if call.block().is_some() {
            return Err(format!("`{method}` is called with a block"));
        }
        let mut args = Vec::new();
        if let Some(list) = call.arguments() {
            for arg in list.arguments().iter() {
                if arg.as_keyword_hash_node().is_some() || arg.as_splat_node().is_some() {
                    return Err(format!(
                        "`{method}` is called with splatted or keyword arguments"
                    ));
                }
                args.push(
                    self.literal(&arg, &Env::default())
                        .map_err(|e| format!("an argument of `{method}`: {e}"))?,
                );
            }
        }
        let mut env = Env::default();
        let mut args = args.into_iter();
        if let Some(params) = def.parameters() {
            if params.optionals().iter().next().is_some()
                || params.posts().iter().next().is_some()
                || params.keywords().iter().next().is_some()
                || params.keyword_rest().is_some()
                || params.block().is_some()
            {
                return Err(format!(
                    "`{method}` takes parameters other than required positionals and a rest"
                ));
            }
            for param in params.requireds().iter() {
                let param = param
                    .as_required_parameter_node()
                    .ok_or(format!("`{method}` destructures a parameter"))?;
                let value = args
                    .next()
                    .ok_or(format!("`{method}` is called with too few arguments"))?;
                env.set(Symbol::from(constant_id_str(&param.name())), value);
            }
            if let Some(rest) = params.rest() {
                let name = rest
                    .as_rest_parameter_node()
                    .and_then(|r| r.name())
                    .ok_or(format!("`{method}` takes an anonymous rest"))?;
                env.set(
                    Symbol::from(constant_id_str(&name)),
                    Val::Array(args.by_ref().collect()),
                );
            }
        }
        if args.next().is_some() {
            return Err(format!("`{method}` is called with too many arguments"));
        }
        Ok(env)
    }

    fn statements(
        &mut self,
        nodes: &[ruby_prism::Node<'_>],
        env: &mut Env,
        in_loop: bool,
    ) -> Result<Flow, String> {
        for node in nodes {
            if let Flow::Next = self.statement(node, env, in_loop)? {
                return Ok(Flow::Next);
            }
        }
        Ok(Flow::Normal)
    }

    fn statement(
        &mut self,
        node: &ruby_prism::Node<'_>,
        env: &mut Env,
        in_loop: bool,
    ) -> Result<Flow, String> {
        if let Some(call) = node.as_call_node() {
            let name = constant_id_str(&call.name());
            let self_recv = call.receiver().is_none_or(|r| r.as_self_node().is_some());
            match name {
                "define_method" | "define_singleton_method" if self_recv => {
                    self.define(&call, env, name == "define_singleton_method")?;
                    return Ok(Flow::Normal);
                }
                "each" | "each_pair" if call.receiver().is_some() && call.arguments().is_none() => {
                    self.each(&call, env)?;
                    return Ok(Flow::Normal);
                }
                "<<" if call
                    .receiver()
                    .is_some_and(|r| r.as_instance_variable_read_node().is_some()) =>
                {
                    self.residual(node, env)?;
                    return Ok(Flow::Normal);
                }
                _ => {
                    return Err(format!(
                        "`{}` is not evaluated statically",
                        text(self.source, node)
                    ));
                }
            }
        }
        if let Some(write) = node.as_local_variable_write_node() {
            let value = self.literal(&write.value(), env)?;
            let name = Symbol::from(constant_id_str(&write.name()));
            if in_loop && write.depth() > 0 {
                return Err(format!("the loop body writes the outer local `{name}`"));
            }
            env.set(name, value);
            return Ok(Flow::Normal);
        }
        if node.as_instance_variable_or_write_node().is_some()
            || node.as_instance_variable_write_node().is_some()
        {
            self.residual(node, env)?;
            return Ok(Flow::Normal);
        }
        if let Some(branch) = node.as_if_node() {
            let taken = self.literal(&branch.predicate(), env)?.truthy()?;
            let chosen: Vec<ruby_prism::Node<'_>> = if taken {
                branch
                    .statements()
                    .map(|s| s.body().iter().collect())
                    .unwrap_or_default()
            } else {
                match branch.subsequent() {
                    None => Vec::new(),
                    Some(other) => match other.as_else_node() {
                        Some(e) => e
                            .statements()
                            .map(|s| s.body().iter().collect())
                            .unwrap_or_default(),
                        None => vec![other],
                    },
                }
            };
            return self.statements(&chosen, env, in_loop);
        }
        if let Some(branch) = node.as_unless_node() {
            let taken = !self.literal(&branch.predicate(), env)?.truthy()?;
            let chosen: Vec<ruby_prism::Node<'_>> = if taken {
                branch
                    .statements()
                    .map(|s| s.body().iter().collect())
                    .unwrap_or_default()
            } else {
                branch
                    .else_clause()
                    .and_then(|e| e.statements())
                    .map(|s| s.body().iter().collect())
                    .unwrap_or_default()
            };
            return self.statements(&chosen, env, in_loop);
        }
        if let Some(next) = node.as_next_node() {
            if !in_loop || next.arguments().is_some() {
                return Err("a `next` outside a loop, or with a value".into());
            }
            return Ok(Flow::Next);
        }
        Err(format!(
            "`{}` is not evaluated statically",
            text(self.source, node)
        ))
    }

    fn each(&mut self, call: &ruby_prism::CallNode<'_>, env: &mut Env) -> Result<(), String> {
        let collection = self.literal(&call.receiver().unwrap(), env)?;
        let block = call
            .block()
            .and_then(|b| b.as_block_node())
            .ok_or("an `each` without a literal block")?;
        let params = required_block_params(&block)?;
        let items: Vec<Vec<Val>> = match collection {
            Val::Array(items) => items
                .into_iter()
                .map(|item| match (item, params.len()) {
                    (Val::Array(parts), n) if n > 1 => parts,
                    (item, _) => vec![item],
                })
                .collect(),
            Val::Hash(pairs) => pairs
                .into_iter()
                .map(|(k, v)| {
                    if params.len() > 1 {
                        vec![k, v]
                    } else {
                        vec![Val::Array(vec![k, v])]
                    }
                })
                .collect(),
            other => return Err(format!("`each` over {}", other.describe())),
        };
        let statements: Vec<ruby_prism::Node<'_>> = block
            .body()
            .map(super::util::flatten_statements)
            .unwrap_or_default();
        for item in items {
            let mut inner = env.clone();
            let mut values = item.into_iter();
            for param in &params {
                inner
                    .locals
                    .push((param.clone(), values.next().unwrap_or(Val::Nil)));
            }
            self.statements(&statements, &mut inner, true)?;
        }
        Ok(())
    }

    fn define(
        &mut self,
        call: &ruby_prism::CallNode<'_>,
        env: &Env,
        singleton: bool,
    ) -> Result<(), String> {
        let what = if singleton {
            "define_singleton_method"
        } else {
            "define_method"
        };
        let args: Vec<ruby_prism::Node<'_>> = call
            .arguments()
            .map(|a| a.arguments().iter().collect())
            .unwrap_or_default();
        let [name_node] = args.as_slice() else {
            return Err(format!("`{what}` with other than one name argument"));
        };
        let name = match self.literal(name_node, env)? {
            Val::Sym(s) => s,
            Val::Str(s) => Symbol::from(s),
            other => return Err(format!("`{what}` named by {}", other.describe())),
        };
        if !valid_def_name(&name) {
            return Err(format!("`{what}` names `{name}`, which no `def` can"));
        }
        let block = call
            .block()
            .and_then(|b| b.as_block_node())
            .ok_or_else(|| format!("`{what}(:{name})` without a literal block"))?;
        let params = required_block_params(&block)?;
        let loc = call.location();
        let send = ir_at(self.ir, loc.start_offset(), loc.end_offset())
            .ok_or_else(|| format!("the body of `{name}` was not ingested"))?;
        let ExprNode::Send {
            block: Some(lambda),
            ..
        } = &*send.node
        else {
            return Err(format!("the body of `{name}` was not ingested"));
        };
        let ExprNode::Lambda { body, .. } = &*lambda.node else {
            return Err(format!("the body of `{name}` was not ingested"));
        };
        let mut captured = env.clone();
        captured.locals.retain(|(n, _)| !params.contains(n));
        let body = self
            .close_over(body, &captured, &params)
            .map_err(|e| format!("`{name}`: {e}"))?;
        let mut def = MethodDef {
            name: name.clone(),
            receiver: if singleton {
                MethodReceiver::Class
            } else {
                MethodReceiver::Instance
            },
            visibility: if !singleton
                && matches!(
                    name.as_str(),
                    "initialize" | "initialize_copy" | "initialize_dup" | "initialize_clone"
                ) {
                MethodVisibility::Private
            } else {
                MethodVisibility::Public
            },
            params: params.iter().cloned().map(Param::positional).collect(),
            unsupported_formals: None,
            has_anonymous_block: false,
            block_param: None,
            name_span: super::util::node_span(name_node, &self.source.path),
            body,
            signature: None,
            effects: Default::default(),
            enclosing_class: Some(owner_name(self.app, self.owner).0.clone()),
            kind: Default::default(),
            is_async: false,
            mutates_self: false,
        };
        def.name_span.file = self.ir.span.file;
        if self
            .out
            .methods
            .iter()
            .any(|m| m.name == def.name && m.receiver == def.receiver)
        {
            return Err(format!("it defines `{name}` twice"));
        }
        self.out.methods.push(def);
        Ok(())
    }

    /// A class-body effect besides the definitions: kept, for a model, as
    /// a class-body statement with the captured locals substituted.
    fn residual(&mut self, node: &ruby_prism::Node<'_>, env: &Env) -> Result<(), String> {
        if matches!(self.owner, Owner::Library(_)) {
            return Err(format!(
                "`{}` is a class-body effect a library class does not carry",
                text(self.source, node)
            ));
        }
        let loc = node.location();
        let ir = ir_at(self.ir, loc.start_offset(), loc.end_offset())
            .ok_or_else(|| format!("`{}` was not ingested", text(self.source, node)))?;
        let expr = self
            .close_over(ir, env, &[])
            .map_err(|e| format!("`{}`: {e}", text(self.source, node)))?;
        self.out.residual.push(expr);
        Ok(())
    }

    /// `body` as a method body: each captured local read becomes its
    /// literal. Refused when the body writes or shadows a captured local,
    /// reads a local that is neither captured nor its own, or uses what
    /// define_method's block and a `def` answer differently.
    fn close_over(&self, body: &Expr, env: &Env, params: &[Symbol]) -> Result<Expr, String> {
        let mut own: HashSet<Symbol> = params.iter().cloned().collect();
        let mut problem = None;
        collect_bindings(body, &mut own, &mut problem);
        if let Some(problem) = problem {
            return Err(problem);
        }
        if let Some((name, _)) = env
            .locals
            .iter()
            .find(|(n, _)| own.contains(n) && !params.contains(n))
        {
            return Err(format!("it writes or shadows the captured local `{name}`"));
        }
        self.check_constants(body)?;
        let mut out = body.clone();
        substitute(&mut out, env, &own, 0)?;
        Ok(out)
    }

    /// A constant the body names must bind the same way in the class the
    /// method lands on (its emitted nesting: each enclosing namespace) as
    /// where the block was written.
    fn check_constants(&self, body: &Expr) -> Result<(), String> {
        let owner = owner_name(self.app, self.owner)
            .0
            .as_str()
            .trim_start_matches("::");
        let segments: Vec<&str> = owner.split("::").collect();
        let target: Vec<String> = (1..=segments.len())
            .map(|n| segments[..n].join("::"))
            .collect();
        let mut first: Vec<Symbol> = Vec::new();
        collect_constant_heads(body, &mut first);
        for head in first {
            let there = definer(self.constants, &self.nesting, head.as_str());
            let here = definer(self.constants, &target, head.as_str());
            if there != here {
                return Err(format!(
                    "constant `{head}` resolves to {} where the block is written but to {} in the class",
                    there.as_deref().unwrap_or("the top level"),
                    here.as_deref().unwrap_or("the top level")
                ));
            }
        }
        Ok(())
    }

    /// The statically known value of an expression, or why there is none.
    fn literal(&self, node: &ruby_prism::Node<'_>, env: &Env) -> Result<Val, String> {
        if let Some(s) = node.as_symbol_node() {
            return Ok(Val::Sym(Symbol::from(utf8(s.unescaped())?)));
        }
        if let Some(s) = node.as_string_node() {
            return Ok(Val::Str(utf8(s.unescaped())?));
        }
        if let Some(i) = node.as_integer_node() {
            return super::util::integer_i64(&i.value())
                .map(Val::Int)
                .ok_or("a wide integer".into());
        }
        if node.as_nil_node().is_some() {
            return Ok(Val::Nil);
        }
        if node.as_true_node().is_some() {
            return Ok(Val::Bool(true));
        }
        if node.as_false_node().is_some() {
            return Ok(Val::Bool(false));
        }
        if let Some(array) = node.as_array_node() {
            return array
                .elements()
                .iter()
                .map(|e| self.literal(&e, env))
                .collect::<Result<_, _>>()
                .map(Val::Array);
        }
        if let Some(hash) = node.as_hash_node() {
            let mut pairs = Vec::new();
            for element in hash.elements().iter() {
                let assoc = element.as_assoc_node().ok_or("a hash splat")?;
                pairs.push((
                    self.literal(&assoc.key(), env)?,
                    self.literal(&assoc.value(), env)?,
                ));
            }
            return Ok(Val::Hash(pairs));
        }
        if let Some(interp) = node.as_interpolated_string_node() {
            return self.interpolate(interp.parts().iter(), env).map(Val::Str);
        }
        if let Some(interp) = node.as_interpolated_symbol_node() {
            return self
                .interpolate(interp.parts().iter(), env)
                .map(|s| Val::Sym(Symbol::from(s)));
        }
        if let Some(paren) = node.as_parentheses_node() {
            let inner: Vec<_> = paren
                .body()
                .map(super::util::flatten_statements)
                .unwrap_or_default();
            if let [one] = inner.as_slice() {
                return self.literal(one, env);
            }
        }
        if let Some(read) = node.as_local_variable_read_node() {
            let name = constant_id_str(&read.name());
            return env
                .get(name)
                .cloned()
                .ok_or_else(|| format!("local `{name}` is not known statically"));
        }
        if let Some(read) = node.as_constant_read_node() {
            return self.lexical_constant(constant_id_str(&read.name()), node);
        }
        if let Some(call) = node.as_call_node() {
            return self.call_value(&call, env);
        }
        Err(format!(
            "`{}` is not known statically",
            text(self.source, node)
        ))
    }

    fn interpolate<'pr>(
        &self,
        parts: impl Iterator<Item = ruby_prism::Node<'pr>>,
        env: &Env,
    ) -> Result<String, String> {
        let mut out = String::new();
        for part in parts {
            if let Some(s) = part.as_string_node() {
                out.push_str(&utf8(s.unescaped())?);
            } else if let Some(embedded) = part.as_embedded_statements_node() {
                let inner: Vec<_> = embedded
                    .statements()
                    .map(|s| s.body().iter().collect())
                    .unwrap_or_default();
                let [one] = inner.as_slice() else {
                    return Err("an empty or multi-statement interpolation".into());
                };
                out.push_str(&self.literal(one, env)?.to_s()?);
            } else {
                return Err("an interpolation that is not a plain expression".into());
            }
        }
        Ok(out)
    }

    fn call_value(&self, call: &ruby_prism::CallNode<'_>, env: &Env) -> Result<Val, String> {
        let name = constant_id_str(&call.name());
        let args: Vec<ruby_prism::Node<'_>> = call
            .arguments()
            .map(|a| a.arguments().iter().collect())
            .unwrap_or_default();
        if call.block().is_some() {
            return Err(format!("`{name}` with a block"));
        }
        let Some(recv) = call.receiver() else {
            // `const_get(name)` on the class the statement runs in.
            if name == "const_get"
                && let [arg] = args.as_slice()
            {
                return self.const_get(arg, env);
            }
            return Err(format!("`{name}` is not known statically"));
        };
        if recv.as_self_node().is_some()
            && name == "const_get"
            && let [arg] = args.as_slice()
        {
            return self.const_get(arg, env);
        }
        if name == "wrap"
            && let [arg] = args.as_slice()
            && self.names_core_constant(&recv, "Array")
        {
            return Ok(match self.literal(arg, env)? {
                Val::Array(items) => Val::Array(items),
                Val::Nil => Val::Array(Vec::new()),
                other => Val::Array(vec![other]),
            });
        }
        let value = self.literal(&recv, env)?;
        match (name, args.as_slice()) {
            ("freeze", []) => Ok(value),
            ("to_s", []) => value.to_s().map(Val::Str),
            ("to_sym", []) => match value {
                Val::Sym(s) => Ok(Val::Sym(s)),
                Val::Str(s) => Ok(Val::Sym(Symbol::from(s))),
                other => Err(format!("`to_sym` on {}", other.describe())),
            },
            ("upcase" | "downcase", []) => {
                let text = match &value {
                    Val::Sym(s) => s.as_str().to_string(),
                    Val::Str(s) => s.clone(),
                    other => return Err(format!("`{name}` on {}", other.describe())),
                };
                if !text.is_ascii() {
                    return Err(format!("`{name}` of a non-ASCII name"));
                }
                let cased = if name == "upcase" {
                    text.to_ascii_uppercase()
                } else {
                    text.to_ascii_lowercase()
                };
                Ok(match value {
                    Val::Sym(_) => Val::Sym(Symbol::from(cased)),
                    _ => Val::Str(cased),
                })
            }
            ("!", []) => Ok(Val::Bool(!value.truthy()?)),
            ("==" | "!=", [other]) => {
                let other = self.literal(other, env)?;
                Ok(Val::Bool((value == other) == (name == "==")))
            }
            _ => Err(format!("`{name}` is not known statically")),
        }
    }

    /// `Array` (or `::Array`) where no app constant of that name is in
    /// the lexical scope.
    fn names_core_constant(&self, node: &ruby_prism::Node<'_>, name: &str) -> bool {
        let written = node
            .as_constant_read_node()
            .map(|c| constant_id_str(&c.name()) == name)
            .or_else(|| {
                node.as_constant_path_node().map(|p| {
                    p.parent().is_none() && p.name().is_some_and(|n| constant_id_str(&n) == name)
                })
            })
            .unwrap_or(false);
        written
            && definer(self.constants, &self.nesting, name).is_none()
            && !self.constants.contains(name)
    }

    /// `const_get("#{name.upcase}_ID")` on the class: a constant its body
    /// writes before the call site. The method bodies read it as written.
    fn const_get(&self, arg: &ruby_prism::Node<'_>, env: &Env) -> Result<Val, String> {
        let name = match self.literal(arg, env)? {
            Val::Sym(s) => s.as_str().to_string(),
            Val::Str(s) => s,
            other => return Err(format!("`const_get` of {}", other.describe())),
        };
        if name.contains("::") || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return Err(format!(
                "`const_get({name:?})` is not a constant of the class"
            ));
        }
        let class = owner_name(self.app, self.owner)
            .0
            .as_str()
            .trim_start_matches("::")
            .to_string();
        if self.site_nesting.last() != Some(&class) {
            return Err(format!(
                "`const_get({name:?})` runs on {class}, but the call is not written in its body"
            ));
        }
        let (start, end) = self.container_constant(&name).ok_or_else(|| {
            format!("`const_get({name:?})`: {class} does not write `{name}` before the call")
        })?;
        // The object the macro's local holds: an immediate written as a
        // literal is that same object wherever the method body reads it.
        let parsed = ruby_prism::parse(self.site_source.text.as_bytes());
        let root = parsed.node();
        let value = node_at(&root, start, end, |_| true).ok_or("constant value not found")?;
        if let Some(i) = value.as_integer_node() {
            return super::util::integer_i64(&i.value())
                .map(Val::Int)
                .ok_or_else(|| format!("`{class}::{name}` is a wide integer"));
        }
        if let Some(sym) = value.as_symbol_node() {
            return Ok(Val::Sym(Symbol::from(utf8(sym.unescaped())?)));
        }
        if value.as_nil_node().is_some() {
            return Ok(Val::Nil);
        }
        if value.as_true_node().is_some() || value.as_false_node().is_some() {
            return Ok(Val::Bool(value.as_true_node().is_some()));
        }
        Err(format!(
            "`const_get({name:?})`: `{class}::{name}` is not an Integer, Symbol, nil or boolean literal"
        ))
    }

    /// A constant the statements' innermost lexical scope writes, before
    /// them in the same body: a literal, frozen if it is a collection.
    fn lexical_constant(&self, name: &str, at: &ruby_prism::Node<'_>) -> Result<Val, String> {
        if !std::ptr::eq(self.source, self.site_source) || self.nesting != self.site_nesting {
            return Err(format!(
                "constant `{name}` is read where the macro is written"
            ));
        }
        let value = self
            .container_constant(name)
            .filter(|(_, end)| *end <= at.location().start_offset())
            .ok_or_else(|| {
                format!("constant `{name}` is not a literal written earlier in the same body")
            })?;
        let parsed = ruby_prism::parse(self.source.text.as_bytes());
        let root = parsed.node();
        let node = node_at(&root, value.0, value.1, |_| true).ok_or("constant value not found")?;
        let frozen = node
            .as_call_node()
            .is_some_and(|c| constant_id_str(&c.name()) == "freeze" && c.arguments().is_none());
        let value = self
            .literal(&node, &Env::default())
            .map_err(|e| format!("constant `{name}`: {e}"))?;
        if matches!(value, Val::Array(_) | Val::Hash(_)) && !frozen {
            return Err(format!(
                "constant `{name}` is a collection that is not frozen"
            ));
        }
        Ok(value)
    }

    /// The value location of `NAME = …` written directly in the body of
    /// the class holding the call site, before the call.
    fn container_constant(&self, name: &str) -> Option<(usize, usize)> {
        let (start, end) = self.site_container?;
        let parsed = ruby_prism::parse(self.site_source.text.as_bytes());
        let root = parsed.node();
        let container = node_at(&root, start, end, |n| {
            n.as_class_node().is_some() || n.as_module_node().is_some()
        })?;
        let body = container
            .as_class_node()
            .and_then(|c| c.body())
            .or_else(|| container.as_module_node().and_then(|m| m.body()))?;
        super::util::flatten_statements(body)
            .into_iter()
            .filter_map(|stmt| {
                let write = stmt.as_constant_write_node()?;
                (constant_id_str(&write.name()) == name
                    && stmt.location().end_offset() <= self.site_offset)
                    .then(|| {
                        let loc = write.value().location();
                        (loc.start_offset(), loc.end_offset())
                    })
            })
            .last()
    }
}

fn required_block_params(block: &ruby_prism::BlockNode<'_>) -> Result<Vec<Symbol>, String> {
    let Some(params) = block.parameters() else {
        return Ok(Vec::new());
    };
    let params = params
        .as_block_parameters_node()
        .ok_or("numbered or `it` block parameters")?;
    if params.locals().iter().next().is_some() {
        return Err("block-local variables".into());
    }
    let Some(list) = params.parameters() else {
        return Ok(Vec::new());
    };
    if list.optionals().iter().next().is_some()
        || list.rest().is_some()
        || list.posts().iter().next().is_some()
        || list.keywords().iter().next().is_some()
        || list.keyword_rest().is_some()
        || list.block().is_some()
    {
        return Err("block parameters other than required positionals".into());
    }
    list.requireds()
        .iter()
        .map(|p| {
            p.as_required_parameter_node()
                .map(|r| Symbol::from(constant_id_str(&r.name())))
                .ok_or_else(|| "a destructuring block parameter".to_string())
        })
        .collect()
}

/// The innermost lexical scope (full name) that declares `head`, if any.
fn definer(constants: &HashSet<String>, nesting: &[String], head: &str) -> Option<String> {
    nesting
        .iter()
        .rev()
        .find(|scope| constants.contains(&format!("{scope}::{head}")))
        .cloned()
}

fn collect_constant_heads(expr: &Expr, out: &mut Vec<Symbol>) {
    if let ExprNode::Const { path } = &*expr.node
        && let Some(head) = path.first()
        && !head.as_str().is_empty()
        && !out.contains(head)
    {
        out.push(head.clone());
    }
    expr.node
        .for_each_child(&mut |child| collect_constant_heads(child, out));
}

/// Locals the body binds itself (writes, rescue names, nested block
/// parameters), and the constructs a `def` and define_method's block
/// answer differently.
fn collect_bindings(expr: &Expr, own: &mut HashSet<Symbol>, problem: &mut Option<String>) {
    let lvalue = |target: &LValue, own: &mut HashSet<Symbol>| {
        if let LValue::Var { name, .. } = target {
            own.insert(name.clone());
        }
    };
    match &*expr.node {
        ExprNode::Assign { target, .. } | ExprNode::OpAssign { target, .. } => lvalue(target, own),
        ExprNode::MultiAssign { targets, .. } => targets.iter().for_each(|t| lvalue(t, own)),
        ExprNode::Let { name, .. } => {
            own.insert(name.clone());
        }
        ExprNode::Lambda {
            params,
            rest_param,
            block_param,
            ..
        } => {
            own.extend(params.iter().cloned());
            own.extend(rest_param.iter().cloned());
            own.extend(block_param.iter().cloned());
        }
        ExprNode::BeginRescue { rescues, .. } => {
            own.extend(rescues.iter().filter_map(|r| r.binding.clone()));
        }
        ExprNode::Yield { .. } => {
            problem
                .get_or_insert("`yield` in a define_method block reaches the macro's block".into());
        }
        ExprNode::Super { args: None, .. } => {
            problem
                .get_or_insert("implicit-argument `super` raises in a define_method method".into());
        }
        ExprNode::CaseMatch { .. }
        | ExprNode::MatchPredicate { .. }
        | ExprNode::MatchRequired { .. } => {
            problem
                .get_or_insert("pattern matching binds locals the expansion does not track".into());
        }
        ExprNode::Send {
            recv: None, method, ..
        } if matches!(
            method.as_str(),
            "binding" | "local_variables" | "block_given?" | "iterator?" | "eval"
        ) =>
        {
            problem.get_or_insert(format!("`{method}` sees the block's closure"));
        }
        _ => {}
    }
    expr.node
        .for_each_child(&mut |child| collect_bindings(child, own, problem));
}

/// Replace captured-local reads with their literals. `depth` counts the
/// nested blocks: a bare `next`/`break`/`redo` at depth 0 would leave
/// define_method's block where a `def` has no block to leave.
fn substitute(
    expr: &mut Expr,
    env: &Env,
    own: &HashSet<Symbol>,
    depth: usize,
) -> Result<(), String> {
    match &*expr.node {
        ExprNode::Var { name, .. } if !own.contains(name) => {
            let value = env
                .get(name.as_str())
                .ok_or_else(|| format!("it reads `{name}`, a local of the enclosing scope"))?;
            *expr = value.substitute(expr.span).ok_or_else(|| {
                format!(
                    "it reads `{name}`, bound to {}, whose identity a literal would not keep",
                    value.describe()
                )
            })?;
            return Ok(());
        }
        ExprNode::Next { .. } | ExprNode::Break { .. } | ExprNode::Redo if depth == 0 => {
            return Err("a `next`/`break`/`redo` that leaves the define_method block".into());
        }
        _ => {}
    }
    let depth = if matches!(&*expr.node, ExprNode::Lambda { .. }) {
        depth + 1
    } else {
        depth
    };
    let mut result = Ok(());
    expr.node.for_each_child_mut(&mut |child| {
        if result.is_ok() {
            result = substitute(child, env, own, depth);
        }
    });
    result
}

fn valid_def_name(name: &Symbol) -> bool {
    let header = format!("def {name}\nend\n");
    let parsed = ruby_prism::parse(header.as_bytes());
    let node = parsed.node();
    let Some(program) = node.as_program_node() else {
        return false;
    };
    let nodes: Vec<_> = program.statements().body().iter().collect();
    let [node] = nodes.as_slice() else {
        return false;
    };
    let Some(def) = node.as_def_node() else {
        return false;
    };
    parsed.errors().next().is_none()
        && def.receiver().is_none()
        && def.name().as_slice() == name.as_str().as_bytes()
}
