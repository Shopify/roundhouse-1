//! A test body's call to a Minitest method the emitted test harness
//! supplies rather than the transpiled runtime (`capture_io`). Test
//! bodies are not checked for the methods they call, so such a call
//! checked clean and the emitted test failed with `NoMethodError`
//! wherever the harness does not define it. `project` refuses each call
//! on a target whose emitted harness does not declare the method.
//!
//! The assertions the inliner lowers are the other side of it: emitted
//! tests carry them inline at each call, never as methods, so a test
//! class's own override that calls `super` reaches nothing.

use crate::diagnostic::Diagnostic;
use crate::dialect::MethodReceiver;
use crate::expr::{Expr, ExprNode};
use crate::lower::test_module_to_library::inline_assertions::LOWERED;
use crate::span::Span;
use crate::App;

/// Minitest methods a harness supplies, called on the test itself.
const HARNESS_METHODS: &[&str] = &["capture_io"];

/// The instance methods a Ruby harness source defines on the top-level
/// `TestBase` (not `M::TestBase`, not its singleton class).
pub(crate) fn declared_methods(text: &str) -> Vec<String> {
    struct Declared(Vec<String>, Vec<String>);
    impl Declared {
        fn on_test_base(&self) -> bool {
            self.1.join("::") == "TestBase"
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Declared {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.1.push(String::from_utf8_lossy(node.constant_path().location().as_slice()).into_owned());
            ruby_prism::visit_class_node(self, node);
            self.1.pop();
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.1.push(String::from_utf8_lossy(node.constant_path().location().as_slice()).into_owned());
            ruby_prism::visit_module_node(self, node);
            self.1.pop();
        }
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            self.1.push("<< self".to_string());
            ruby_prism::visit_singleton_class_node(self, node);
            self.1.pop();
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            if self.on_test_base() && node.receiver().is_none() {
                self.0.push(String::from_utf8_lossy(node.name().as_slice()).into_owned());
            }
        }
    }
    let mut declared = Declared(Vec::new(), Vec::new());
    let parsed = ruby_prism::parse(text.as_bytes());
    if parsed.errors().next().is_none() {
        ruby_prism::Visit::visit(&mut declared, &parsed.node());
    }
    declared.0
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
            .filter(|method| !module.defines_instance_method(&app.test_modules, method))
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

fn supers(expr: &Expr, out: &mut Vec<Span>) {
    if matches!(&*expr.node, ExprNode::Super { .. }) {
        out.push(expr.span);
    }
    expr.node.for_each_child(&mut |child| supers(child, out));
}

/// Each `super` in a test class's own instance method named for an
/// assertion the inliner lowers: natively it reaches Minitest's, which
/// no emitted test defines.
pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for module in &app.test_modules {
        for method in &module.helpers {
            let name = method.name.as_str();
            if method.receiver != MethodReceiver::Instance || !LOWERED.contains(&name) {
                continue;
            }
            let mut spans = Vec::new();
            supers(&method.body, &mut spans);
            out.extend(spans.into_iter().map(|span| {
                Diagnostic::unsupported(
                    span,
                    None,
                    "assertion_super",
                    format!("`super` from {}#{name} reaches Minitest's {name}, which emitted tests inline at each call rather than define", module.name.0.as_str()),
                )
            }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::declared_methods;

    #[test]
    fn declared_methods_are_the_top_level_test_base_instance_defs() {
        assert_eq!(declared_methods("class TestBase\n  def capture_io; end\nend\n"), ["capture_io"]);
        for text in [
            "module M\n  class TestBase\n    def capture_io; end\n  end\nend\n",
            "class TestBase\n  class << self\n    def capture_io; end\n  end\nend\n",
            "class TestBase\n  def self.capture_io; end\nend\n",
            "class Other\n  def capture_io; end\nend\n",
        ] {
            assert!(declared_methods(text).is_empty(), "{text}");
        }
    }
}
