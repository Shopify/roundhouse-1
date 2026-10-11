//! A bare `new` in a CLASS BODY is the class's own constructor.
//!
//! `self` in a class body — and in a `def self.` method — is the class,
//! so Ruby reads `new(name: "bell")` there as `Sound.new(name: "bell")`.
//! campfire's `Sound::BUILTIN` is fifty-six of them in one array
//! literal, and the emit replayed the bare spelling:
//!
//! ```text
//! app/models/sound.rb:39: unsupported call:
//!   node 95214 (CallNode `new`) recv=-/ty-1 argc=1 arg0ty28
//! ```
//!
//! A receiverless call has no receiver to resolve against, and `new`
//! is not a free function anywhere. The same array with the receiver
//! spelled out compiles and runs on spinel today (probed), which is
//! what makes this ours rather than a compiler gap.
//!
//! Scoped to the two positions where implicit self IS the class:
//! class-body constant initializers (they run at load time, in the
//! class body) and class-side method bodies. An INSTANCE method's bare
//! `new` is a NoMethodError in Ruby too, so there is nothing there to
//! preserve.
//!
//! AND `self` IS NOT ALWAYS THE OWNER. In a class-side method that a
//! subclass INHERITS, `self` at call time is the subclass — that is the
//! whole point of the class-side template method:
//!
//! ```ruby
//! class ActionText::Content::Filter          # campfire, rails_ext/filter.rb
//!   class << self
//!     def apply(content)
//!       filter = new(content)                # `self` is the SUBCLASS
//!       filter.applicable? ? ... : content
//!     end
//!   end
//!   def applicable? = raise NotImplementedError
//! end
//! ```
//!
//! Binding that to the owner made every `ContentFilters::*.apply` build
//! the ABSTRACT BASE, whose `applicable?` raises — and campfire wraps
//! the filter chain in its own `rescue Exception` returning `""`, so
//! every message body rendered EMPTY behind a 200 and a green suite.
//! `scripts/campfire-cable-walk` is what found it.
//!
//! So a class-side method on a class the tree SUBCLASSES is
//! MONOMORPHIZED: a copy lands in every descendant that does not define
//! the name itself, each copy's `new` bound to the class it now lives
//! in, and the original keeps the owner. `Filter.apply` builds a
//! `Filter`; `SanitizeAttributes.apply` builds a `SanitizeAttributes`;
//! neither needs a dispatch at run time.
//!
//! WHY NOT `self.new`, WHICH IS WHAT THE SOURCE SAYS. It was tried, and
//! it is one arity short on spinel: `self.new` with NO arguments
//! compiles and dispatches to the receiver, `self.new(x)` is
//! `unsupported call: CallNode 'new' recv=SelfNode` (filed upstream).
//! Beyond that gap the shape is the wrong one for this pipeline anyway
//! — a class-side virtual call is exactly what the strict targets have
//! no vtable for, and a lowering may not branch on the target. The
//! subclass set of an ingested tree is CLOSED and known here, which is
//! what makes copying available where late binding is not.
//!
//! It costs a copy of the method per descendant. The whole corpus has
//! ONE site (`ActionText::Content::Filter.apply`, four descendants), so
//! the bound is measured rather than assumed — and if a class ever
//! turns up with a large class-side method and many subclasses, this is
//! the pass that will say so.
//!
//! A PRIVATE `new` (`private_class_method :new`, the factory-method
//! idiom) is the exception. Ruby refuses `Owner.new` there, so the bare
//! call is the only working spelling and is left as written, and Ruby's
//! own dispatch on `self` already builds the subclass, so nothing is
//! copied either. See [`private_constructors`].
//!
//! A class some code subclasses AT RUN TIME (`Class.new(Measurable)`)
//! is the other exception: the tree's subclass set is not closed for it.
//! measured 3.2.1's `Measured.build` returns `Class.new(Measurable) do
//! ... end`, and `Measurable.parse` (`new(*Parser.parse_string(string))`)
//! reached as `Measured::Weight.parse` must build a Weight; bound to the
//! owner it built the abstract `Measurable`, whose `unit_system` raises.
//! Its class-side methods keep the bare `new`, so Ruby's dispatch on
//! `self` builds the subclass; a strict target is told so by a warning.
//! A constant initializer still runs in the owner's body and is bound.
//! See [`dynamic_superclasses`].
//!
//! Only `new`. `create` / `create!` are the same shape on a MODEL class
//! body, but they are also association-scope constructors that
//! `lower::scope_chain` already claims at implicit self
//! (`SELF_CONSTRUCTORS`), and no corpus app writes one in a constant.
//! When one does, it belongs beside this — not in a second walk.

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::dialect::{MethodReceiver, ModelBodyItem};
use crate::expr::{Expr, ExprNode};
use crate::ident::Symbol;

