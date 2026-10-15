//! A Concern's `included do` block, run on a controller that includes it.
//!
//! ```ruby
//! module AccessAndAuth::Admin::IdentityAwareController
//!   extend ActiveSupport::Concern
//!
//!   included do
//!     include IdentityClient::BearerTokenAuthorization
//!     authorized_identity_clients { |auth| unified_admin_auth(auth) }
//!   end
//!
//!   module ClassMethods
//!     def authorized_identity_clients(**options, &block) = authorized_clients(**options, &block)
//!     def inherited(subclass) = subclass.all_authorized_clients.push(*@all_authorized_clients) && super
//!   end
//! end
//! ```
//!
//! `splice_concerns_into_controllers` copies a Concern's instance methods
//! and filters into the controller, which is all the static dispatch
//! reads, and the lowered controller writes no `include`. A Concern whose
//! block calls the class macros its own `ClassMethods` (or a module the
//! block includes) defines ran none of it: `inherited` ran with no
//! `all_authorized_clients`, which only `BearerTokenAuthorization`'s
//! block-included `ClassMethods` define. Core's `AdminAreaController`
//! reaches `IdentityAwareController` through
//! `ActiveSupport.on_load(:admin_area_controller) { include … }`
//! (`load_hook_includes`), and every `Admin::*Controller` raised
//! NoMethodError at class creation.
//!
//! For each controller include whose Concern runs such statements, the
//! controller's lowered class writes the real `include` and, right after
//! it, the statements of the block in Ruby's order (`App::
//! included_block_statements`): a nested `include M` with M's own
//! statements behind it, and a call to a class macro a Concern's
//! `ClassMethods` defines. Filter macros (`skip_before_action`) stay with
//! the filter chain, and a statement of any other shape is not replayed.
//! A Concern a superclass controller already includes is skipped, as
//! `append_features` returns early when `base < self`.

use crate::App;
use crate::det::{HashMap, HashSet};
use crate::dialect::{ControllerBodyItem, ModelBodyItem};
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

use super::library_class::ConcernClassMethodSpans;

pub(super) fn carry(
    app: &mut App,
    carriers: &[ConcernClassMethodSpans],
    framework_shadows: &HashSet<ClassId>,
) {
    if app.concern_model_items.is_empty() || app.controllers.is_empty() {
        return;
    }
    let class_side = super::app::concern_class_method_catalog(&app.library_classes, carriers);
    let mut defined: HashSet<String> = HashSet::default();
    for lc in &app.library_classes {
        defined.insert(lc.name.0.as_str().to_string());
        for (name, _) in &lc.constants {
            defined.insert(format!("{}::{}", lc.name.0.as_str(), name.as_str()));
        }
    }
    defined.extend(app.models.iter().map(|m| m.name.0.as_str().to_string()));
    let module_includes: HashMap<ClassId, Vec<ClassId>> =
        app.library_classes.iter().map(|lc| (lc.name.clone(), lc.includes.clone())).collect();
    // A `class_attribute` in a block `concern_class_attribute::expand`
    // carries is its; every other one runs here, as the macro.
    let carried = super::concern_class_attribute::admitted_carriers(app, carriers, framework_shadows, false);
    let walk = Walk {
        app: &*app,
        class_side: &class_side,
        defined: &defined,
        module_includes: &module_includes,
        carried: &carried,
    };

    // Each controller's own `include`s (a `prepend` is no include), and
    // every Concern its chain reaches.
    let mut direct: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    let mut reached: HashMap<ClassId, HashSet<ClassId>> = HashMap::default();
    for controller in &app.controllers {
        let includes = own_includes(&controller.body);
        let mut seen = HashSet::default();
        for id in &includes {
            walk.reach(id, &mut seen);
        }
        direct.insert(controller.name.clone(), includes);
        reached.insert(controller.name.clone(), seen);
    }
    let parents: HashMap<&ClassId, &ClassId> =
        app.controllers.iter().filter_map(|c| Some((&c.name, c.parent.as_ref()?))).collect();

    let mut planned: Vec<(ClassId, Vec<ClassId>, Vec<(ClassId, Expr)>)> = Vec::new();
    for controller in &app.controllers {
        let mut inherited: HashSet<ClassId> = HashSet::default();
        let mut at = parents.get(&controller.name).copied();
        for _ in 0..32 {
            let Some(parent) = at.take() else { break };
            inherited.extend(reached.get(parent).into_iter().flatten().cloned());
            at = parents.get(parent).copied();
        }
        let mut includes = Vec::new();
        let mut statements = Vec::new();
        let mut seen = inherited.clone();
        for root in direct.get(&controller.name).into_iter().flatten() {
            if inherited.contains(root) || includes.contains(root) {
                continue;
            }
            let mut own = Vec::new();
            walk.run(root, &mut seen, &mut own);
            // A Concern with a `ClassMethods` and no macro to run stays
            // as it was (`extend` alone).
            if own.iter().all(is_extend) {
                continue;
            }
            includes.push(root.clone());
            statements.extend(own.into_iter().map(|expr| (root.clone(), expr)));
        }
        if !statements.is_empty() {
            planned.push((controller.name.clone(), includes, statements));
        }
    }
    for (name, includes, statements) in planned {
        app.controller_runtime_includes.insert(name.clone(), includes);
        app.included_block_statements.entry(name).or_default().extend(statements);
    }
}

