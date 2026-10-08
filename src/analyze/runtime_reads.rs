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
//! harness names (`test_constants`). One file is narrower than the Ruby
//! family: useragent's `UserAgent::Version` rides only on the PostgreSQL
//! CRuby tree's copy of the user_agent port. The gate is the finished
//! tree's files either way.
//!
//! A port's values are gated too (`ActiveSupport::Cache::OPTION_ALIASES`,
//! `Rails::VERSION::STRING`): the analyzer types them from the port's
//! own definition and marks the read (`expr::RUNTIME_VALUE_REF`), and
//! `project` refuses it where no file the finished tree loads assigns it.

use crate::det::{HashMap, HashSet};
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
    ("runtime/spinel/active_support_mem_cache_store.rb", "runtime/active_support_mem_cache_store.rb"),
    ("runtime/spinel/active_support_testing_time_helpers.rb", "runtime/active_support_testing_time_helpers.rb"),
    ("runtime/spinel/active_support_messages.rb", "runtime/active_support_messages.rb"),
    ("runtime/spinel/active_model_type.rb", "runtime/active_model_type.rb"),
    ("runtime/spinel/active_model_validator.rb", "runtime/active_model_validator.rb"),
    ("runtime/spinel/active_support_backtrace_cleaner.rb", "runtime/active_support_backtrace_cleaner.rb"),
    ("runtime/spinel/active_record_log_subscriber.rb", "runtime/active_record_log_subscriber.rb"),
    // The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).
    ("runtime/spinel/user_agent_version.rb", "runtime/user_agent_version.rb"),
];

/// Shared runtime every target carries: `active_support_ext.rb` stands
/// `ActiveSupport::MessageVerifier::InvalidSignature` in for a signed-id
/// rescue, and `security_utils.rb` answers
/// `ActiveSupport::SecurityUtils.secure_compare`. A name either declares
/// stays ungated though a gated port declares it too.
const SHARED_RUNTIME: &[&str] = &["runtime/ruby/active_support_ext.rb", "runtime/ruby/security_utils.rb"];

/// Runtime files whose VALUES are gated though their namespaces are not:
/// railties' Rails::Railtie and Rails::Engine are a lowering's concern
/// (`lower::unported_rails_subclasses`), but `Rails::VERSION::STRING`
/// is a plain read of rails_railtie.rb, which only the PostgreSQL CRuby
/// tree carries.
const VALUE_GATED_RUNTIME: &[(&str, &str)] = &[("runtime/spinel/rails_railtie.rb", "runtime/rails_railtie.rb")];

/// The full names of the constants a runtime file assigns in its class
/// and module bodies (`ActiveSupport::Cache::OPTION_ALIASES`), not the
/// namespaces it opens.
pub(crate) fn declared_constants(text: &str) -> HashSet<String> {
    struct Declared(Vec<String>, HashSet<String>);
    impl Declared {
        fn enter(&mut self, path: &[u8], body: Option<ruby_prism::Node<'_>>) {
            self.0.push(String::from_utf8_lossy(path).trim_start_matches("::").to_string());
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
        // A method body's constant assignment is a SyntaxError; a
        // singleton class opens no constant path.
        fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}
        fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
            if !self.0.is_empty() {
                let mut path = self.0.clone();
                path.push(String::from_utf8_lossy(node.name().as_slice()).into_owned());
                self.1.insert(path.join("::"));
            }
        }
    }
    let mut declared = Declared(Vec::new(), HashSet::default());
    let parsed = ruby_prism::parse(text.as_bytes());
    if parsed.errors().next().is_none() {
        ruby_prism::Visit::visit(&mut declared, &parsed.node());
    }
    declared.1
}

