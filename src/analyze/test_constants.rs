//! Test bodies are typed by the same constant resolver as the app's own
//! code (`test_module::type_test_modules`), which marks a written
//! constant it cannot resolve as an unsupported `constant`. Nothing else
//! read a test body's diagnostics, so an `assert_raises(Missing)` or
//! `rescue Missing` checked clean and the emitted test failed with a
//! `NameError` instead.
//!
//! One name resolves in the emitted test where the resolver has no class
//! for it: a class the test class declares in its body. The emit hoists
//! it to file scope under its own name (`emit::ruby`, inner classes), so
//! the bare read of exactly that name in the test reaches it, while the
//! resolver answers the lexical `WidgetTest::Local` no registry carries.

use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::dialect::TestModule;
use crate::expr::{Expr, ExprNode};
use crate::App;

fn collect(expr: &Expr, module: &TestModule, out: &mut Vec<Diagnostic>) {
    if let Some(DiagnosticKind::Unsupported { target, construct, detail }) = &expr.diagnostic {
        let hoisted = matches!(&*expr.node, ExprNode::Const { path } if path.len() == 1
            && module.inner_classes.iter().any(|inner| inner.name.0.as_str() == path[0].as_str()));
        if construct.as_str() == "constant" && !hoisted {
            out.push(Diagnostic::unsupported(expr.span, target.clone(), construct.as_str(), detail.clone()));
        }
    }
    expr.node.for_each_child(&mut |child| collect(child, module, out));
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for module in &app.test_modules {
        for (_, value) in &module.constants {
            collect(value, module, &mut out);
        }
        if let Some(setup) = &module.setup {
            collect(setup, module, &mut out);
        }
        for method in &module.helpers {
            for param in &method.params {
                if let Some(default) = &param.default {
                    collect(default, module, &mut out);
                }
            }
            collect(&method.body, module, &mut out);
        }
        for test in &module.tests {
            collect(&test.body, module, &mut out);
        }
    }
    out
}