fn is_extend(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Send { method, .. } if method.as_str() == "extend")
}

/// The modules a controller body `include`s, by constant, in order.
fn own_includes(body: &[ControllerBodyItem]) -> Vec<ClassId> {
    let mut out = Vec::new();
    for item in body {
        let ControllerBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else { continue };
        if method.as_str() != "include" {
            continue;
        }
        for arg in args {
            if let ExprNode::Const { path } = &*arg.node {
                out.push(crate::analyze::mixin_path_id(path));
            }
        }
    }
    out
}

struct Walk<'a> {
    app: &'a App,
    class_side: &'a HashMap<ClassId, (Vec<crate::dialect::MethodDef>, HashSet<Symbol>)>,
    defined: &'a HashSet<String>,
    module_includes: &'a HashMap<ClassId, Vec<ClassId>>,
    carried: &'a HashSet<ClassId>,
}

impl Walk<'_> {
    fn items(&self, concern: &ClassId) -> &[ModelBodyItem] {
        self.app.concern_model_items.get(concern).map_or(&[][..], Vec::as_slice)
    }

    /// Whether the emitted `id` has a `ClassMethods` module: its nested
    /// `module ClassMethods` or the one a `class_methods do` builds.
    fn has_class_methods_module(&self, id: &ClassId) -> bool {
        let carrier = format!("{}::ClassMethods", id.0.as_str());
        self.app.class_methods_blocks.contains_key(id)
            || self.app.library_classes.iter().any(|lc| lc.is_module && lc.name.0.as_str() == carrier)
    }

    /// The modules `concern`'s block includes, resolved from its scope.
    fn block_includes(&self, concern: &ClassId) -> Vec<ClassId> {
        self.items(concern)
            .iter()
            .flat_map(|item| super::concern_accessors::included_modules(item, concern, self.defined))
            .collect()
    }

    /// The `class_attribute` statements of `concern`'s `included do`
    /// blocks, as written, and the `private :reader` / `protected :reader`
    /// that make a generated reader non-public on the includer.
    fn block_class_attributes(&self, concern: &ClassId) -> Vec<Expr> {
        let mut out = Vec::new();
        for lc in self.app.library_classes.iter().filter(|lc| &lc.name == concern) {
            for call in &lc.unknown_calls {
                let ExprNode::Send { recv: None, method, args, block: Some(block), .. } = &*call.node else {
                    continue;
                };
                if method.as_str() != "included" || !args.is_empty() {
                    continue;
                }
                let ExprNode::Lambda { body, .. } = &*block.node else { continue };
                let statements: Vec<&Expr> = match &*body.node {
                    ExprNode::Seq { exprs } => exprs.iter().collect(),
                    _ => vec![body],
                };
                // The attributes the block declares: `self.rules = []`
                // seeds one, and the macros below it (`deny_frames`
                // reading `rules + [..]`) find nil without it.
                let mut declared: Vec<&str> = Vec::new();
                for stmt in statements {
                    let keep = match &*stmt.node {
                        ExprNode::Send { recv: None, method, args, block: None, .. } if method.as_str() == "class_attribute" => {
                            for arg in args {
                                if let ExprNode::Lit { value: crate::expr::Literal::Sym { value } } = &*arg.node {
                                    declared.push(value.as_str());
                                }
                            }
                            true
                        }
                        ExprNode::Send { recv: None, method, args, block: None, .. } => {
                            matches!(method.as_str(), "private" | "protected")
                                && !args.is_empty()
                                && args.iter().all(|a| matches!(&*a.node, ExprNode::Lit { value: crate::expr::Literal::Sym { .. } }))
                        }
                        // `self.rules = []`: the writer call, on the class itself.
                        ExprNode::Send { recv: Some(recv), method, args, block: None, .. } => {
                            matches!(&*recv.node, ExprNode::SelfRef)
                                && args.len() == 1
                                && method.as_str().strip_suffix('=').is_some_and(|attr| declared.contains(&attr))
                        }
                        _ => false,
                    };
                    if keep {
                        out.push(stmt.clone());
                    }
                }
            }
        }
        out
    }

    /// Every module `id` brings in: its Concern dependencies, itself, and
    /// what its block includes.
    fn reach(&self, id: &ClassId, seen: &mut HashSet<ClassId>) {
        if !seen.insert(id.clone()) {
            return;
        }
        for dependency in self.module_includes.get(id).into_iter().flatten() {
            self.reach(dependency, seen);
        }
        for included in self.block_includes(id) {
            self.reach(&included, seen);
        }
    }

    /// The statements `include id` runs on the includer, in Ruby's order:
    /// the Concern's dependencies first, then its block.
    fn run(&self, id: &ClassId, seen: &mut HashSet<ClassId>, out: &mut Vec<Expr>) {
        if !seen.insert(id.clone()) {
            return;
        }
        for dependency in self.module_includes.get(id).into_iter().flatten() {
            self.run(dependency, seen, out);
        }
        // `append_features` extends the includer with `ClassMethods` before
        // it runs the block, and the emitted module has no Concern to do
        // it (`Concern::ClassMethods` is the nested module or the one a
        // `class_methods do` builds). A Concern with neither (only an
        // `included do` block) has no module to extend with.
        if self.class_side.contains_key(id) && self.has_class_methods_module(id) {
            let mut path: Vec<Symbol> = id.0.as_str().split("::").map(Symbol::from).collect();
            path.push(Symbol::from("ClassMethods"));
            out.push(Expr::new(
                crate::span::Span::synthetic(),
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("extend"),
                    args: vec![Expr::new(crate::span::Span::synthetic(), ExprNode::Const { path })],
                    block: None,
                    parenthesized: false,
                },
            ));
        }
        // The class macros the block can call: those the Concern's own
        // `ClassMethods` define, and those of every module it includes.
        let mut names: HashSet<&str> = HashSet::default();
        let mut reach = HashSet::default();
        self.reach(id, &mut reach);
        for module in &reach {
            if let Some((methods, _)) = self.class_side.get(module) {
                names.extend(methods.iter().map(|m| m.name.as_str()));
            }
        }
        let class_attributes: &[Symbol] = &[];
        // `class_attribute :x, default: …` of a Concern nothing carries:
        // the runtime macro declares it on the includer, as `included do`
        // does in Rails (Core's `ApiVersioningConcern`, reached through
        // `LoginProtection`). Ahead of the block's other statements, which
        // read what it declares.
        if !self.carried.contains(id) {
            for expr in self.block_class_attributes(id) {
                let item = ModelBodyItem::Unknown { expr, leading_comments: vec![], leading_blank_line: false };
                if let ModelBodyItem::Unknown { expr, .. } =
                    super::app::rehome_carried(item, id, id, class_attributes, self.defined)
                {
                    out.push(expr);
                }
            }
        }
        for item in self.items(id) {
            let included = super::concern_accessors::included_modules(item, id, self.defined);
            let ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let replay = !included.is_empty()
                || matches!(&*expr.node, ExprNode::Send { recv, method, .. }
                    if recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
                        && names.contains(method.as_str()));
            if !replay {
                continue;
            }
            if let ModelBodyItem::Unknown { expr, .. } =
                super::app::rehome_carried(item.clone(), id, id, class_attributes, self.defined)
            {
                out.push(expr);
            }
            for module in included {
                self.run(&module, seen, out);
            }
        }
    }
}
