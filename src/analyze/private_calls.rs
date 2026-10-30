use std::collections::HashMap;

use crate::App;
use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::dialect::{MethodDef, MethodReceiver, MethodVisibility};
use crate::expr::{Expr, ExprNode};
use crate::ident::ClassId;
use crate::ty::Ty;

struct Owners<'a> {
    ancestry: crate::ancestry::Ancestors<'a, crate::ancestry::AppLinks<'a>>,
    defined: HashMap<(ClassId, bool, &'a str), MethodVisibility>,
}

impl<'a> Owners<'a> {
    fn of(app: &'a App) -> Self {
        let mut defined = HashMap::new();
        let mut define = |owner: &'a ClassId, methods: &mut dyn Iterator<Item = &'a MethodDef>| {
            for m in methods {
                let key = (owner.clone(), m.receiver == MethodReceiver::Class, m.name.as_str());
                // Not the first definition: Ruby keeps the later one, visibility included.
                defined.insert(key, m.visibility);
            }
        };
        for model in &app.models {
            define(&model.name, &mut model.methods());
        }
        for lc in app.library_classes.iter().filter(|lc| !lc.is_module) {
            define(&lc.name, &mut lc.methods.iter());
        }
        Owners { ancestry: crate::ancestry::Ancestors::for_app(app), defined }
    }

    fn private(&self, class: &ClassId, class_side: bool, name: &str) -> bool {
        for id in self.ancestry.superclasses(class).into_iter().take(32) {
            if let Some(visibility) = self.defined.get(&(id, class_side, name)) {
                return *visibility == MethodVisibility::Private;
            }
        }
        false
    }
}

pub(crate) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let owners = Owners::of(app);
    let mut out = Vec::new();
    let mut visit = |body: &Expr| walk(body, &owners, &mut out);
    for model in &app.models {
        model.methods().for_each(|m| visit(&m.body));
        model.scopes().for_each(|s| visit(&s.body));
    }
    for controller in &app.controllers {
        controller.actions().for_each(|a| visit(&a.body));
        controller.class_methods().for_each(|m| visit(&m.body));
    }
    for lc in &app.library_classes {
        lc.methods.iter().for_each(|m| visit(&m.body));
    }
    for view in &app.views {
        visit(&view.body);
    }
    out
}

fn walk(expr: &Expr, owners: &Owners<'_>, out: &mut Vec<Diagnostic>) {
    if let ExprNode::Send { recv: Some(recv), method, .. } = &*expr.node
        && !matches!(&*recv.node, ExprNode::SelfRef)
        && let Some(Ty::Class { id, .. }) = &recv.ty
    {
        // Not the receiver's type: `self.class` types as the class it names, the same `Ty` an instance has.
        let class_side = match &*recv.node {
            ExprNode::Const { .. } => true,
            ExprNode::Send { method, args, .. } => method.as_str() == "class" && args.is_empty(),
            _ => false,
        };
        if owners.private(id, class_side, method.as_str()) {
            let kind = DiagnosticKind::SendDispatchFailed { method: method.clone(), recv_ty: Ty::Class { id: id.clone(), args: vec![].into() } };
            out.push(Diagnostic {
                span: expr.span,
                severity: crate::diagnostic::Severity::Error,
                kind,
                message: format!(
                    "private method `{}` called on {}{}: only `self` reaches it",
                    method.as_str(),
                    id.0.as_str(),
                    if class_side { "" } else { " (an instance)" }
                ),
            });
        }
    }
    expr.node.for_each_child(&mut |child| walk(child, owners, out));
}