pub fn apply_class_body_new_lowering(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let private = private_constructors(app);
    let dynamic = dynamic_superclasses(app);
    let mut subclassed = subclassed_names(app);
    subclassed.retain(|name| !private.contains(name));
    diags.extend(dynamic_subclass_warnings(app, &dynamic));

    // Copies FIRST, while every method body still holds its bare `new`:
    // the rewrite below is what binds a receiver, and a copy taken after
    // it would carry the base's name into the subclass.
    diags.extend(monomorphize_subclassed(app, &subclassed));

    for model in &mut app.models {
        let owner = model.name.0.as_str().to_string();
        if private.contains(&owner) {
            continue;
        }
        for item in &mut model.body {
            match item {
                // A model's constants arrive as replayed class-body
                // code rather than on a field of their own. A constant
                // initializer runs ONCE, in the body of the class that
                // declares it, so `self` there is the owner however many
                // subclasses exist.
                ModelBodyItem::Unknown { expr, .. } => {
                    if matches!(&*expr.node, ExprNode::Assign { target: crate::expr::LValue::Const { .. }, .. }) {
                        rewrite(expr, &owner);
                    }
                }
                ModelBodyItem::Method { method, .. }
                    if matches!(method.receiver, MethodReceiver::Class) && !dynamic.contains(&owner) =>
                {
                    rewrite(&mut method.body, &owner);
                }
                _ => {}
            }
        }
    }
    // A concern's class-side method runs on its includer, and with one
    // includer that class is `self`: campfire's
    // `Opengraph::Metadata::Fetching.from_url` ends in a bare
    // `new attributes.merge(…)` that builds a Metadata, not a Fetching.
    // The analyzer types the body the same way (`App::
    // sole_includer_of_modules`), so the return it declares and the
    // receiver bound here name the same class.
    let sole_includer = app.sole_includer_of_modules();
    for lc in &mut app.library_classes {
        let owner = if lc.is_module {
            sole_includer.get(&lc.name).unwrap_or(&lc.name).0.as_str().to_string()
        } else {
            lc.name.0.as_str().to_string()
        };
        if private.contains(&owner) {
            continue;
        }
        for (_, value) in &mut lc.constants {
            rewrite(value, &owner);
        }
        for m in &mut lc.methods {
            if matches!(m.receiver, MethodReceiver::Class) && !dynamic.contains(&owner) {
                rewrite(&mut m.body, &owner);
            }
        }
    }
    diags
}

/// Every class of the tree that a `Class.new(X)` call names as its
/// superclass, anywhere in a model's or library class's code: `self` in
/// its class-side methods may be a class the tree never declares. The
/// argument resolves by its analyzed type, else from the calling class's
/// namespace outward, as Ruby resolves a constant.
fn dynamic_superclasses(app: &App) -> crate::det::HashSet<String> {
    let classes: crate::det::HashSet<&str> = app
        .models
        .iter()
        .map(|m| m.name.0.as_str())
        .chain(app.library_classes.iter().filter(|lc| !lc.is_module).map(|lc| lc.name.0.as_str()))
        .collect();
    let mut out = crate::det::HashSet::default();
    fn scan(e: &Expr, scope: &str, classes: &crate::det::HashSet<&str>, out: &mut crate::det::HashSet<String>) {
        e.node.for_each_child(&mut |c| scan(c, scope, classes, out));
        let ExprNode::Send { recv: Some(r), method, args, .. } = &*e.node else { return };
        let (ExprNode::Const { path }, Some(arg)) = (&*r.node, args.first()) else { return };
        if method.as_str() != "new" || path.last().map(|s| s.as_str()) != Some("Class") || path.len() > 1 {
            return;
        }
        let ExprNode::Const { path: parent } = &*arg.node else { return };
        if let Some(crate::ty::Ty::Class { id, .. }) = arg.ty.as_ref() {
            if classes.contains(id.0.as_str()) {
                out.insert(id.0.as_str().to_string());
                return;
            }
        }
        let parent = parent.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
        let mut namespace: Vec<&str> = scope.split("::").collect();
        loop {
            let candidate =
                if namespace.is_empty() { parent.clone() } else { format!("{}::{parent}", namespace.join("::")) };
            if classes.contains(candidate.as_str()) {
                out.insert(candidate);
                return;
            }
            if namespace.pop().is_none() {
                return;
            }
        }
    }
    for m in &app.models {
        let scope = m.name.0.as_str();
        for item in &m.body {
            match item {
                ModelBodyItem::Method { method, .. } => scan(&method.body, scope, &classes, &mut out),
                ModelBodyItem::Unknown { expr, .. } => scan(expr, scope, &classes, &mut out),
                _ => {}
            }
        }
    }
    for lc in &app.library_classes {
        let scope = lc.name.0.as_str();
        let exprs = lc.methods.iter().map(|m| &m.body).chain(lc.constants.iter().map(|(_, v)| v));
        for e in exprs.chain(&lc.unknown_calls).chain(&lc.class_ivar_initializers) {
            scan(e, scope, &classes, &mut out);
        }
    }
    out
}

