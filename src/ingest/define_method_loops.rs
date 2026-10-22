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
//! required positionals, optionals, a named rest, keywords (required, or
//! with a default known statically), a named keyword rest and a named
//! block, a name the class (or another expansion)
//! already defines, and a library class's non-definition side effects.
//! The diagnostic is reported where the emitted class drops the
//! statement; a library class that replays its body (a gem's base) or a
//! macro of an `extend`ed module (the emit runs those natively) keeps
//! the statement as before, unreported.
//!
//! On the Ruby (CRuby) emit a refused statement is written back as
//! source, since Ruby runs `define_method` itself: the diagnostic stays
//! for `check` and the strict targets, and the class body never holds a
//! raising placeholder that would fire when the class loads. The one
//! exception is a statement hoisted out of a Concern's `class_methods`
//! block (`expr::CLASS_METHODS_BLOCK_STATEMENT`), whose `self` is not the
//! module body it would be written into; it keeps the named refusal.
//! A statement of that block that does expand (Core's
//! `DeprecatedStiClassMethods`, `DEPRECATED_METHODS.each { |name|
//! define_method(name) { |*args, **kwargs, &block| …; super(*args,
//! **kwargs, &block) } }`) defines class methods of every includer: each
//! becomes a class-side method of the concern, expanded before the
//! concern splice (`expand_class_methods_blocks`) so the splice copies it
//! onto model includers and the emitted `ClassMethods` carries it.
//!
//! INHERITED MACROS. A library class's call to a class macro its app
//! superclass defines (Core's `Sales::OrderEditing::StagedChanges::Base`
//! has `class << self; def data_accessor(*keys)`, and each subclass
//! calls it with its own keys: `ReverseWriteOff` has
//! `data_accessor(:write_off_id)`) is expanded for that subclass alone:
//! the methods are the ones Ruby defines on the class whose body makes
//! the call. The emitted subclass replays the call itself, which runs
//! the inherited macro natively, so the call stays and the copies are
//! analysis's and the other targets' (`App::class_eval_macro_expansions`
//! tells the Ruby emit to drop them). Because the replay runs the
//! macro's other statements, a call statement that cannot affect what
//! the evaluator tracks (`data_attributes.concat(keys)`, see
//! `passes_over`) and the class-body effects are passed over rather
//! than refused; a `define_singleton_method` there is still refused.
//! Two such calls defining one name keep the later one, as the replay
//! does (`shadow_replayed`).

use crate::ingest::prism_api::PrismLocation;
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

/// The app's library classes and models by name, for the macro
/// lookups: each was a scan of every library class, per namespace
/// prefix, per ancestor, per class-body call.
struct Names<'a> {
    /// Each name's app (origin-less) classes, not modules, in order.
    classes: HashMap<&'a str, Vec<&'a crate::dialect::LibraryClass>>,
    /// Each name's first app module.
    modules: HashMap<&'a str, &'a crate::dialect::LibraryClass>,
    models: HashSet<&'a str>,
}

impl<'a> Names<'a> {
    fn new(app: &'a App) -> Self {
        let mut classes: HashMap<&str, Vec<&crate::dialect::LibraryClass>> = HashMap::default();
        let mut modules: HashMap<&str, &crate::dialect::LibraryClass> = HashMap::default();
        for c in app.library_classes.iter().filter(|c| c.origin.is_none()) {
            if c.is_module {
                modules.entry(c.name.0.as_str()).or_insert(c);
            } else {
                classes.entry(c.name.0.as_str()).or_default().push(c);
            }
        }
        let models = app.models.iter().map(|m| m.name.0.as_str()).collect();
        Names { classes, modules, models }
    }

    /// The app classes (not modules) named `name`, in order.
    fn classes(&self, name: &str) -> &[&'a crate::dialect::LibraryClass] {
        self.classes.get(name).map_or(&[], Vec::as_slice)
    }
}

/// How a macro call reaches the macro it runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The class's own class method, or an eval block in its body.
    Own,
    /// An instance method of a module the class `extend`s.
    Extended,
    /// A class method of an app library superclass (or of a module it
    /// `extend`s): the emitted subclass replays the call natively.
    Inherited,
}

struct Expansion {
    methods: Vec<MethodDef>,
    residual: Vec<Expr>,
    /// Names a later replayed call on the same class redefines: this
    /// call still defines them when the emit replays it, so the emitted
    /// class drops their copies too, but analysis keeps the later ones.
    shadowed: Vec<Symbol>,
}

pub(super) fn expand(app: &mut App, sources: &[SourceFile]) {
    run(app, sources, None);
}

