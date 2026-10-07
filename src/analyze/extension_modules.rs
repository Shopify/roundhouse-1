//! Library modules whose instance methods only ever run with a class or
//! module object as `self`.
//!
//! `extend M` in a class body makes M's instance methods the class's
//! singleton methods, so inside them `self` is the extending class.
//! Core's macro modules rely on that: `SymbolEnum#symbol_enum` calls
//! `define_method`, `AttributeOptions#valid_options` reads `constants`
//! and `HasImageAttachmentRelation#image_attachment_relation` calls
//! `class_eval`. A hook's own `base.extend(M)`
//! (`def self.included(base)`) extends a class too.
//!
//! When that is the only way M is used, its methods are typed class
//! side, the way a Concern's `module ClassMethods` already is.
//! Including or prepending M, extending a value with it, or an `extend`
//! anywhere else gives `self` an instance, so any such use outside a
//! class body or hook keeps M on the instance side. Those uses are
//! matched by the module's last name segment, which errs toward the
//! instance side.

use crate::app::App;
use crate::det::HashSet;
use crate::dialect::{ControllerBodyItem, MethodDef, MethodReceiver, ModelBodyItem};
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

const MIXINS: [&str; 3] = ["include", "prepend", "extend"];
const HOOKS: [&str; 4] = ["included", "extended", "inherited", "prepended"];

pub(super) fn extension_only_modules(app: &App) -> HashSet<ClassId> {
    let modules: HashSet<&str> = app.library_classes.iter()
        .filter(|lc| lc.is_module)
        .map(|lc| lc.name.0.as_str())
        .collect();
    let mut scan = Scan { modules: &modules, extended: HashSet::default(), barred: HashSet::default() };
    for model in &app.models {
        for item in &model.body {
            match item {
                ModelBodyItem::Unknown { expr, .. } => scan.class_body_call(expr, &model.name),
                ModelBodyItem::Method { method, .. } => scan.method(method, &model.name),
                _ => {}
            }
        }
    }
    for controller in &app.controllers {
        for item in &controller.body {
            match item {
                ControllerBodyItem::Unknown { expr, .. } => scan.class_body_call(expr, &controller.name),
                ControllerBodyItem::ClassMethod { method, .. } => scan.method(method, &controller.name),
                ControllerBodyItem::Action { action, .. } => scan.bar_within(&action.body),
                _ => {}
            }
        }
    }
    for lc in &app.library_classes {
        // Ingest keeps a class-body `include` here, not as a call.
        scan.bar_ids(&lc.includes);
        for call in &lc.unknown_calls {
            scan.class_body_call(call, &lc.name);
        }
        for method in &lc.methods {
            scan.method(method, &lc.name);
        }
        for init in &lc.class_ivar_initializers {
            scan.bar_within(init);
        }
    }
    for tm in &app.test_modules {
        scan.bar_ids(&tm.includes);
        for test in &tm.tests {
            scan.bar_within(&test.body);
        }
        if let Some(setup) = &tm.setup {
            scan.bar_within(setup);
        }
        for helper in &tm.helpers {
            scan.bar_within(&helper.body);
        }
        for lc in &tm.inner_classes {
            scan.bar_ids(&lc.includes);
            for call in lc.unknown_calls.iter().chain(lc.methods.iter().map(|m| &m.body)) {
                scan.bar_within(call);
            }
        }
    }
    let Scan { mut extended, barred, .. } = scan;
    extended.retain(|id| !barred.contains(last_segment(id.0.as_str())));
    extended
}

struct Scan<'a> {
    modules: &'a HashSet<&'a str>,
    extended: HashSet<ClassId>,
    barred: HashSet<String>,
}

impl Scan<'_> {
    /// A call in a class or module body: `extend M` there extends the
    /// class object. Anything nested in it (an `included do` block) is
    /// not this simple shape and only bars.
    fn class_body_call(&mut self, expr: &Expr, owner: &ClassId) {
        if let ExprNode::Send { recv, method, args, block, .. } = &*expr.node
            && method.as_str() == "extend"
            && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        {
            for arg in args {
                self.extend_with(arg, owner);
            }
            if let Some(block) = block {
                self.bar_within(block);
            }
            return;
        }
        self.bar_within(expr);
    }

    /// A method body. In `def self.included(base)` (and the other
    /// hooks), `base.extend(M)` extends the class the hook receives.
    fn method(&mut self, method: &MethodDef, owner: &ClassId) {
        let hook_param = (method.receiver == MethodReceiver::Class
            && HOOKS.contains(&method.name.as_str())
            && method.params.len() == 1)
            .then(|| method.params[0].name.clone());
        self.walk(&method.body, owner, hook_param.as_ref());
    }

    fn bar_ids(&mut self, ids: &[ClassId]) {
        for id in ids {
            self.barred.insert(last_segment(id.0.as_str()).to_string());
        }
    }

    fn bar_within(&mut self, expr: &Expr) {
        self.walk(expr, &ClassId(Symbol::from("")), None);
    }

    fn walk(&mut self, expr: &Expr, owner: &ClassId, hook_param: Option<&Symbol>) {
        if let ExprNode::Send { recv, method, args, .. } = &*expr.node
            && MIXINS.contains(&method.as_str())
        {
            let hook_extend = method.as_str() == "extend"
                && hook_param.is_some_and(|param| {
                    matches!(recv.as_ref().map(|r| &*r.node), Some(ExprNode::Var { name, .. }) if name == param)
                });
            for arg in args {
                if hook_extend {
                    self.extend_with(arg, owner);
                } else if let ExprNode::Const { path } = &*arg.node
                    && let Some(last) = path.last()
                {
                    self.barred.insert(last.as_str().to_string());
                }
            }
        }
        expr.node.for_each_child(&mut |child| self.walk(child, owner, hook_param));
    }

    /// Record the module an `extend` names, resolved from `owner`'s
    /// scope outward. A name that resolves to no library module is
    /// some other kind of module and is not tracked.
    fn extend_with(&mut self, arg: &Expr, owner: &ClassId) {
        let ExprNode::Const { path } = &*arg.node else { return };
        let written = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
        let mut scope: Vec<&str> = owner.0.as_str().split("::").filter(|s| !s.is_empty()).collect();
        loop {
            let candidate = if scope.is_empty() { written.clone() } else { format!("{}::{written}", scope.join("::")) };
            if self.modules.contains(candidate.as_str()) {
                self.extended.insert(ClassId(Symbol::from(candidate.as_str())));
                return;
            }
            if scope.pop().is_none() {
                return;
            }
        }
    }
}

fn last_segment(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}