/// A warning for each class-side method of a [`dynamic_superclasses`]
/// class that builds with a bare `new`: it is left bare, which the Ruby
/// family runs as Ruby does and a strict target cannot resolve.
fn dynamic_subclass_warnings(app: &App, dynamic: &crate::det::HashSet<String>) -> Vec<Diagnostic> {
    let model_methods = app.models.iter().flat_map(|m| {
        m.body.iter().filter_map(move |item| match item {
            ModelBodyItem::Method { method, .. } => Some((m.name.0.as_str(), method)),
            _ => None,
        })
    });
    let library_methods =
        app.library_classes.iter().flat_map(|lc| lc.methods.iter().map(move |m| (lc.name.0.as_str(), m)));
    model_methods
        .chain(library_methods)
        .filter(|(owner, m)| {
            dynamic.contains(*owner) && matches!(m.receiver, MethodReceiver::Class) && constructs(&m.body)
        })
        .map(|(owner, m)| {
            let mut d = Diagnostic::unsupported(
                Span::synthetic(),
                None,
                "inherited class-side constructor",
                format!(
                    "`{owner}.{}` builds with a bare `new` and `{owner}` is subclassed at run time \
                     (`Class.new({owner})`), so its `new` keeps no receiver: the ruby family builds \
                     the receiving class as Ruby does, and a strict target cannot resolve it.",
                    m.name.as_str(),
                ),
            );
            d.severity = crate::diagnostic::Severity::Warning;
            d
        })
        .collect()
}

/// Ruby family: a SUBCLASSED model's own class methods get their bare
/// `new` back. [`apply_class_body_new_lowering`] bound it to the owner
/// for every target, and for a model it only reports the subclass case
/// ([`monomorphize_subclassed`]); CRuby has the late binding the strict
/// targets lack, so `self.new` there builds the receiving class as Ruby
/// does. `Markets::Region.find_or_initialize_all` reached as
/// `Markets::Subdivision.find_or_initialize_all` builds Subdivisions, not
/// Regions that fail `validates :type`. The receiver this pass wrote
/// carries the whole call's span; an author-written `Owner.new` constant
/// is a strict sub-range of its call and keeps its class.
pub(crate) fn unbind_subclassed_model_new(lcs: &mut [crate::dialect::LibraryClass], app: &App) {
    let models: crate::det::HashSet<&str> = app.models.iter().map(|m| m.name.0.as_str()).collect();
    let private = private_constructors(app);
    let mut subclassed = subclassed_names(app);
    subclassed.retain(|name| models.contains(name.as_str()) && !private.contains(name));
    fn unbind(expr: &mut Expr, owner: &[Symbol]) {
        if matches!(&*expr.node, ExprNode::MethodDefExpr { .. }) {
            return;
        }
        expr.node.for_each_child_mut(&mut |c| unbind(c, owner));
        let span = expr.span;
        let ExprNode::Send { recv, method, .. } = &mut *expr.node else { return };
        if method.as_str() != "new" {
            return;
        }
        if recv.as_ref().is_some_and(|r| {
            r.span == span && matches!(&*r.node, ExprNode::Const { path } if path[..] == owner[..])
        }) {
            *recv = None;
        }
    }
    for lc in lcs.iter_mut() {
        if !subclassed.contains(lc.name.0.as_str()) {
            continue;
        }
        let owner: Vec<Symbol> = lc.name.0.as_str().split("::").map(Symbol::from).collect();
        for m in &mut lc.methods {
            if m.receiver == MethodReceiver::Class && !m.name_span.is_synthetic() {
                unbind(&mut m.body, &owner);
            }
        }
    }
}

