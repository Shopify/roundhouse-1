//! Test bodies are typed by the same constant resolver as the app's own
//! code (`test_module::type_test_modules`), which marks a written
//! constant it cannot resolve as an unsupported `constant`. Nothing else
//! read a test body's diagnostics, so an `assert_raises(Missing)` or
//! `rescue Missing` checked clean and the emitted test failed with a
//! `NameError` instead.
//!
//! Two kinds of name resolve in the emitted test where the resolver has
//! no class for them:
//!
//! - A namespaced constant the emitted test harness declares
//!   (`runtime/spinel/test/test_helper.rb`: `ActiveSupport::Notifications`,
//!   `ActiveSupport::Cache::MemoryStore`), read by exactly that path.
//!   The harness's own top-level classes (`TestBase`, `Dom`) are not
//!   Rails names, so a test naming one stays refused.
//! - A class a test class, or a test class it inherits from, declares in
//!   its body. The emit hoists it to file scope under its own name
//!   (`emit::ruby`, inner classes), while the resolver answers the
//!   lexical `WidgetTest::Local` no registry carries. A bare read of
//!   exactly that name reaches the hoisted class unless an app namespace
//!   of the same name is found first from the test's modules, or is
//!   reopened by the hoist (`Shop::Late` for `module Shop`, `Late`).

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::dialect::TestModule;
use crate::expr::{Expr, ExprNode};
use crate::App;

/// The namespaced class and module paths the emitted test harness
/// declares.
fn harness_namespaces() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        struct Declared(Vec<String>, HashSet<String>);
        impl Declared {
            fn enter(&mut self, path: &[u8], body: Option<ruby_prism::Node<'_>>) {
                let path = String::from_utf8_lossy(path).into_owned();
                self.0.push(path);
                let full = self.0.join("::");
                if self.0.len() > 1 || full.contains("::") {
                    self.1.insert(full);
                }
                if let Some(body) = body {
                    ruby_prism::Visit::visit(self, &body);
                }
                self.0.pop();
            }
        }
        impl<'pr> ruby_prism::Visit<'pr> for Declared {
            fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
                self.enter(node.constant_path().location().as_slice(), node.body());
            }
            fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
                self.enter(node.constant_path().location().as_slice(), node.body());
            }
        }
        let mut declared = Declared(Vec::new(), HashSet::new());
        if let Ok(text) = crate::runtime_files::read_to_string("runtime/spinel/test/test_helper.rb") {
            let parsed = ruby_prism::parse(text.as_bytes());
            if parsed.errors().next().is_none() {
                ruby_prism::Visit::visit(&mut declared, &parsed.node());
            }
        }
        declared.1
    })
}

struct Scope<'a> {
    /// Inner classes of the test class and the test classes it inherits from.
    hoisted: Vec<&'a str>,
    /// The modules around the test class, innermost first (`Shop`).
    enclosing: Vec<String>,
}

impl Scope<'_> {
    fn reaches_hoisted(&self, name: &str, app_namespace: &dyn Fn(&str) -> bool) -> bool {
        self.hoisted.contains(&name)
            && !app_namespace(name)
            && !self.enclosing.iter().any(|module| app_namespace(&format!("{module}::{name}")))
    }
}

fn scope<'a>(app: &'a App, module: &'a TestModule) -> Scope<'a> {
    let mut hoisted = Vec::new();
    let mut current = Some(module);
    for _ in 0..32 {
        let Some(tm) = current else { break };
        hoisted.extend(tm.inner_classes.iter().map(|inner| inner.name.0.as_str()));
        current = tm.parent.as_ref()
            .and_then(|parent| app.test_modules.iter().find(|other| other.name == *parent && other.name != tm.name));
    }
    let segments: Vec<&str> = module.name.0.as_str().split("::").collect();
    let enclosing = (1..segments.len()).rev().map(|n| segments[..n].join("::")).collect();
    Scope { hoisted, enclosing }
}

fn collect(expr: &Expr, scope: &Scope<'_>, app_namespace: &dyn Fn(&str) -> bool, out: &mut Vec<Diagnostic>) {
    if let Some(DiagnosticKind::Unsupported { target, construct, detail }) = &expr.diagnostic {
        let provided = match &*expr.node {
            ExprNode::Const { path } if path.len() == 1 => scope.reaches_hoisted(path[0].as_str(), app_namespace),
            ExprNode::Const { path } => {
                let written: Vec<&str> = path.iter().map(|s| s.as_str()).skip_while(|s| s.is_empty()).collect();
                harness_namespaces().contains(&written.join("::"))
            }
            _ => false,
        };
        if construct.as_str() == "constant" && !provided {
            out.push(Diagnostic::unsupported(expr.span, target.clone(), construct.as_str(), detail.clone()));
        }
    }
    expr.node.for_each_child(&mut |child| collect(child, scope, app_namespace, out));
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let resolver = app.const_resolver.for_sources(&app.sources);
    let app_namespace = |name: &str| resolver.has_source_namespace(name);
    let mut out = Vec::new();
    for module in &app.test_modules {
        let scope = scope(app, module);
        for (_, value) in &module.constants {
            collect(value, &scope, &app_namespace, &mut out);
        }
        if let Some(setup) = &module.setup {
            collect(setup, &scope, &app_namespace, &mut out);
        }
        for method in &module.helpers {
            for param in &method.params {
                if let Some(default) = &param.default {
                    collect(default, &scope, &app_namespace, &mut out);
                }
            }
            collect(&method.body, &scope, &app_namespace, &mut out);
        }
        for test in &module.tests {
            collect(&test.body, &scope, &app_namespace, &mut out);
        }
    }
    out
}
