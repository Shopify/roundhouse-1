//! Refuse an `unscope` the runtime cannot honour.
//!
//! The shared `Relation#unscope` follows Rails' `unscope!`: the unscoping
//! symbols (`unscope(:order)`, `unscope(:where)`, `unscope(:left_joins)` ...)
//! and `unscope(where: <column>)` / `unscope(where: [<column>, ...])`, the
//! column a symbol, a string (`"table.col"`) or an expression yielding one,
//! with Rails' `ArgumentError` for anything else it is given. What it cannot
//! answer is a call with no argument: Rails raises for it, the runtime
//! would remove nothing. A removed error is a claim the program runs, so
//! that shape is an Error here rather than a late surprise.

use crate::app::App;
use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::expr::{Expr, ExprNode};
use crate::ident::Symbol;

pub fn apply_unscope_check(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let seen: std::cell::RefCell<Vec<crate::span::Span>> = std::cell::RefCell::new(Vec::new());
    super::for_each_hook_body_ref(app, &mut |body| {
        let _ = crate::lower::controller_to_library::util::map_expr(body, &|e| {
            if let ExprNode::Send { method, args, .. } = &*e.node {
                if method.as_str() == "unscope" && !is_supported(args) {
                    seen.borrow_mut().push(e.span);
                }
            }
            None
        });
    });
    for span in seen.borrow().iter() {
        let kind = DiagnosticKind::Unsupported {
            target: None,
            construct: Symbol::from("unscope"),
            detail: "unscope needs an argument".into(),
        };
        out.push(Diagnostic {
            span: *span,
            severity: Diagnostic::default_severity(&kind),
            kind,
            message: "`unscope` with no argument raises in Rails and would remove nothing here; \
                      name what to unscope (`unscope(:order)`, `unscope(where: :column)`)"
                .into(),
        });
    }
    out
}

/// `unscope` with no argument names nothing to unscope. Every other call
/// is the runtime's to judge: it answers Rails' unscoping symbols and
/// `where:` columns, and raises Rails' `ArgumentError` for an invalid
/// symbol, a hash key other than `where:` or another object.
fn is_supported(args: &[Expr]) -> bool {
    !args.is_empty()
}