/// The statements a Concern's `class_methods` block hoisted into the
/// concern's body (`expr::CLASS_METHODS_BLOCK_STATEMENT`), expanded into
/// class-side methods before the concern splice reads them: each
/// expanded method's span joins its concern's `methods` and
/// `block_methods`, so the splice and the emitted `ClassMethods` module
/// carry it as they carry a `def` written in the block.
pub(super) fn expand_class_methods_blocks(
    app: &mut App,
    sources: &[SourceFile],
    carriers: &mut [super::library_class::ConcernClassMethodSpans],
) {
    for (owner, spans) in run(app, sources, Some(carriers)) {
        if let Some(carrier) = carriers
            .iter_mut()
            .find(|c| c.owner == owner && !c.concern_extensions.is_empty())
        {
            carrier.methods.extend(spans.iter().copied());
            carrier.block_methods.extend(spans);
        }
    }
}

/// `carriers` is `Some` for the early pass over `class_methods` block
/// statements alone, which answers the names it expanded per concern;
/// the general pass leaves those statements to it.
fn run(
    app: &mut App,
    sources: &[SourceFile],
    carriers: Option<&[super::library_class::ConcernClassMethodSpans]>,
) -> Vec<(ClassId, Vec<Span>)> {
    let early = carriers.is_some();
    let mut class_side: Vec<(ClassId, Vec<Span>)> = Vec::new();
    let constants = app_constants(app);
    let mut library: Vec<(usize, usize, Result<Expansion, String>)> = Vec::new();
    // Calls the emitted class replays when they stay: a library class
    // runs a macro of a module it `extend`s, or one an app superclass
    // defines, natively (`emit::ruby::library`'s `app_extension_calls`).
    let mut replayed: HashSet<(usize, usize)> = HashSet::default();
    let mut inherited: HashSet<(usize, usize)> = HashSet::default();
    let names = Names::new(app);
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.origin.is_some() {
            continue;
        }
        for (j, call) in lc.unknown_calls.iter().enumerate() {
            // A statement the body keeps whole runs as written where it
            // stands (`crate::expr::BODY_STATEMENT`), or stays reported.
            if call.decisions & crate::expr::BODY_STATEMENT != 0 {
                continue;
            }
            // Only a Concern's block is written as its `ClassMethods`.
            if (call.decisions & crate::expr::CLASS_METHODS_BLOCK_STATEMENT != 0) != early
                || carriers.is_some_and(|carriers| {
                    !carriers.iter().any(|c| c.owner == lc.name && !c.concern_extensions.is_empty())
                })
            {
                continue;
            }
            if let Some((outcome, reach)) =
                plan(app, &names, sources, &constants, Owner::Library(i), call)
            {
                if reach != Reach::Own {
                    replayed.insert((i, j));
                }
                if reach == Reach::Inherited {
                    inherited.insert((i, j));
                }
                library.push((i, j, outcome));
            }
        }
    }
    let mut models: Vec<(usize, usize, Result<Expansion, String>)> = Vec::new();
    for (i, model) in app.models.iter().enumerate().filter(|_| !early) {
        for (j, item) in model.body.iter().enumerate() {
            let ModelBodyItem::Unknown { expr, .. } = item else {
                continue;
            };
            if let Some((outcome, _)) = plan(app, &names, sources, &constants, Owner::Model(i), expr) {
                models.push((i, j, outcome));
            }
        }
    }
    shadow_replayed(app, &mut library, &inherited);
    let collided = settle_collisions(app, &mut library, Owner::Library);
    settle_collisions(app, &mut models, Owner::Model);

    // Only a refused call reads its class's answer.
    let dropped: Vec<bool> = {
        let asked: HashSet<usize> = library
            .iter()
            .filter(|(i, j, outcome)| outcome.is_err() && !replayed.contains(&(*i, *j)))
            .map(|(i, ..)| *i)
            .collect();
        let lineage = (!asked.is_empty()).then(|| Lineage::new(app));
        (0..app.library_classes.len())
            .map(|i| lineage.as_ref().is_some_and(|lineage| asked.contains(&i) && body_dropped(app, lineage, i)))
            .collect()
    };
    // Apply in reverse index order so earlier indices stay valid.
    library.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
    for (i, j, outcome) in library {
        let lc = &mut app.library_classes[i];
        match outcome {
            Ok(expansion) if inherited.contains(&(i, j)) => {
                // The emitted subclass replays the call, which runs the
                // inherited macro as Ruby does; these copies are what
                // analysis and the other targets see of it, and the
                // Ruby emit writes the class without them.
                let call = lc.unknown_calls[j].span;
                let mut names: Vec<Symbol> = expansion.methods.iter().map(|m| m.name.clone()).collect();
                names.extend(expansion.shadowed);
                lc.methods.extend(expansion.methods);
                let name = lc.name.clone();
                app.class_eval_macro_expansions
                    .entry(name)
                    .or_default()
                    .push(crate::app::ClassEvalExpansion { call, methods: names, hook: false });
            }
            Ok(expansion) => {
                lc.unknown_calls.remove(j);
                if early {
                    let spans = expansion.methods.iter().map(|m| m.name_span).collect();
                    class_side.push((lc.name.clone(), spans));
                }
                lc.methods.extend(expansion.methods);
            }
            Err(reason) => {
                // A loop on a literal list whose names the class defines
                // elsewhere too (`%i[a b].each { define_method }` above
                // a `def b` or an `alias_method :a`) is not settled here:
                // which definition Ruby keeps is the written body's
                // order. It stays the class-body call it was, which the
                // Ruby family runs where it stands (CRuby keeps the
                // later one) and the other targets name as the class
                // body statement it is.
                let order_decides = collided.contains(&(i, j)) && super::app::is_literal_list_call(&lc.unknown_calls[j]);
                if dropped[i] && !replayed.contains(&(i, j)) && !order_decides {
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
    class_side
}

/// Whether the emitted library class drops a class-body statement it
/// does not model: its superclass chain ends in no class or in an app
/// class or model, and (a module) it includes only app modules. Any
/// other class replays its class body verbatim when the emitted file
/// loads (the gem's class is its receiver), so the statement runs there
/// and an unexpanded one is no gap to report.
fn body_dropped(app: &App, lineage: &Lineage<'_>, i: usize) -> bool {
    let app_class = |name: &str| {
        let name = name.trim_start_matches("::");
        lineage.models.contains(name) || lineage.app_classes.contains(name)
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
        if lineage.models.contains(p.0.as_str()) || seen.contains(&p.0.as_str()) {
            return true;
        }
        seen.push(p.0.as_str());
        parent = lineage.parents.get(p.0.as_str()).copied();
    }
    true
}

/// What [`body_dropped`] asks of every class, by name.
struct Lineage<'a> {
    models: HashSet<&'a str>,
    /// The app (origin-less) library classes' names, without a leading `::`.
    app_classes: HashSet<&'a str>,
    /// Each name's first parent among its classes (not modules).
    parents: HashMap<&'a str, &'a ClassId>,
}

impl<'a> Lineage<'a> {
    fn new(app: &'a App) -> Self {
        let mut parents: HashMap<&str, &ClassId> = HashMap::default();
        for c in app.library_classes.iter().filter(|c| !c.is_module) {
            if let Some(parent) = &c.parent {
                parents.entry(c.name.0.as_str()).or_insert(parent);
            }
        }
        Lineage {
            models: app.models.iter().map(|m| m.name.0.as_str()).collect(),
            app_classes: app
                .library_classes
                .iter()
                .filter(|c| c.origin.is_none())
                .map(|c| c.name.0.as_str().trim_start_matches("::"))
                .collect(),
            parents,
        }
    }
}

fn refuse(call: &mut Expr, reason: String) {
    call.diagnostic_mut().get_or_insert(DiagnosticKind::Unsupported {
        target: None,
        construct: Symbol::from(REFUSAL_CONSTRUCT),
        detail: reason,
    });
}

/// Two replayed calls on one class body that define the same name
/// (Core's `AddManualDiscount` calls `data_accessor(:fixed_amount, …)`
/// then `money_data_accessor(:fixed_amount)`): the emitted class runs
/// both, in order, so the later definition is the one Ruby keeps. The
/// earlier expansion gives that name up (its call still defines it when
/// replayed, so the emit drops its copy too). Calls in different files
/// (a reopened class) have no order here and stay collisions.
fn shadow_replayed(
    app: &App,
    planned: &mut [(usize, usize, Result<Expansion, String>)],
    inherited: &HashSet<(usize, usize)>,
) {
    let spans: Vec<Option<(String, Span)>> = planned
        .iter()
        .map(|(i, j, outcome)| {
            (outcome.is_ok() && inherited.contains(&(*i, *j))).then(|| {
                let lc = &app.library_classes[*i];
                (lc.name.0.to_string(), lc.unknown_calls[*j].span)
            })
        })
        .collect();
    let defined: Vec<Vec<Symbol>> = planned
        .iter()
        .map(|(_, _, outcome)| match outcome {
            Ok(e) => e.methods.iter().map(|m| m.name.clone()).collect(),
            Err(_) => Vec::new(),
        })
        .collect();
    for (k, entry) in planned.iter_mut().enumerate() {
        let Some((class, span)) = &spans[k] else { continue };
        let later: HashSet<&Symbol> = spans
            .iter()
            .enumerate()
            .filter(|(_, other)| {
                other.as_ref().is_some_and(|(c, s)| c == class && s.file == span.file && s.start > span.start)
            })
            .flat_map(|(l, _)| defined[l].iter())
            .collect();
        let Ok(expansion) = &mut entry.2 else { continue };
        let (gone, kept): (Vec<MethodDef>, Vec<MethodDef>) = std::mem::take(&mut expansion.methods)
            .into_iter()
            .partition(|m| m.receiver == MethodReceiver::Instance && later.contains(&m.name));
        expansion.methods = kept;
        expansion.shadowed.extend(gone.into_iter().map(|m| m.name));
    }
}

/// Ruby keeps the last definition of a name; an expansion that would
/// redefine one the class has (or another expansion makes) is refused,
/// both sides, rather than guessing which one the emitted class keeps.
fn settle_collisions(
    app: &App,
    planned: &mut [(usize, usize, Result<Expansion, String>)],
    owner: fn(usize) -> Owner,
) -> HashSet<(usize, usize)> {
    let mut collided = HashSet::default();
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
    for (i, j, outcome) in planned.iter_mut() {
        let Ok(expansion) = outcome else { continue };
        let taken = existing_names(app, owner(*i));
        let name = owner_name(app, owner(*i)).0.to_string();
        let clash = expansion.methods.iter().find(|m| {
            let class = m.receiver == MethodReceiver::Class;
            counts[&(name.clone(), m.name.clone(), class)] > 1
                || taken.contains(&(m.name.clone(), class))
        });
        if let Some(m) = clash {
            collided.insert((*i, *j));
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
    collided
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
/// the expansion, or why there is none, and how the call reaches its
/// macro.
fn plan(
    app: &App,
    names: &Names<'_>,
    sources: &[SourceFile],
    constants: &HashSet<String>,
    owner: Owner,
    call: &Expr,
) -> Option<(Result<Expansion, String>, Reach)> {
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
    // A class body's own `COLLECTION.each { |name| define_method … }`: the
    // loop is the statement the evaluator walks.
    if matches!(method.as_str(), "each" | "each_pair")
        && args.is_empty()
        && recv.is_some()
        && block.as_ref().is_some_and(|b| defines(b))
    {
        return Some((plan_eval(app, sources, constants, owner, call, true), Reach::Own));
    }
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
        return Some((plan_eval(app, sources, constants, owner, call, false), Reach::Own));
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
    let (def, via, inherited) = macro_for(app, names, owner, method, call.span)?;
    if !defines(&def.body) {
        return None;
    }
    let reach = if inherited {
        Reach::Inherited
    } else if via != owner_name(app, owner).0.as_str() {
        Reach::Extended
    } else {
        Reach::Own
    };
    Some((
        plan_macro(app, sources, constants, owner, call, def, via, reach == Reach::Inherited),
        reach,
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

/// The app macro a bare class-body call `name` reaches, where it lives,
/// and whether the class inherits it: the class's own class-side method
/// (a concern's `class_methods` arrive spliced as such), an instance
/// method of a module the class `extend`s before the call, or (a
/// library class) a class method of an app library superclass or of a
/// module that superclass `extend`s (`inherited_macro`). A model's
/// concern macros are `model_macros`' to expand. A `def self.x` written
/// after the call in the same file is not there yet when the class body
/// calls it.
fn macro_for<'a>(
    app: &'a App,
    names: &Names<'a>,
    owner: Owner,
    name: &Symbol,
    at: Span,
) -> Option<(&'a MethodDef, String, bool)> {
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
                [def] => return Some((def, lc.name.0.to_string(), false)),
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
            let Some(module) = resolve_module(names, scope, path) else {
                continue;
            };
            if let Some(def) = module_method(names, module, name, &mut Vec::new()) {
                return Some((def, module.name.0.to_string(), false));
            }
        }
    }
    match owner {
        Owner::Library(i) => inherited_macro(names, &app.library_classes[i], name)
            .map(|(def, via)| (def, via, true)),
        Owner::Model(_) => None,
    }
}

/// The class macro `name` a library class inherits: Ruby looks it up on
/// the singleton classes of its superclass chain, nearest first, each
/// with the modules it `extend`s. Only app library classes count (the
/// superclass as Ruby reads it from the subclass's namespace outwards;
/// the ingest has not resolved it yet), as the Ruby emit replays exactly
/// those calls (`emit::ruby::library`'s `inherited_class_macro_calls`);
/// a model, a gem class or a cycle ends the chain. A name an ancestor
/// defines more than once (a reopened class) is not guessed at.
fn inherited_macro<'a>(
    names: &Names<'a>,
    lc: &'a crate::dialect::LibraryClass,
    name: &Symbol,
) -> Option<(&'a MethodDef, String)> {
    if lc.is_module {
        return None;
    }
    let mut seen: Vec<&ClassId> = vec![&lc.name];
    let mut at = lc;
    while let Some(parent) = at.parent.as_ref() {
        let ancestor = resolve_class(names, &at.name, parent.0.as_str())?;
        if seen.contains(&ancestor) || names.models.contains(ancestor.0.as_str()) {
            return None;
        }
        seen.push(ancestor);
        let parts: &[&crate::dialect::LibraryClass] = names.classes(ancestor.0.as_str());
        let own: Vec<&MethodDef> = parts
            .iter()
            .flat_map(|c| c.methods.iter())
            .filter(|m| m.receiver == MethodReceiver::Class && &m.name == name)
            .collect();
        match own.as_slice() {
            [def] => return Some((def, ancestor.0.to_string())),
            [] => {}
            _ => return None,
        }
        for part in parts {
            for call in part.unknown_calls.iter().rev() {
                let ExprNode::Send { recv: None, method, args, .. } = &*call.node else {
                    continue;
                };
                if method.as_str() != "extend" {
                    continue;
                }
                for arg in args.iter().rev() {
                    let ExprNode::Const { path } = &*arg.node else {
                        continue;
                    };
                    let Some(module) = resolve_module(names, ancestor, path) else {
                        continue;
                    };
                    if let Some(def) = module_method(names, module, name, &mut Vec::new()) {
                        return Some((def, module.name.0.to_string()));
                    }
                }
            }
        }
        at = parts.iter().copied().find(|c| c.parent.is_some()).unwrap_or(parts[0]);
    }
    None
}

/// The app library class (not a module) `written` names, read from
/// `scope`'s namespace outwards.
fn resolve_class<'a>(names: &Names<'a>, scope: &ClassId, written: &str) -> Option<&'a ClassId> {
    let find = |name: &str| names.classes(name).first().map(|c| &c.name);
    if let Some(rooted) = written.strip_prefix("::") {
        return find(rooted);
    }
    let segments: Vec<&str> = scope.0.as_str().split("::").collect();
    (0..segments.len()).rev().find_map(|n| {
        if n == 0 {
            find(written)
        } else {
            find(&format!("{}::{written}", segments[..n].join("::")))
        }
    })
}

fn resolve_module<'a>(
    names: &Names<'a>,
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
        names.modules.get(name.as_str()).copied()
    })
}