/// Classes whose class-side `new` is private or protected: the nearest
/// `private_class_method :new` / `public_class_method :new` along the
/// superclass chain in this tree decides. A superclass is looked up from
/// the subclass's namespace outward, the way Ruby resolves it.
pub(crate) fn private_constructors(app: &App) -> crate::det::HashSet<String> {
    use crate::dialect::{MethodVisibility, VisibilityOverride};
    use crate::det::HashMap;

    let mut classes: HashMap<&str, (Option<&str>, &[VisibilityOverride])> = HashMap::default();
    for m in &app.models {
        classes.insert(m.name.0.as_str(), (m.parent.as_ref().map(|p| p.0.as_str()), &m.visibility_overrides));
    }
    for lc in app.library_classes.iter().filter(|lc| !lc.is_module) {
        classes.insert(lc.name.0.as_str(), (lc.parent.as_ref().map(|p| p.0.as_str()), &lc.visibility_overrides));
    }
    // A class that defines `self.new` itself and then makes it private
    // (`def self.new(v) … super … end; private_class_method :new`) has no
    // visibility override: the change landed on its own method.
    let mut local_new: HashMap<&str, MethodVisibility> = HashMap::default();
    for m in &app.models {
        for item in &m.body {
            if let ModelBodyItem::Method { method, .. } = item {
                if method.receiver == MethodReceiver::Class && method.name.as_str() == "new" {
                    local_new.insert(m.name.0.as_str(), method.visibility);
                }
            }
        }
    }
    for lc in app.library_classes.iter().filter(|lc| !lc.is_module) {
        if let Some(m) =
            lc.methods.iter().rev().find(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "new")
        {
            local_new.insert(lc.name.0.as_str(), m.visibility);
        }
    }
    let resolve = |child: &str, parent: &str| -> Option<String> {
        let parent = parent.trim_start_matches("::");
        let mut scope: Vec<&str> = child.split("::").collect();
        scope.pop();
        loop {
            let candidate =
                if scope.is_empty() { parent.to_string() } else { format!("{}::{parent}", scope.join("::")) };
            if classes.contains_key(candidate.as_str()) {
                return Some(candidate);
            }
            scope.pop()?;
        }
    };
    let mut out = crate::det::HashSet::default();
    for &name in classes.keys() {
        let mut current = name.to_string();
        let mut seen = crate::det::HashSet::default();
        while seen.insert(current.clone()) {
            let (parent, overrides) = classes[current.as_str()];
            if let Some(visibility) = local_new.get(current.as_str()) {
                if *visibility != MethodVisibility::Public {
                    out.insert(name.to_string());
                }
                break;
            }
            if let Some(o) = overrides
                .iter()
                .rev()
                .find(|o| o.receiver == MethodReceiver::Class && o.name.as_str() == "new")
            {
                if o.visibility != MethodVisibility::Public {
                    out.insert(name.to_string());
                }
                break;
            }
            let Some(next) = parent.and_then(|p| resolve(&current, p)) else { break };
            current = next;
        }
    }
    out
}

/// Copy an inherited class-side method that constructs into each
/// descendant, so the copy's `new` can be bound to the descendant.
///
/// LIBRARY CLASSES ONLY, and a MODEL that needs the same is reported
/// rather than quietly bound to the wrong class. STI is the model shape
/// that would reach it (`Rooms::Open < Room`), no app in the corpus has
/// one, and building a second copy of this walk over `ModelBodyItem`
/// for a case nothing exercises is how a lowering acquires an untested
/// arm. The diagnostic is what stops it being silent.
fn monomorphize_subclassed(
    app: &mut App,
    subclassed: &crate::det::HashSet<String>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for m in &app.models {
        let owner = m.name.0.as_str();
        if !subclassed.contains(owner) {
            continue;
        }
        for item in &m.body {
            let ModelBodyItem::Method { method, .. } = item else { continue };
            if !matches!(method.receiver, MethodReceiver::Class) || !constructs(&method.body) {
                continue;
            }
            let mut d = Diagnostic::unsupported(
                Span::synthetic(),
                None,
                "inherited class-side constructor",
                format!(
                    "`{}.{}` builds with a bare `new` and `{owner}` is subclassed in this \
                     tree, so on the strict targets a subclass calling it gets a `{owner}` where \
                     Ruby would give it a subclass instance (the ruby family keeps the bare \
                     `new`). Only library classes are monomorphized here.",
                    owner,
                    method.name.as_str(),
                ),
            );
            d.severity = crate::diagnostic::Severity::Warning;
            diags.push(d);
        }
    }

    // name -> index, and parent -> children, over library classes.
    let index: crate::det::HashMap<String, usize> = app
        .library_classes
        .iter()
        .enumerate()
        .map(|(i, lc)| (lc.name.0.as_str().to_string(), i))
        .collect();
    let mut children: crate::det::HashMap<String, Vec<String>> =
        crate::det::HashMap::default();
    for lc in &app.library_classes {
        if let Some(p) = &lc.parent {
            children
                .entry(p.0.as_str().to_string())
                .or_default()
                .push(lc.name.0.as_str().to_string());
        }
    }

    // (destination index, the method to add)
    let mut planned: Vec<(usize, crate::dialect::MethodDef)> = Vec::new();
    for lc in &app.library_classes {
        let owner = lc.name.0.as_str().to_string();
        if !subclassed.contains(owner.as_str()) {
            continue;
        }
        for m in &lc.methods {
            if !matches!(m.receiver, MethodReceiver::Class) || !constructs(&m.body) {
                continue;
            }
            plan_copies(&owner, m, &children, &index, &app.library_classes, &mut planned);
        }
    }

    for (idx, mut method) in planned {
        let dest = app.library_classes[idx].name.0.as_str().to_string();
        rewrite(&mut method.body, &dest);
        app.library_classes[idx].methods.push(method);
    }

    diags
}

