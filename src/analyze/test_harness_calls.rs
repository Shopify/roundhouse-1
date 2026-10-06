//! A test body's call to a Minitest method the emitted test harness
//! supplies rather than the transpiled runtime (`capture_io`). Test
//! bodies are not checked for the methods they call, so such a call
//! checked clean and the emitted test failed with `NoMethodError`
//! wherever the harness does not define it. `project` refuses each call
//! on a target whose emitted harness does not declare the method.

use crate::dialect::TestModule;
use crate::expr::{Expr, ExprNode};
use crate::span::Span;
use crate::App;

/// Minitest methods a harness supplies, called on the test itself.
const HARNESS_METHODS: &[&str] = &["capture_io"];

/// The `TestBase` instance methods a Ruby harness source defines.
pub(crate) fn declared_methods(text: &str) -> Vec<String> {
    struct Declared(Vec<String>, bool);
    impl<'pr> ruby_prism::Visit<'pr> for Declared {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            let outer = self.1;
            self.1 = node.constant_path().location().as_slice() == b"TestBase";
            ruby_prism::visit_class_node(self, node);
            self.1 = outer;
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            let outer = self.1;
            self.1 = false;
            ruby_prism::visit_module_node(self, node);
            self.1 = outer;
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            if self.1 && node.receiver().is_none() {
                self.0.push(String::from_utf8_lossy(node.name().as_slice()).into_owned());
            }
        }
    }
    let mut declared = Declared(Vec::new(), false);
    let parsed = ruby_prism::parse(text.as_bytes());
    if parsed.errors().next().is_none() {
        ruby_prism::Visit::visit(&mut declared, &parsed.node());
    }
    declared.0
}

/// The test class's own helpers and those of the test classes it
/// inherits from, which a call reaches before the harness.
fn defines(app: &App, module: &TestModule, method: &str) -> bool {
    let mut current = Some(module);
    for _ in 0..32 {
        let Some(tm) = current else { break };
        if tm.helpers.iter().any(|helper| helper.name.as_str() == method) {
            return true;
        }
        current = tm.parent.as_ref()
            .and_then(|parent| app.test_modules.iter().find(|other| other.name == *parent && other.name != tm.name));
    }
    false
}

fn collect(expr: &Expr, wanted: &[&'static str], out: &mut Vec<(Span, &'static str)>) {
    if let ExprNode::Send { recv, method, .. } = &*expr.node {
        let on_self = recv.as_ref().is_none_or(|recv| matches!(*recv.node, ExprNode::SelfRef));
        if let Some(name) = wanted.iter().find(|name| on_self && **name == method.as_str()) {
            out.push((expr.span, name));
        }
    }
    expr.node.for_each_child(&mut |child| collect(child, wanted, out));
}

/// Each test-body call of a harness-supplied method the test class does
/// not define itself, with the method it names.
pub(crate) fn harness_calls(app: &App) -> Vec<(Span, &'static str)> {
    let mut out = Vec::new();
    for module in &app.test_modules {
        let wanted: Vec<&'static str> = HARNESS_METHODS.iter().copied()
            .filter(|method| !defines(app, module, method))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        if let Some(setup) = &module.setup {
            collect(setup, &wanted, &mut out);
        }
        for method in &module.helpers {
            for param in &method.params {
                if let Some(default) = &param.default {
                    collect(default, &wanted, &mut out);
                }
            }
            collect(&method.body, &wanted, &mut out);
        }
        for test in &module.tests {
            collect(&test.body, &wanted, &mut out);
        }
    }
    out
}