fn module_method<'a>(
    names: &Names<'a>,
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
        resolve_module(names, &module.name, &path)
            .and_then(|inner| module_method(names, inner, name, seen))
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
pub(super) fn node_at<'pr>(
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

/// Methods that read their argument without changing it, as a call
/// statement's outermost send (`data_attributes.concat(keys)`).
const READS_ARGUMENTS: &[&str] = &["concat", "push", "append", "<<", "unshift", "merge!", "update", "add"];

/// Whether a replayed macro's call statement can be passed over: it
/// takes no block, sends to `self` (a chain of blockless sends rooted
/// at an implicit or explicit `self`), writes no local, and reads the
/// macro's locals only as direct arguments of an outermost method that
/// does not change its argument. Nothing it does can then change the
/// locals the evaluator tracks or a value a later `define_method` reads;
/// the replay runs it as Ruby does (Core's `data_accessor(*keys)` starts
/// with `data_attributes.concat(keys)`).
fn passes_over(call: &ruby_prism::CallNode<'_>, env: &Env) -> bool {
    if call.block().is_some() {
        return false;
    }
    let mut receiver = call.receiver();
    while let Some(node) = receiver {
        if node.as_self_node().is_some() {
            break;
        }
        let Some(inner) = node.as_call_node() else { return false };
        if inner.block().is_some() {
            return false;
        }
        receiver = inner.receiver();
    }
    struct Locals {
        reads: usize,
        writes: bool,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Locals {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(&node);
        }
        fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(&node);
        }
    }
    impl Locals {
        fn check(&mut self, node: &ruby_prism::Node<'_>) {
            if node.as_local_variable_read_node().is_some() {
                self.reads += 1;
            }
            if node.as_local_variable_write_node().is_some()
                || node.as_local_variable_operator_write_node().is_some()
                || node.as_local_variable_or_write_node().is_some()
                || node.as_local_variable_and_write_node().is_some()
                || node.as_local_variable_target_node().is_some()
                || node.as_block_node().is_some()
                || node.as_lambda_node().is_some()
            {
                self.writes = true;
            }
        }
    }
    let mut locals = Locals { reads: 0, writes: false };
    ruby_prism::Visit::visit(&mut locals, &call.as_node());
    if locals.writes {
        return false;
    }
    if locals.reads == 0 {
        return true;
    }
    if !READS_ARGUMENTS.contains(&constant_id_str(&call.name())) {
        return false;
    }
    let direct = call
        .arguments()
        .map(|a| {
            a.arguments()
                .iter()
                .filter(|arg| {
                    let known = |read: Option<ruby_prism::LocalVariableReadNode<'_>>| {
                        read.is_some_and(|r| env.get(constant_id_str(&r.name())).is_some())
                    };
                    match arg.as_splat_node().and_then(|s| s.expression()) {
                        Some(inner) => known(inner.as_local_variable_read_node()),
                        None => known(arg.as_local_variable_read_node()),
                    }
                })
                .count()
        })
        .unwrap_or(0);
    direct == locals.reads
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
    /// The emitted class replays the call natively (an inherited macro),
    /// so the statements besides the definitions run there: a call
    /// statement that cannot change what the evaluator tracks is passed
    /// over (`passes_over`) instead of refused, and no class-body effect
    /// is carried.
    replayed: bool,
    /// The statements came from a concern's `class_methods` block: what
    /// they define are class methods of every includer.
    class_side: bool,
    out: Expansion,
}

