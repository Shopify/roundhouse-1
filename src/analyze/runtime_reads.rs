//! Reads of names only the Ruby-family runtime declares.
//!
//! The constant resolver indexes every runtime `.rb` file, and the
//! registry types `Rails.event` / `Rails.error` from
//! `runtime/ruby/active_support_reporters.rb`, so these names resolve
//! for target-independent analysis (`check`), and on every target.
//! Only the Ruby-family trees (ruby, jruby, spinel) carry the files that
//! declare them: no strict target's transpile table names either file,
//! and no hand-written strict runtime declares the names. `project`
//! refuses each read on a target whose finished tree does not declare
//! the name, before code naming it is emitted, as it does a test body's
//! harness names (`test_constants`).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::expr::{Expr, ExprNode};
use crate::span::Span;
use crate::ty::Ty;
use crate::App;

/// The runtime files whose names are gated, by source path and the
/// path each takes in a finished Ruby-family tree.
const RUBY_FAMILY_RUNTIME: &[(&str, &str)] = &[
    ("runtime/spinel/active_support_notifications.rb", "runtime/active_support_notifications.rb"),
    ("runtime/ruby/active_support_reporters.rb", "runtime/active_support_reporters.rb"),
    ("runtime/spinel/rails_configuration.rb", "runtime/rails_configuration.rb"),
    ("runtime/spinel/active_support_cache.rb", "runtime/active_support_cache.rb"),
    ("runtime/spinel/active_support_messages.rb", "runtime/active_support_messages.rb"),
];

/// Shared runtime every target carries: `active_support_ext.rb` stands
/// `ActiveSupport::MessageVerifier::InvalidSignature` in for a signed-id
/// rescue, and `security_utils.rb` answers
/// `ActiveSupport::SecurityUtils.secure_compare`. A name either declares
/// stays ungated though a gated port declares it too.
const SHARED_RUNTIME: &[&str] = &["runtime/ruby/active_support_ext.rb", "runtime/ruby/security_utils.rb"];

/// Each gated namespace and the finished-tree file that declares it.
pub(crate) fn ruby_family_runtime_names() -> &'static HashMap<String, &'static str> {
    static NAMES: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let shared: HashSet<String> = SHARED_RUNTIME.iter()
            .flat_map(|source| {
                let text = crate::runtime_files::read_to_string(source).unwrap_or_default();
                super::test_constants::declared_namespaces(&text)
            })
            .collect();
        let mut names = HashMap::new();
        for (source, emitted) in RUBY_FAMILY_RUNTIME {
            let text = crate::runtime_files::read_to_string(source).unwrap_or_default();
            for name in super::test_constants::declared_namespaces(&text) {
                if !shared.contains(&name) {
                    names.insert(name, *emitted);
                }
            }
        }
        names
    })
}

/// Whether a Ruby-family runtime file this module knows declares the
/// namespace: a gated port or the shared runtime beside it. A declared
/// type (`#: ActiveSupport::MessageEncryptor`) naming one is a modeled
/// dependency; whether a target's tree carries it is decided at its
/// reads, as for every other gated name.
pub(crate) fn is_runtime_declared_namespace(name: &str) -> bool {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        RUBY_FAMILY_RUNTIME.iter().map(|(source, _)| *source).chain(SHARED_RUNTIME.iter().copied())
            .flat_map(|source| {
                let text = crate::runtime_files::read_to_string(source).unwrap_or_default();
                super::test_constants::declared_namespaces(&text)
            })
            .collect()
    }).contains(name)
}

/// One read of a gated name.
pub(crate) struct RuntimeRead {
    pub span: Span,
    /// The namespace the read needs declared (`ActiveSupport::EventReporter`
    /// for `Rails.event`).
    pub name: String,
    /// The read as written (`Rails.event`, `ActiveSupport::Notifications`).
    pub shown: String,
    /// Read in a test body, where the Ruby-family test harness may
    /// declare the name too.
    pub in_test: bool,
}

/// Singleton methods a gated runtime file defines that the registry
/// types as no gated class, by receiver, method and the namespace each
/// needs: `ActiveSupport.filter_parameters` needs the reporters file's
/// `ParameterFilter`, and `Rails.configuration` (Untyped, so the config
/// keys chained off it type gradually) needs rails_configuration.rb's
/// `Rails::Configuration`. A lifted key's read was rewritten to
/// `Rails.application.<key>` before this survey, so what is left is a
/// read only that file answers.
const UNTYPED_SINGLETONS: &[(&str, &str, &str)] = &[
    ("ActiveSupport", "filter_parameters", "ActiveSupport::ParameterFilter"),
    ("ActiveSupport", "filter_parameters=", "ActiveSupport::ParameterFilter"),
    ("Rails", "configuration", "Rails::Configuration"),
];

fn written(path: &[crate::ident::Symbol]) -> String {
    path.iter().map(|s| s.as_str()).skip_while(|s| s.is_empty()).collect::<Vec<_>>().join("::")
}

fn class_name(ty: &Option<Ty>) -> Option<&str> {
    match ty {
        Some(Ty::Class { id, .. }) => Some(id.0.as_str()),
        _ => None,
    }
}

