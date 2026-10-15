//! `validates :attr, http_uri: { .. }` names a validator by key, and Rails
//! `const_get`s `"#{key.camelize}Validator"` from the model, whose
//! ancestors include `ActiveModel::Validations` and
//! `ActiveRecord::Validations`: Core's `HttpUriValidator` is declared as
//! `ActiveModel::Validations::HttpUriValidator` and answers there. The
//! ingest writes the bare name (it knows no other class), and a compiled
//! model includes neither module, so a bare read raised `NameError:
//! uninitialized constant ApiClient::HttpUriValidator` at the first
//! `valid?`. In the `validate` of a model, a bare `XValidator` that no
//! class of the app declares at the top level, but the app declares in one
//! of those two modules, is read at that full name.

use crate::App;
use crate::dialect::{LibraryClass, MethodReceiver};
use crate::expr::{Expr, ExprNode};
use crate::ident::Symbol;

/// The modules a model's ancestors put in front of `Object`, in Rails' order.
const NAMESPACES: [&str; 2] = ["ActiveModel::Validations", "ActiveRecord::Validations"];

pub(crate) fn apply(lcs: &mut [LibraryClass], app: &App) {
    let declared = |name: &str| {
        let name = name.trim_start_matches("::");
        app.models.iter().any(|m| m.name.0.as_str().trim_start_matches("::") == name)
            || app.library_classes.iter().any(|c| c.name.0.as_str().trim_start_matches("::") == name)
    };
    for lc in lcs.iter_mut() {
        if !app.models.iter().any(|m| m.name == lc.name) {
            continue;
        }
        let Some(validate) = lc.methods.iter_mut().find(|m| m.name.as_str() == "validate" && m.receiver == MethodReceiver::Instance)
        else {
            continue;
        };
        qualify(&mut validate.body, &declared);
    }
}

fn qualify(e: &mut Expr, declared: &impl Fn(&str) -> bool) {
    if let ExprNode::Send { recv: Some(recv), method, .. } = &mut *e.node
        && method.as_str() == "new"
        && let ExprNode::Const { path } = &mut *recv.node
        && let [name] = path.as_slice()
        && name.as_str().ends_with("Validator")
        && !declared(name.as_str())
        && let Some(owner) = NAMESPACES.iter().find(|owner| declared(&format!("{owner}::{name}")))
    {
        let mut full: Vec<Symbol> = owner.split("::").map(Symbol::from).collect();
        full.push(name.clone());
        *path = full;
    }
    e.node.for_each_child_mut(&mut |c| qualify(c, declared));
}