fn plan_eval(
    app: &App,
    sources: &[SourceFile],
    constants: &HashSet<String>,
    owner: Owner,
    call: &Expr,
    is_loop: bool,
) -> Result<Expansion, String> {
    let class_side = call.decisions & crate::expr::CLASS_METHODS_BLOCK_STATEMENT != 0;
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
    if !is_loop && block.parameters().is_some() {
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
        replayed: false,
        class_side,
        out: Expansion {
            methods: Vec::new(),
            residual: Vec::new(),
            shadowed: Vec::new(),
        },
    };
    let statements: Vec<ruby_prism::Node<'_>> = if is_loop {
        vec![node.as_node()]
    } else {
        block
            .body()
            .map(super::util::flatten_statements)
            .unwrap_or_default()
    };
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
    replayed: bool,
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
        replayed,
        class_side: false,
        out: Expansion {
            methods: Vec::new(),
            residual: Vec::new(),
            shadowed: Vec::new(),
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
    /// The macro's parameters bound to the call's literal arguments:
    /// required positionals, a rest, and keywords (a required one, or an
    /// optional one whose default is known statically), as Ruby binds
    /// them. A braceless trailing hash is the keywords only when the
    /// macro takes keywords; otherwise it is a positional hash, which is
    /// refused.
    fn bind(
        &self,
        def: &ruby_prism::DefNode<'_>,
        call: &ruby_prism::CallNode<'_>,
        method: &Symbol,
    ) -> Result<Env, String> {
        if call.block().is_some() {
            return Err(format!("`{method}` is called with a block"));
        }
        let params = def.parameters();
        let takes_keywords = params
            .as_ref()
            .is_some_and(|p| p.keywords().iter().next().is_some());
        let mut args = Vec::new();
        let mut keywords: Vec<(Symbol, Val)> = Vec::new();
        if let Some(list) = call.arguments() {
            let all: Vec<ruby_prism::Node<'_>> = list.arguments().iter().collect();
            for (k, arg) in all.iter().enumerate() {
                if let Some(hash) = arg.as_keyword_hash_node()
                    && takes_keywords
                    && k + 1 == all.len()
                {
                    for element in hash.elements().iter() {
                        let assoc = element
                            .as_assoc_node()
                            .ok_or(format!("`{method}` is called with a keyword splat"))?;
                        let Val::Sym(key) = self.literal(&assoc.key(), &Env::default())? else {
                            return Err(format!("`{method}` is called with a non-Symbol keyword"));
                        };
                        let value = self
                            .literal(&assoc.value(), &Env::default())
                            .map_err(|e| format!("keyword `{key}` of `{method}`: {e}"))?;
                        keywords.push((key, value));
                    }
                    continue;
                }
                if arg.as_keyword_hash_node().is_some() || arg.as_splat_node().is_some() {
                    return Err(format!(
                        "`{method}` is called with splatted or keyword arguments"
                    ));
                }
                args.push(
                    self.literal(arg, &Env::default())
                        .map_err(|e| format!("an argument of `{method}`: {e}"))?,
                );
            }
        }
        let mut env = Env::default();
        let mut args = args.into_iter();
        if let Some(params) = params {
            if params.optionals().iter().next().is_some()
                || params.posts().iter().next().is_some()
                || params.keyword_rest().is_some()
                || params.block().is_some()
            {
                return Err(format!(
                    "`{method}` takes parameters other than required positionals, a rest and keywords"
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
            for keyword in params.keywords().iter() {
                let (name, default) = if let Some(required) = keyword.as_required_keyword_parameter_node() {
                    (Symbol::from(constant_id_str(&required.name())), None)
                } else if let Some(optional) = keyword.as_optional_keyword_parameter_node() {
                    (Symbol::from(constant_id_str(&optional.name())), Some(optional.value()))
                } else {
                    return Err(format!("`{method}` takes an unrecognized keyword"));
                };
                let value = match keywords.iter().position(|(k, _)| *k == name) {
                    Some(at) => keywords.remove(at).1,
                    None => match default {
                        Some(node) => self
                            .literal(&node, &env)
                            .map_err(|e| format!("the default of `{method}`'s `{name}:`: {e}"))?,
                        None => return Err(format!("`{method}` is called without its keyword `{name}:`")),
                    },
                };
                env.set(name, value);
            }
        }
        if let Some((key, _)) = keywords.first() {
            return Err(format!("`{method}` is called with the unknown keyword `{key}:`"));
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
                _ if self.replayed && passes_over(&call, env) => return Ok(Flow::Normal),
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
        if singleton && self.replayed {
            // The emitted class drops only instance-side copies of a
            // replayed call (`App::class_eval_macro_expansions`).
            return Err(format!("`{what}` in a replayed inherited macro"));
        }
        if singleton && self.class_side {
            // Its receiver is the concern's `ClassMethods` module, not
            // the includer.
            return Err(format!("`{what}` in a `class_methods` block"));
        }
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
        // A name no `def` can spell (Core's `ShopPlan.names` holds
        // "grandfather-trial") is still a method `define_method` makes: a
        // model keeps the call, naming it with the evaluated Symbol.
        let spellable = valid_def_name(&name);
        if !spellable
            && (singleton || self.replayed || self.class_side || !matches!(self.owner, Owner::Model(_)))
        {
            return Err(format!("`{what}` names `{name}`, which no `def` can"));
        }
        let block = call
            .block()
            .and_then(|b| b.as_block_node())
            .ok_or_else(|| format!("`{what}(:{name})` without a literal block"))?;
        let params = define_method_block_params(&block)?;
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
        let ExprNode::Lambda { body, signature, rest_param, block_param, .. } = &*lambda.node else {
            return Err(format!("the body of `{name}` was not ingested"));
        };
        // The block's optional, keyword and keyword-rest parameters are
        // the method's, as define_method makes them; a default closes
        // over the captured locals as the body does.
        let names: Vec<Symbol> = params
            .iter()
            .chain(signature.names())
            .chain(rest_param.iter())
            .chain(block_param.iter())
            .cloned()
            .collect();
        let mut captured = env.clone();
        captured.locals.retain(|(n, _)| !names.contains(n));
        let close = |e: &Expr| self.close_over(e, &captured, &names).map_err(|e| format!("`{name}`: {e}"));
        let mut formals: Vec<Param> = params.iter().cloned().map(Param::positional).collect();
        for (n, default) in &signature.optionals {
            formals.push(Param::with_default(n.clone(), close(default)?));
        }
        if let Some(n) = rest_param {
            formals.push(Param::rest(n.clone()));
        }
        for (n, default) in &signature.keywords {
            formals.push(Param::keyword(n.clone(), default.as_ref().map(close).transpose()?));
        }
        if let Some(n) = &signature.kwrest {
            formals.push(Param { rest: true, ..Param::keyword(n.clone(), None) });
        }
        let body = close(body)?;
        if !spellable {
            if !signature.optionals.is_empty() || !signature.keywords.is_empty() || signature.kwrest.is_some() {
                return Err(format!(
                    "`{what}` names `{name}`, which no `def` can, and its block has optional or keyword parameters"
                ));
            }
            let mut call = send.clone();
            if let ExprNode::Send { args, block: Some(lambda), .. } = &mut *call.node {
                args[0] = Expr::new(args[0].span, ExprNode::Lit { value: Literal::Sym { value: name.clone() } });
                if let ExprNode::Lambda { body: lambda_body, .. } = &mut *lambda.node {
                    *lambda_body = body;
                }
            }
            self.out.residual.push(call);
            return Ok(());
        }
        let mut def = MethodDef {
            name: name.clone(),
            receiver: if singleton || self.class_side {
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
            params: formals,
            unsupported_formals: None,
            has_anonymous_block: false,
            block_param: block_param.clone().map(Param::positional),
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
        if self.replayed {
            return Ok(());
        }
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
        // `cond ? a : b`, or an `if` expression, whose condition and
        // chosen single-expression branch are known statically.
        if let Some(branch) = node.as_if_node() {
            let taken = self.literal(&branch.predicate(), env)?.truthy()?;
            let chosen: Vec<ruby_prism::Node<'_>> = if taken {
                branch.statements().map(|s| s.body().iter().collect()).unwrap_or_default()
            } else {
                match branch.subsequent().and_then(|other| other.as_else_node()) {
                    Some(other) => other
                        .statements()
                        .map(|s| s.body().iter().collect())
                        .unwrap_or_default(),
                    None => Vec::new(),
                }
            };
            return match chosen.as_slice() {
                [] => Ok(Val::Nil),
                [one] => self.literal(one, env),
                _ => Err(format!("`{}` has a multi-statement branch", text(self.source, node))),
            };
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
        if args.is_empty()
            && let Some(value) = self.class_method_literal(&recv, name)
        {
            return value;
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

    /// `Const.name` where `Const` is an app class whose class method
    /// `name` takes no arguments and answers one literal collection
    /// (`def self.names; ["a", "b"]; end`): that collection, as the call
    /// answers it every time. None when the receiver is no such class.
    fn class_method_literal(
        &self,
        recv: &ruby_prism::Node<'_>,
        name: &str,
    ) -> Option<Result<Val, String>> {
        let written = if let Some(read) = recv.as_constant_read_node() {
            constant_id_str(&read.name()).to_string()
        } else if recv.as_constant_path_node().is_some() {
            text(self.source, recv)
        } else {
            return None;
        };
        let written = written.trim_start_matches("::");
        let candidates: Vec<String> = (0..=self.nesting.len())
            .rev()
            .map(|n| match n {
                0 => written.to_string(),
                n => format!("{}::{written}", self.nesting[n - 1]),
            })
            .collect();
        let class = candidates.iter().find_map(|candidate| {
            self.app
                .library_classes
                .iter()
                .find(|c| c.origin.is_none() && !c.is_module && c.name.0.as_str() == candidate)
        })?;
        let defs: Vec<&MethodDef> = class
            .methods
            .iter()
            .filter(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == name)
            .collect();
        let [def] = defs.as_slice() else { return None };
        if !def.params.is_empty() {
            return None;
        }
        Some(literal_value(&def.body).ok_or_else(|| {
            format!("`{written}.{name}` does not answer a literal collection")
        }))
    }

    /// `Array` (or `::Array`) where no app constant of that name is in
    /// the lexical scope. An app's own top-level `class Array` (a core
    /// extension file) reopens the core class rather than shadowing it,
    /// so only a nested `Scope::Array` counts.
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
        written && definer(self.constants, &self.nesting, name).is_none()
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

/// A `define_method` block's required positionals. Its optional,
/// keyword and named keyword-rest parameters are allowed too: they are
/// in the ingested block's `signature`, which `Scope::define`
/// reads. A rest, post, block or nameless `**` parameter is refused.
/// A literal an IR body evaluates to: scalars, and arrays and hashes
/// of them, as the method answers it on every call.
fn literal_value(expr: &Expr) -> Option<Val> {
    let expr = match &*expr.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => &exprs[0],
        _ => expr,
    };
    match &*expr.node {
        ExprNode::Lit { value: Literal::Str { value } } => Some(Val::Str(value.clone())),
        ExprNode::Lit { value: Literal::Sym { value } } => Some(Val::Sym(value.clone())),
        ExprNode::Lit { value: Literal::Int { value } } => Some(Val::Int(*value)),
        ExprNode::Lit { value: Literal::Nil } => Some(Val::Nil),
        ExprNode::Lit { value: Literal::Bool { value } } => Some(Val::Bool(*value)),
        ExprNode::Array { elements, .. } => {
            elements.iter().map(literal_value).collect::<Option<_>>().map(Val::Array)
        }
        ExprNode::Hash { entries, .. } => entries
            .iter()
            .map(|(k, v)| Some((literal_value(k)?, literal_value(v)?)))
            .collect::<Option<_>>()
            .map(Val::Hash),
        _ => None,
    }
}

fn define_method_block_params(block: &ruby_prism::BlockNode<'_>) -> Result<Vec<Symbol>, String> {
    let Some(list) = block
        .parameters()
        .and_then(|p| p.as_block_parameters_node())
        .and_then(|p| p.parameters())
    else {
        return required_block_params(block);
    };
    let named_kwrest = list.keyword_rest().is_none_or(|k| {
        k.as_keyword_rest_parameter_node().is_some_and(|k| k.name().is_some())
    });
    let named_rest = list.rest().is_none_or(|r| r.as_rest_parameter_node().is_some_and(|r| r.name().is_some()));
    let named_block = list.block().is_none_or(|_| super::prism_api::block_parameter(&list).is_some_and(|b| b.name().is_some()));
    if !named_rest || !named_block || list.posts().iter().next().is_some() || !named_kwrest {
        return Err(
            "block parameters other than required, optional, named rest, keyword, named keyword-rest and named block ones"
                .into(),
        );
    }
    let required: Vec<Symbol> = list
        .requireds()
        .iter()
        .map(|p| {
            p.as_required_parameter_node()
                .map(|r| Symbol::from(constant_id_str(&r.name())))
                .ok_or_else(|| "a destructuring block parameter".to_string())
        })
        .collect::<Result<_, _>>()?;
    if block.parameters().and_then(|p| p.as_block_parameters_node()).is_some_and(|p| p.locals().iter().next().is_some()) {
        return Err("block-local variables".into());
    }
    Ok(required)
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
            signature,
            ..
        } => {
            own.extend(params.iter().cloned());
            own.extend(rest_param.iter().cloned());
            own.extend(signature.names().cloned());
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
    // `captured == other`: a String literal compares the same as the
    // captured String (String#==), and an operand's identity is not
    // observable there, so it is carried as a literal.
    if let ExprNode::Send { method, recv, args, .. } = &mut *expr.node
        && matches!(method.as_str(), "==" | "!=")
    {
        for operand in recv.iter_mut().chain(args.iter_mut()) {
            if let ExprNode::Var { name, .. } = &*operand.node
                && !own.contains(name)
                && let Some(Val::Str(text)) = env.get(name.as_str())
            {
                let literal = ExprNode::Lit { value: Literal::Str { value: text.clone() } };
                *operand = Expr::new(operand.span, literal);
            }
        }
    }
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