fn collect(expr: &Expr, gated: &dyn Fn(&str) -> bool, in_test: bool, out: &mut Vec<RuntimeRead>) {
    match &*expr.node {
        ExprNode::Const { path } => {
            if let Some(name) = class_name(&expr.ty).filter(|name| gated(name)) {
                out.push(RuntimeRead { span: expr.span, name: name.to_string(), shown: written(path), in_test });
            }
        }
        ExprNode::Send { recv: Some(recv), method, .. } => {
            if let ExprNode::Const { path } = &*recv.node {
                if !class_name(&recv.ty).is_some_and(|name| gated(name)) {
                    let receiver = written(path);
                    let needs = class_name(&expr.ty).filter(|name| gated(name)).map(str::to_string).or_else(|| {
                        UNTYPED_SINGLETONS.iter()
                            .find(|(r, m, _)| receiver == *r && *m == method.as_str())
                            .map(|(_, _, name)| name.to_string())
                    });
                    if let Some(name) = needs {
                        out.push(RuntimeRead { span: expr.span, name, shown: format!("{receiver}.{}", method.as_str()), in_test });
                    }
                }
            }
        }
        _ => {}
    }
    expr.node.for_each_child(&mut |child| collect(child, gated, in_test, out));
}

fn test_roots(app: &App, f: &mut impl FnMut(&Expr)) {
    for module in &app.test_modules {
        if let Some(setup) = &module.setup {
            f(setup);
        }
        for test in &module.tests {
            f(&test.body);
        }
        for (_, value) in &module.constants {
            f(value);
        }
        for method in &module.helpers {
            f(&method.body);
            for default in method.params.iter().filter_map(|p| p.default.as_ref()) {
                f(default);
            }
        }
        for class in &module.inner_classes {
            for method in &class.methods {
                f(&method.body);
                for default in method.params.iter().filter_map(|p| p.default.as_ref()) {
                    f(default);
                }
            }
            for (_, value) in &class.constants {
                f(value);
            }
            for call in &class.unknown_calls {
                f(call);
            }
            for initializer in &class.class_ivar_initializers {
                f(initializer);
            }
        }
    }
}

/// Every read of a gated name in the app's emit-bound code, its tests
/// included. A name the app declares itself is the app's, not gated.
pub(crate) fn ruby_family_runtime_reads(app: &App) -> Vec<RuntimeRead> {
    let resolver = app.const_resolver.for_sources(&app.sources);
    let names = ruby_family_runtime_names();
    let gated = |name: &str| names.contains_key(name) && !resolver.has_source_namespace(name);
    let mut tests = Vec::new();
    test_roots(app, &mut |root| collect(root, &gated, true, &mut tests));
    let in_test: HashSet<(Span, String)> = tests.iter().map(|read| (read.span, read.shown.clone())).collect();
    let mut out = Vec::new();
    crate::lower::for_each_emit_body_ref(app, &mut |root| collect(root, &gated, false, &mut out));
    for read in &mut out {
        read.in_test = in_test.contains(&(read.span, read.shown.clone()));
    }
    // A test root the emit survey does not reach is still a test read.
    let seen: HashSet<(Span, String)> = out.iter().map(|read| (read.span, read.shown.clone())).collect();
    out.extend(tests.into_iter().filter(|read| !seen.contains(&(read.span, read.shown.clone()))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gated_names_are_the_ruby_family_runtime_declarations() {
        let names = ruby_family_runtime_names();
        for (name, file) in [
            ("ActiveSupport::Notifications", "runtime/active_support_notifications.rb"),
            ("ActiveSupport::Notifications::Event", "runtime/active_support_notifications.rb"),
            ("ActiveSupport::EventReporter", "runtime/active_support_reporters.rb"),
            ("ActiveSupport::ErrorReporter", "runtime/active_support_reporters.rb"),
            ("ActiveSupport::ParameterFilter", "runtime/active_support_reporters.rb"),
            ("ActiveSupport::ExecutionContext", "runtime/active_support_reporters.rb"),
            ("Rails::Configuration", "runtime/rails_configuration.rb"),
            ("ActiveSupport::Cache::MemoryStore", "runtime/active_support_cache.rb"),
            ("ActiveSupport::Cache::Strategy::LocalCache", "runtime/active_support_cache.rb"),
            ("ActiveSupport::IsolatedExecutionState", "runtime/active_support_cache.rb"),
            ("ActiveSupport::MessageEncryptor", "runtime/active_support_messages.rb"),
            ("ActiveSupport::MessageEncryptor::InvalidMessage", "runtime/active_support_messages.rb"),
            ("ActiveSupport::SecureCompareRotator", "runtime/active_support_messages.rb"),
            ("ActiveSupport::KeyGenerator", "runtime/active_support_messages.rb"),
            ("ActiveSupport::CachingKeyGenerator", "runtime/active_support_messages.rb"),
            ("ActiveSupport::Messages::Rotator", "runtime/active_support_messages.rb"),
        ] {
            assert_eq!(names.get(name).copied(), Some(file), "{name}: {names:?}");
        }
        assert!(!names.contains_key("ActiveSupport"), "{names:?}");
        assert!(!names.contains_key("Rails"), "{names:?}");
        assert!(!names.contains_key("Rails::Application"), "{names:?}");
        // active_support_ext.rb and security_utils.rb declare these for
        // every target.
        assert!(!names.contains_key("ActiveSupport::MessageVerifier"), "{names:?}");
        assert!(!names.contains_key("ActiveSupport::MessageVerifier::InvalidSignature"), "{names:?}");
        assert!(!names.contains_key("ActiveSupport::SecurityUtils"), "{names:?}");
        for name in ["ActiveSupport::MessageVerifier", "ActiveSupport::MessageEncryptor", "ActiveSupport::Cache::MemoryStore"] {
            assert!(is_runtime_declared_namespace(name), "{name}");
        }
        assert!(!is_runtime_declared_namespace("ActiveSupport::MessageVerifiers"));
    }
}