/// The finished tree's `.rb` files that load: everything outside
/// runtime/ (boot.rb, main.rb, app/, test/), and each file those reach
/// by `require_relative`, or by `require` of a feature runtime/features/
/// answers (runtime/gem_facades.rb puts that directory first on the load
/// path). The trees chain their runtime by `require_relative`; a carried
/// file nothing reaches defines nothing. The SQLite CRuby tree carries
/// runtime/rails_railtie.rb with the rest of runtime/spinel/, but only
/// the PostgreSQL tree's boot.rb requires it.
pub(crate) fn loaded_files(files: &[(String, String)]) -> HashSet<String> {
    struct Requires(Vec<(bool, String)>);
    impl<'pr> ruby_prism::Visit<'pr> for Requires {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let name = node.name().as_slice();
            if node.receiver().is_none() && matches!(name, b"require" | b"require_relative") {
                let args: Vec<_> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                if let [arg] = args.as_slice() {
                    if let Some(string) = arg.as_string_node() {
                        let feature = String::from_utf8_lossy(string.unescaped()).into_owned();
                        self.0.push((name == b"require_relative", feature));
                    }
                }
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    fn normalize(path: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
        parts.join("/")
    }
    let rb: HashMap<&str, &str> = files.iter()
        .filter(|(p, _)| p.ends_with(".rb"))
        .map(|(p, text)| (p.as_str(), text.as_str()))
        .collect();
    let mut loaded: HashSet<String> = HashSet::default();
    let mut queue: Vec<String> = rb.keys().filter(|p| !p.starts_with("runtime/")).map(|p| p.to_string()).collect();
    while let Some(path) = queue.pop() {
        if !loaded.insert(path.clone()) {
            continue;
        }
        let Some(text) = rb.get(path.as_str()) else { continue };
        let parsed = ruby_prism::parse(text.as_bytes());
        let mut requires = Requires(Vec::new());
        ruby_prism::Visit::visit(&mut requires, &parsed.node());
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        for (relative, feature) in requires.0 {
            let feature = feature.trim_end_matches(".rb");
            let target = if relative {
                normalize(&format!("{dir}/{feature}.rb"))
            } else {
                format!("runtime/features/{feature}.rb")
            };
            if rb.contains_key(target.as_str()) && !loaded.contains(&target) {
                queue.push(target);
            }
        }
    }
    loaded
}

/// Each gated runtime value and the finished-tree file that assigns it:
/// the constants of the gated ports and of [`VALUE_GATED_RUNTIME`],
/// less any the shared runtime assigns too.
pub(crate) fn ruby_family_runtime_values() -> &'static HashMap<String, &'static str> {
    static VALUES: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    VALUES.get_or_init(|| {
        let shared: HashSet<String> = SHARED_RUNTIME.iter()
            .flat_map(|source| declared_constants(&crate::runtime_files::read_to_string(source).unwrap_or_default()))
            .collect();
        let mut values = HashMap::default();
        for (source, emitted) in RUBY_FAMILY_RUNTIME.iter().chain(VALUE_GATED_RUNTIME) {
            let text = crate::runtime_files::read_to_string(source).unwrap_or_default();
            for name in declared_constants(&text) {
                if !shared.contains(&name) {
                    values.insert(name, *emitted);
                }
            }
        }
        values
    })
}

/// The finished-tree file that declares a gated namespace or assigns a
/// gated value.
pub(crate) fn gated_runtime_file(name: &str) -> Option<&'static str> {
    ruby_family_runtime_names().get(name).or_else(|| ruby_family_runtime_values().get(name)).copied()
}

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
        let mut names = HashMap::default();
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

/// A gated port's constant aliases of its own classes, by the alias's
/// full name: `ActiveRecord::Type::String = ActiveModel::Type::String`
/// in runtime/spinel/active_model_type.rb. An expression read of the
/// alias types as its target, so is gated as the target; a superclass
/// or `include` written with the alias needs the target too.
fn runtime_aliases() -> &'static HashMap<String, String> {
    static ALIASES: OnceLock<HashMap<String, String>> = OnceLock::new();
    ALIASES.get_or_init(|| {
        struct Aliases(Vec<String>, HashMap<String, String>);
        impl Aliases {
            fn enter(&mut self, path: &[u8], body: Option<ruby_prism::Node<'_>>) {
                self.0.push(String::from_utf8_lossy(path).into_owned());
                if let Some(body) = body {
                    ruby_prism::Visit::visit(self, &body);
                }
                self.0.pop();
            }
        }
        impl<'pr> ruby_prism::Visit<'pr> for Aliases {
            fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
                self.enter(node.constant_path().location().as_slice(), node.body());
            }
            fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
                self.enter(node.constant_path().location().as_slice(), node.body());
            }
            fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
                let value = node.value();
                if value.as_constant_path_node().is_some() || value.as_constant_read_node().is_some() {
                    let target = String::from_utf8_lossy(value.location().as_slice());
                    let mut alias = self.0.clone();
                    alias.push(String::from_utf8_lossy(node.name().as_slice()).into_owned());
                    self.1.insert(alias.join("::"), target.trim_start_matches("::").to_string());
                }
            }
        }
        let names = ruby_family_runtime_names();
        let mut aliases = Aliases(Vec::new(), HashMap::default());
        for (source, _) in RUBY_FAMILY_RUNTIME {
            let text = crate::runtime_files::read_to_string(source).unwrap_or_default();
            let parsed = ruby_prism::parse(text.as_bytes());
            if parsed.errors().next().is_none() {
                ruby_prism::Visit::visit(&mut aliases, &parsed.node());
            }
        }
        aliases.1.retain(|_, target| names.contains_key(target.as_str()));
        aliases.1
    })
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
    /// A class's superclass or `include` (`shown` names the class), not
    /// an expression: it has no source span of its own.
    pub declaration: bool,
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
                out.push(RuntimeRead { span: expr.span, name: name.to_string(), shown: written(path), in_test, declaration: false });
            } else if expr.decisions & crate::expr::RUNTIME_VALUE_REF != 0 {
                // The analyzer qualified the read to the port's own name.
                let name = written(path);
                if ruby_family_runtime_values().contains_key(&name) {
                    out.push(RuntimeRead { span: expr.span, shown: name.clone(), name, in_test, declaration: false });
                }
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
                        out.push(RuntimeRead { span: expr.span, name, shown: format!("{receiver}.{}", method.as_str()), in_test, declaration: false });
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
    // A class built on a port's class or module
    // (`class NormalizedStringType < ActiveRecord::Type::String`,
    // `include ActiveRecord::Type::QueryPredicates`) needs it at load.
    for class in &app.library_classes {
        let parent = class.parent.iter().map(|parent| (parent, "<"));
        let includes = class.includes.iter().map(|included| (included, "includes"));
        for (needed, how) in parent.chain(includes) {
            let name = runtime_aliases().get(needed.0.as_str()).map(String::as_str).unwrap_or(needed.0.as_str());
            if gated(name) && !resolver.has_source_namespace(needed.0.as_str()) {
                out.push(RuntimeRead {
                    span: Span::synthetic(),
                    name: name.to_string(),
                    shown: format!("{} {how} {}", class.name.0, needed.0),
                    in_test: false,
                    declaration: true,
                });
            }
        }
    }
    out
}