/// Walk the descendants of `owner`, copying `method` into each one that
/// does not define the name itself.
///
/// A descendant that DOES define it shadows the inherited one, and every
/// class below that descendant inherits the shadow rather than this
/// method — so that subtree is skipped whole rather than walked past.
fn plan_copies(
    owner: &str,
    method: &crate::dialect::MethodDef,
    children: &crate::det::HashMap<String, Vec<String>>,
    index: &crate::det::HashMap<String, usize>,
    classes: &[crate::dialect::LibraryClass],
    planned: &mut Vec<(usize, crate::dialect::MethodDef)>,
) {
    let Some(kids) = children.get(owner) else { return };
    for kid in kids {
        let Some(&i) = index.get(kid.as_str()) else { continue };
        let shadows = classes[i].methods.iter().any(|m| {
            matches!(m.receiver, MethodReceiver::Class) && m.name == method.name
        });
        if shadows {
            continue;
        }
        planned.push((i, method.clone()));
        plan_copies(kid, method, children, index, classes, planned);
    }
}

/// Does this body construct with a bare `new`?
fn constructs(expr: &Expr) -> bool {
    // A `def` in a block (`Class.new do def self.build = new end`) has
    // the other class as its `self`.
    if matches!(&*expr.node, ExprNode::MethodDefExpr { .. }) {
        return false;
    }
    if let ExprNode::Send { recv: None, method, .. } = &*expr.node {
        if method.as_str() == "new" {
            return true;
        }
    }
    let mut found = false;
    expr.node.for_each_child(&mut |c| {
        if constructs(c) {
            found = true;
        }
    });
    found
}

/// Every class in the tree that something else declares as its parent.
///
/// Read off `parent` rather than from an inheritance index, because the
/// question is exactly "could `self` here be something other than the
/// owner", and one recorded superclass edge is the whole of it. A class
/// subclassed only OUTSIDE the tree (a gem's) is not in here and does
/// not need to be: nothing outside the tree calls into it either.
fn subclassed_names(app: &App) -> crate::det::HashSet<String> {
    let mut out = crate::det::HashSet::default();
    for m in &app.models {
        if let Some(p) = &m.parent {
            out.insert(p.0.as_str().to_string());
        }
    }
    for lc in &app.library_classes {
        if let Some(p) = &lc.parent {
            out.insert(p.0.as_str().to_string());
        }
    }
    for c in &app.controllers {
        if let Some(p) = &c.parent {
            out.insert(p.0.as_str().to_string());
        }
    }
    out
}

fn rewrite(expr: &mut Expr, owner: &str) {
    if matches!(&*expr.node, ExprNode::MethodDefExpr { .. }) {
        return;
    }
    expr.node.for_each_child_mut(&mut |c| rewrite(c, owner));
    let ExprNode::Send { recv: recv @ None, method, .. } = &mut *expr.node else { return };
    if method.as_str() != "new" {
        return;
    }
    let span = expr.span;
    let path: Vec<Symbol> = owner.split("::").map(Symbol::from).collect();
    let mut konst = Expr::new(span, ExprNode::Const { path });
    // Receiver identity is independent of the constructor's result: the
    // result may be SelfInstance or an overridden constructor's other class.
    // Later keyword expansion resolves initialize against this receiver.
    konst.ty = Some(crate::ty::Ty::Class {
        id: crate::ident::ClassId(Symbol::from(owner)),
        args: Vec::new(),
    });
    *recv = Some(konst);
}