/// Every `include` / `extend` / `prepend` of a gated module by an app
/// class or model, as (span, including class, module). A library class
/// keeps its includes as names, so its span is synthetic, as the
/// analyzer's own include refusal is.
pub(crate) fn ruby_family_runtime_includes(app: &App) -> Vec<(Span, String, String)> {
    let resolver = app.const_resolver.for_sources(&app.sources);
    let names = ruby_family_runtime_names();
    let gated = |name: &str| names.contains_key(name) && !resolver.has_source_namespace(name);
    let mut out = Vec::new();
    for class in &app.library_classes {
        for included in class.includes.iter().filter(|m| gated(m.0.as_str())) {
            out.push((Span::synthetic(), class.name.0.to_string(), included.0.to_string()));
        }
    }
    for model in &app.models {
        for item in &model.body {
            let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else { continue };
            if !matches!(method.as_str(), "include" | "extend" | "prepend") {
                continue;
            }
            for arg in args {
                if let ExprNode::Const { path } = &*arg.node {
                    let name = written(path);
                    if gated(&name) {
                        out.push((expr.span, model.name.0.to_string(), name));
                    }
                }
            }
        }
    }
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
            ("ActiveSupport::Cache", "runtime/active_support_cache.rb"),
            ("ActiveSupport::Cache::MemCacheStore", "runtime/active_support_mem_cache_store.rb"),
            ("ActiveSupport::Testing::TimeHelpers", "runtime/active_support_testing_time_helpers.rb"),
            ("ActiveSupport::Testing::SimpleStubs", "runtime/active_support_testing_time_helpers.rb"),
            ("ActiveSupport::MessageEncryptor", "runtime/active_support_messages.rb"),
            ("ActiveSupport::MessageEncryptor::InvalidMessage", "runtime/active_support_messages.rb"),
            ("ActiveSupport::SecureCompareRotator", "runtime/active_support_messages.rb"),
            ("ActiveSupport::KeyGenerator", "runtime/active_support_messages.rb"),
            ("ActiveSupport::CachingKeyGenerator", "runtime/active_support_messages.rb"),
            ("ActiveSupport::Messages::Rotator", "runtime/active_support_messages.rb"),
            ("ActiveModel::Type::Integer", "runtime/active_model_type.rb"),
            ("ActiveModel::Type::DateTime", "runtime/active_model_type.rb"),
            ("ActiveModel::Type::Helpers::AcceptsMultiparameterTime", "runtime/active_model_type.rb"),
            ("ActiveRecord::Type::Text", "runtime/active_model_type.rb"),
            ("ActiveRecord::Type::DateTime", "runtime/active_model_type.rb"),
            ("ActiveRecord::Type::Internal::Timezone", "runtime/active_model_type.rb"),
            ("ActiveRecord::Type::QueryPredicates", "runtime/active_model_type.rb"),
            ("ActiveRecord::Type::QueryPredicates::Decorator", "runtime/active_model_type.rb"),
            ("ActiveModel::RangeError", "runtime/active_model_type.rb"),
            ("ActiveSupport::BacktraceCleaner", "runtime/active_support_backtrace_cleaner.rb"),
            ("Rails::BacktraceCleaner", "runtime/active_support_backtrace_cleaner.rb"),
            ("ActiveRecord::LogSubscriber", "runtime/active_record_log_subscriber.rb"),
            ("UserAgent::Version", "runtime/user_agent_version.rb"),
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

    #[test]
    fn the_gated_values_are_the_ports_own_constants() {
        let values = ruby_family_runtime_values();
        for (name, file) in [
            ("ActiveModel::Type::Boolean::FALSE_VALUES", "runtime/active_model_type.rb"),
            ("ActiveSupport::Cache::OPTION_ALIASES", "runtime/active_support_cache.rb"),
            ("ActiveSupport::Cache::DEFAULT_COMPRESS_LIMIT", "runtime/active_support_cache.rb"),
            ("Rails::VERSION::STRING", "runtime/rails_railtie.rb"),
            ("Rails::VERSION::MAJOR", "runtime/rails_railtie.rb"),
        ] {
            assert_eq!(values.get(name).copied(), Some(file), "{name}: {values:?}");
            assert_eq!(gated_runtime_file(name), Some(file), "{name}");
        }
        // Namespaces are not values; a method body assigns none.
        assert!(!values.contains_key("ActiveSupport::Cache"), "{values:?}");
        assert!(!values.contains_key("Rails::VERSION"), "{values:?}");
        let declared = declared_constants(
            "module A\n  X = 1\n  class B\n    Y = [1].freeze\n  end\n  def m; end\nend\nTOP = 1\nmodule C::D\n  Z = 2\nend\n",
        );
        let mut declared: Vec<_> = declared.into_iter().collect();
        declared.sort();
        assert_eq!(declared, ["A::B::Y", "A::X", "C::D::Z"]);
    }

    #[test]
    fn port_aliases_name_their_gated_targets() {
        let aliases = runtime_aliases();
        for (alias, target) in [
            ("ActiveRecord::Type::String", "ActiveModel::Type::String"),
            ("ActiveRecord::Type::Integer", "ActiveModel::Type::Integer"),
            ("ActiveRecord::Type::Value", "ActiveModel::Type::Value"),
            ("ActiveRecord::Type::Boolean", "ActiveModel::Type::Boolean"),
        ] {
            assert_eq!(aliases.get(alias).map(String::as_str), Some(target), "{aliases:?}");
        }
        // An alias of a class no gated port declares is not a port name.
        assert!(!aliases.contains_key("ActiveSupport::MessageEncryptor::OpenSSLCipherError"), "{aliases:?}");
    }
}
