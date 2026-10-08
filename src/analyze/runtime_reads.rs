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
    ("runtime/spinel/rails_autoloaders.rb", "runtime/rails_autoloaders.rb"),
    ("runtime/spinel/active_support_cache.rb", "runtime/active_support_cache.rb"),
    ("runtime/spinel/active_support_isolated_execution_state.rb", "runtime/active_support_isolated_execution_state.rb"),
    ("runtime/spinel/active_support_logger.rb", "runtime/active_support_logger.rb"),
    ("runtime/spinel/active_support_callbacks.rb", "runtime/active_support_callbacks.rb"),
    ("runtime/spinel/active_support_descendants_tracker.rb", "runtime/active_support_descendants_tracker.rb"),
    ("runtime/spinel/active_support_rescuable.rb", "runtime/active_support_rescuable.rb"),
    ("runtime/spinel/active_job_callbacks.rb", "runtime/active_job_callbacks.rb"),
    ("runtime/spinel/active_support_mem_cache_store.rb", "runtime/active_support_mem_cache_store.rb"),
    ("runtime/spinel/active_support_testing_time_helpers.rb", "runtime/active_support_testing_time_helpers.rb"),
    ("runtime/spinel/active_support_messages.rb", "runtime/active_support_messages.rb"),
    ("runtime/spinel/active_support_json.rb", "runtime/active_support_json.rb"),
    ("runtime/spinel/active_model_type.rb", "runtime/active_model_type.rb"),
    ("runtime/spinel/active_model_validator.rb", "runtime/active_model_validator.rb"),
    ("runtime/spinel/active_support_backtrace_cleaner.rb", "runtime/active_support_backtrace_cleaner.rb"),
    ("runtime/spinel/active_support_deprecation.rb", "runtime/active_support_deprecation.rb"),
    ("runtime/spinel/active_job_errors.rb", "runtime/active_job_errors.rb"),
    ("runtime/spinel/active_support_tryable.rb", "runtime/active_support_tryable.rb"),
    ("runtime/spinel/active_support_inflector.rb", "runtime/active_support_inflector.rb"),
    ("runtime/spinel/active_support_multibyte_unicode.rb", "runtime/active_support_multibyte_unicode.rb"),
    ("runtime/spinel/active_support_ractors.rb", "runtime/active_support_ractors.rb"),
    // The PostgreSQL CRuby tree with i18n in its lockfile.
    ("runtime/spinel/active_support_i18n_railtie.rb", "runtime/active_support_i18n_railtie.rb"),
    ("runtime/spinel/action_dispatch_session_store.rb", "runtime/action_dispatch_session_store.rb"),
    ("runtime/spinel/action_dispatch_show_exceptions.rb", "runtime/action_dispatch_show_exceptions.rb"),
    ("runtime/spinel/active_record_log_subscriber.rb", "runtime/active_record_log_subscriber.rb"),
    ("runtime/spinel/active_record_associations.rb", "runtime/active_record_associations.rb"),
    ("runtime/spinel/active_model_errors.rb", "runtime/active_model_errors.rb"),
    // The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).
    ("runtime/spinel/user_agent_version.rb", "runtime/user_agent_version.rb"),
    ("runtime/spinel/active_model_naming.rb", "runtime/active_model_naming.rb"),
    ("runtime/spinel/active_model_attribute_methods.rb", "runtime/active_model_attribute_methods.rb"),
    ("runtime/spinel/active_model_serialization.rb", "runtime/active_model_serialization.rb"),
    ("runtime/spinel/active_support_string_inquirer.rb", "runtime/active_support_string_inquirer.rb"),
];

/// Shared runtime every target carries: `active_support_ext.rb` stands
/// `ActiveSupport::MessageVerifier::InvalidSignature` in for a signed-id
/// rescue, and `security_utils.rb` answers
/// `ActiveSupport::SecurityUtils.secure_compare`, and
/// `action_dispatch/session.rb` declares the `ActionDispatch::Session`
/// object the session-store port reopens. A name either declares stays
/// ungated though a gated port declares it too.
const SHARED_RUNTIME: &[&str] =
    &["runtime/ruby/active_support_ext.rb", "runtime/ruby/security_utils.rb", "runtime/ruby/logger.rb", "runtime/ruby/action_dispatch/session.rb"];

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
/// the PostgreSQL tree's runtime/gem_facades.rb requires it.
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

/// The class methods a gated port module gives the class that includes it:
/// the instance methods of its `ClassMethods` module, which the module's
/// `append_features` (ActiveSupport::Concern's, written out in the port)
/// extends the includer with. `ActiveSupport::Callbacks` gives
/// `define_callbacks`, `set_callback`, `skip_callback` and the rest, which
/// an includer's body calls (Core's Plus::Plan::Change).
///
/// With the class methods of the port modules its `append_features`
/// includes in the includer (`base.include M`), and theirs: the
/// ActiveJob::Callbacks port brings ActiveSupport::Callbacks'
/// `set_callback` and ActiveSupport::Rescuable's `rescue_from`, as
/// Rails' ActiveJob::Base does.
pub(crate) fn mixed_in_class_methods(module: &str) -> Option<&'static [String]> {
    static METHODS: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    METHODS.get_or_init(|| {
        /// The enclosing module path; each module's ClassMethods defs;
        /// each module's `base.include` arguments as written; every
        /// module path entered.
        struct Carriers(Vec<String>, HashMap<String, Vec<String>>, Vec<(String, String)>, HashSet<String>);
        impl Carriers {
            fn enter(&mut self, path: &[u8], body: Option<ruby_prism::Node<'_>>) {
                self.0.push(String::from_utf8_lossy(path).trim_start_matches("::").to_string());
                self.3.insert(self.0.join("::"));
                if let Some(body) = body {
                    ruby_prism::Visit::visit(self, &body);
                }
                self.0.pop();
            }
        }
        /// `base.include M, ...` in an `append_features(base)` body.
        struct Includes<'a>(&'a [u8], Vec<String>);
        impl<'pr> ruby_prism::Visit<'pr> for Includes<'_> {
            fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                let on_base = node
                    .receiver()
                    .and_then(|r| r.as_local_variable_read_node())
                    .is_some_and(|r| r.name().as_slice() == self.0);
                if on_base && node.name().as_slice() == b"include" {
                    for arg in node.arguments().iter().flat_map(|a| a.arguments().iter()) {
                        if arg.as_constant_read_node().is_some() || arg.as_constant_path_node().is_some() {
                            self.1.push(String::from_utf8_lossy(arg.location().as_slice()).into_owned());
                        }
                    }
                }
                ruby_prism::visit_call_node(self, node);
            }
        }
        impl<'pr> ruby_prism::Visit<'pr> for Carriers {
            fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
                self.enter(node.constant_path().location().as_slice(), node.body());
            }
            fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
                self.enter(node.constant_path().location().as_slice(), node.body());
            }
            fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
            fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
                if node.receiver().is_some_and(|r| r.as_self_node().is_some())
                    && node.name().as_slice() == b"append_features"
                    && !self.0.is_empty()
                {
                    let base = node
                        .parameters()
                        .and_then(|p| p.requireds().iter().next())
                        .and_then(|p| p.as_required_parameter_node())
                        .map(|p| p.name().as_slice().to_vec());
                    if let (Some(base), Some(body)) = (base, node.body()) {
                        let mut includes = Includes(&base, Vec::new());
                        ruby_prism::Visit::visit(&mut includes, &body);
                        let owner = self.0.join("::");
                        self.2.extend(includes.1.into_iter().map(|written| (owner.clone(), written)));
                    }
                    return;
                }
                if node.receiver().is_some() || self.0.last().map(String::as_str) != Some("ClassMethods") {
                    return;
                }
                let owner = self.0[..self.0.len() - 1].join("::");
                if owner.is_empty() {
                    return;
                }
                let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
                self.1.entry(owner).or_default().push(name);
            }
        }
        let mut carriers = Carriers(Vec::new(), HashMap::default(), Vec::new(), HashSet::default());
        // And the gem runtime ports' (database_documentation's
        // `ModelMetadata::Metadata::ClassMethods`).
        for source in RUBY_FAMILY_RUNTIME.iter().map(|(source, _)| *source).chain(crate::project::gem_port_sources()) {
            let text = crate::runtime_files::read_to_string(source).unwrap_or_default();
            let parsed = ruby_prism::parse(text.as_bytes());
            if parsed.errors().next().is_none() {
                ruby_prism::Visit::visit(&mut carriers, &parsed.node());
            }
        }
        let Carriers(_, own, includes, modules) = carriers;
        // Each written name resolved as Ruby reads it from the owner's
        // nesting: innermost scope first, then outward, then top level.
        let resolve = |owner: &str, written: &str| -> Option<String> {
            if let Some(top) = written.strip_prefix("::") {
                return modules.contains(top).then(|| top.to_string());
            }
            let mut scope = Some(owner);
            while let Some(prefix) = scope {
                let candidate = format!("{prefix}::{written}");
                if modules.contains(&candidate) {
                    return Some(candidate);
                }
                scope = prefix.rsplit_once("::").map(|(prefix, _)| prefix);
            }
            modules.contains(written).then(|| written.to_string())
        };
        let mut deps: HashMap<String, Vec<String>> = HashMap::default();
        for (owner, written) in &includes {
            if let Some(dep) = resolve(owner, written) {
                deps.entry(owner.clone()).or_default().push(dep);
            }
        }
        let mut owners: Vec<&String> = own.keys().chain(deps.keys()).collect();
        owners.sort();
        owners.dedup();
        let mut methods: HashMap<String, Vec<String>> = HashMap::default();
        for owner in owners {
            let mut gathered: Vec<String> = Vec::new();
            let mut pending = vec![owner.clone()];
            let mut seen: HashSet<String> = HashSet::default();
            while let Some(at) = pending.pop() {
                if !seen.insert(at.clone()) {
                    continue;
                }
                for name in own.get(&at).into_iter().flatten() {
                    if !gathered.contains(name) {
                        gathered.push(name.clone());
                    }
                }
                pending.extend(deps.get(&at).into_iter().flatten().rev().cloned());
            }
            methods.insert(owner.clone(), gathered);
        }
        methods
    }).get(module).map(Vec::as_slice)
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
    // `Rails.autoloaders` (Untyped: the loaders' `on_load` blocks type
    // gradually): rails_autoloaders.rb's Zeitwerk port.
    ("Rails", "autoloaders", "Rails::Autoloaders"),
    // `lower::as_json_residue`'s route for an `as_json` no static method
    // answers (`expr::OBJECT_AS_JSON`): active_support_json.rb's table.
    ("ActiveSupport::JSON::Encoding", "as_json", "ActiveSupport::JSON::Encoding"),
];

/// Instance methods only a gated port answers, though the registry types
/// them on a class every tree has: by method, the class the receiver
/// (`Receiver`) or the result (`Result`) is typed as, and the namespace
/// the finished tree must declare. `errors.attribute_names`, `to_hash`,
/// `as_json` and `messages` need the attribute each message was added
/// for (and its short message), which only the CRuby and JRuby trees
/// record (`ActiveModel::ErrorAttributes`); `record.association(:name)`
/// is the association object, which only they carry. A `messages` read
/// `lower::errors_index` grounded is gone by the time this looks.
pub(crate) enum Typed {
    Receiver(&'static str),
    Result(&'static str),
}

pub(crate) const GATED_METHODS: &[(&str, Typed, &str)] = &[
    ("attribute_names", Typed::Receiver("ActiveModel::Errors"), "ActiveModel::ErrorAttributes"),
    ("to_hash", Typed::Receiver("ActiveModel::Errors"), "ActiveModel::ErrorAttributes"),
    ("as_json", Typed::Receiver("ActiveModel::Errors"), "ActiveModel::ErrorAttributes"),
    ("messages", Typed::Receiver("ActiveModel::Errors"), "ActiveModel::ErrorAttributes"),
    ("merge!", Typed::Receiver("ActiveModel::Errors"), "ActiveModel::ErrorAttributes"),
    ("details", Typed::Receiver("ActiveModel::Errors"), "ActiveModel::ErrorAttributes"),
    ("association", Typed::Result("ActiveRecord::Associations::Association"), "ActiveRecord::Associations::Association"),
];

/// The namespace a gated method read needs, if `expr` is one.
fn gated_method(expr: &Expr) -> Option<(&'static str, String)> {
    let ExprNode::Send { recv, method, .. } = &*expr.node else { return None };
    // A call the analyzer routed to the port (`expr::ERRORS_PORT_CALL`):
    // Enumerable over the Errors, and the reads listed below.
    if expr.decisions & crate::expr::ERRORS_PORT_CALL != 0 {
        return Some(("ActiveModel::ErrorAttributes", format!("ActiveModel::Errors#{}", method.as_str())));
    }
    GATED_METHODS.iter().find_map(|(name, typed, needs)| {
        if *name != method.as_str() {
            return None;
        }
        let typed_as = match typed {
            Typed::Receiver(class) => (class, class_name(&recv.as_ref()?.ty)?),
            Typed::Result(class) => (class, class_name(&expr.ty)?),
        };
        if *typed_as.0 != typed_as.1 {
            return None;
        }
        let shown = match recv.as_ref().map(|r| (&*r.node, &r.ty)) {
            Some((_, ty)) => format!("{}#{}", class_name(ty).unwrap_or("Object"), method.as_str()),
            None => format!("#{}", method.as_str()),
        };
        Some((*needs, shown))
    })
}

/// Whether the app's emit-bound code or its tests read
/// `errors.attribute_names`, `to_hash`, `as_json` or `messages`: the CRuby
/// and JRuby trees then record each error message's attribute and short
/// message (`emit::ruby::with_error_attributes`).
pub(crate) fn reads_error_attribute_names(app: &App) -> bool {
    fn walk(e: &Expr, found: &mut bool) {
        if *found {
            return;
        }
        if gated_method(e).is_some_and(|(needs, _)| needs == "ActiveModel::ErrorAttributes") {
            *found = true;
            return;
        }
        e.node.for_each_child(&mut |c| walk(c, found));
    }
    let mut found = false;
    test_roots(app, &mut |root| walk(root, &mut found));
    crate::lower::for_each_emit_body_ref(app, &mut |root| walk(root, &mut found));
    found
}

fn written(path: &[crate::ident::Symbol]) -> String {
    path.iter().map(|s| s.as_str()).skip_while(|s| s.is_empty()).collect::<Vec<_>>().join("::")
}

fn class_name(ty: &Option<Ty>) -> Option<&str> {
    match ty {
        Some(Ty::Class { id, .. }) => Some(id.0.as_str()),
        _ => None,
    }
}

/// The namespace a record's serialization beyond `only:`
/// (`expr::RECORD_SERIALIZATION`) needs: the ActiveModel serialization
/// port, which ActiveRecord::Base includes on the one tree that carries
/// it (runtime/spinel/cruby/active_record_active_model.rb, beside it on
/// the PostgreSQL CRuby tree).
const RECORD_SERIALIZATION_PORT: &str = "ActiveModel::Serializers::JSON";

fn collect(expr: &Expr, gated: &dyn Fn(&str) -> bool, in_test: bool, out: &mut Vec<RuntimeRead>) {
    if let Some((needs, shown)) = gated_method(expr).filter(|(needs, _)| gated(needs)) {
        out.push(RuntimeRead { span: expr.span, name: needs.to_string(), shown, in_test, declaration: false });
    }
    if expr.decisions & crate::expr::RECORD_SERIALIZATION != 0 && gated(RECORD_SERIALIZATION_PORT) {
        // `lower::as_json_residue` may have made the record an argument
        // of `ActiveSupport::JSON::Encoding.as_json`; the method is still
        // the record's.
        let method = match &*expr.node {
            ExprNode::Send { method, .. } if method.as_str() == "serializable_hash" => "serializable_hash",
            _ => "as_json",
        };
        out.push(RuntimeRead {
            span: expr.span,
            name: RECORD_SERIALIZATION_PORT.to_string(),
            shown: format!("ActiveRecord::Base#{method}(options)"),
            in_test,
            declaration: false,
        });
    }
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

pub(crate) fn test_roots(app: &App, f: &mut impl FnMut(&Expr)) {
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
pub(crate) fn ruby_family_runtime_includes(app: &App) -> Vec<(Span, String, &'static str, String)> {
    let resolver = app.const_resolver.for_sources(&app.sources);
    let names = ruby_family_runtime_names();
    let gated = |name: &str| names.contains_key(name) && !resolver.has_source_namespace(name);
    let mut out = Vec::new();
    for class in &app.library_classes {
        for included in class.includes.iter().filter(|m| gated(m.0.as_str())) {
            out.push((Span::synthetic(), class.name.0.to_string(), "includes", included.0.to_string()));
        }
        // A body `extend M` of a port module (Core's `extend
        // ActiveSupport::DescendantsTracker`), which the Ruby family replays
        // (`emit::ruby::library::runtime_mixin_macro_calls`).
        for call in &class.unknown_calls {
            let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { continue };
            let verb = match method.as_str() {
                "extend" => "extends",
                "prepend" => "prepends",
                _ => continue,
            };
            for arg in args {
                if let ExprNode::Const { path } = &*arg.node {
                    let name = written(path);
                    if gated(&name) {
                        out.push((call.span, class.name.0.to_string(), verb, name));
                    }
                }
            }
        }
    }
    for model in &app.models {
        for item in &model.body {
            let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
            let ExprNode::Send { recv: None, method, args, .. } = &*expr.node else { continue };
            let verb = match method.as_str() {
                "include" => "includes",
                "extend" => "extends",
                "prepend" => "prepends",
                _ => continue,
            };
            for arg in args {
                if let ExprNode::Const { path } = &*arg.node {
                    let name = written(path);
                    if gated(&name) {
                        out.push((expr.span, model.name.0.to_string(), verb, name));
                    }
                }
            }
        }
        // ActiveRecord::Base includes ActiveSupport::Callbacks, so a model
        // runs `define_callbacks`/`set_callback` without naming it; the
        // lowered class includes the port (`model_to_library::model_class`).
        const CALLBACKS: &str = "ActiveSupport::Callbacks";
        let named = out.iter().any(|(_, owner, _, name)| owner == model.name.0.as_str() && name == CALLBACKS);
        if !named && gated(CALLBACKS) {
            if let Some(call) = crate::lower::model_to_library::callback_dsl_calls(model, &app.models).first() {
                out.push((call.span, model.name.0.to_string(), "uses", CALLBACKS.to_string()));
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
            ("Rails::Autoloaders", "runtime/rails_autoloaders.rb"),
            ("Rails::Autoloaders::Loader", "runtime/rails_autoloaders.rb"),
            ("ActiveSupport::Cache::MemoryStore", "runtime/active_support_cache.rb"),
            ("ActiveSupport::Cache::Strategy::LocalCache", "runtime/active_support_cache.rb"),
            ("ActiveSupport::IsolatedExecutionState", "runtime/active_support_isolated_execution_state.rb"),
            ("ActiveSupport::LoggerSilence", "runtime/active_support_logger.rb"),
            ("ActiveSupport::LoggerThreadSafeLevel", "runtime/active_support_logger.rb"),
            ("ActiveSupport::Logger::SimpleFormatter", "runtime/active_support_logger.rb"),
            ("ActiveSupport::Callbacks", "runtime/active_support_callbacks.rb"),
            ("ActiveSupport::Callbacks::CallbackChain", "runtime/active_support_callbacks.rb"),
            ("ActiveSupport::DescendantsTracker", "runtime/active_support_descendants_tracker.rb"),
            ("ActiveSupport::Rescuable", "runtime/active_support_rescuable.rb"),
            ("ActiveJob::Callbacks", "runtime/active_job_callbacks.rb"),
            ("ActiveJob::Execution", "runtime/active_job_callbacks.rb"),
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
            ("ActiveSupport::Deprecation", "runtime/active_support_deprecation.rb"),
            ("ActiveSupport::Deprecation::Deprecators", "runtime/active_support_deprecation.rb"),
            ("ActiveSupport::Deprecation::Reporting", "runtime/active_support_deprecation.rb"),
            ("ActiveSupport::DeprecationException", "runtime/active_support_deprecation.rb"),
            ("ActiveJob::EnqueueError", "runtime/active_job_errors.rb"),
            ("ActiveJob::UnknownJobClassError", "runtime/active_job_errors.rb"),
            ("ActiveJob::DeserializationError", "runtime/active_job_errors.rb"),
            ("ActiveJob::DeserializationError::RecordNotFound", "runtime/active_job_errors.rb"),
            ("ActiveJob::SerializationError", "runtime/active_job_errors.rb"),
            ("ActiveSupport::Tryable", "runtime/active_support_tryable.rb"),
            ("ActiveSupport::Inflector", "runtime/active_support_inflector.rb"),
            ("ActiveSupport::Inflector::Inflections", "runtime/active_support_inflector.rb"),
            ("ActiveSupport::Inflector::Inflections::Uncountables", "runtime/active_support_inflector.rb"),
            ("ActiveSupport::Multibyte::Unicode", "runtime/active_support_multibyte_unicode.rb"),
            ("ActionDispatch::Session::AbstractStore", "runtime/action_dispatch_session_store.rb"),
            ("ActionDispatch::Session::AbstractSecureStore", "runtime/action_dispatch_session_store.rb"),
            ("ActionDispatch::Session::Compatibility", "runtime/action_dispatch_session_store.rb"),
            ("ActionDispatch::Session::SessionRestoreError", "runtime/action_dispatch_session_store.rb"),
            ("ActionDispatch::Constants", "runtime/action_dispatch_show_exceptions.rb"),
            ("ActionDispatch::ShowExceptions", "runtime/action_dispatch_show_exceptions.rb"),
            ("ActiveRecord::LogSubscriber", "runtime/active_record_log_subscriber.rb"),
            ("UserAgent::Version", "runtime/user_agent_version.rb"),
            ("ActiveSupport::JSON::Encoding", "runtime/active_support_json.rb"),
            ("ActiveSupport::JSON::Encoding::JSONGemEncoder", "runtime/active_support_json.rb"),
        ] {
            assert_eq!(names.get(name).copied(), Some(file), "{name}: {names:?}");
        }
        assert!(!names.contains_key("ActiveSupport"), "{names:?}");
        assert!(!names.contains_key("Rails"), "{names:?}");
        assert!(!names.contains_key("Rails::Application"), "{names:?}");
        // active_job.rb declares these for every target; the callbacks
        // port adds modules beside Base and does not reopen it.
        assert!(!names.contains_key("ActiveJob"), "{names:?}");
        assert!(!names.contains_key("ActiveJob::Base"), "{names:?}");
        // runtime/ruby/action_dispatch/session.rb declares it for every target.
        assert!(!names.contains_key("ActionDispatch::Session"), "{names:?}");
        // active_support_ext.rb and security_utils.rb declare these for
        // every target.
        assert!(!names.contains_key("ActiveSupport::MessageVerifier"), "{names:?}");
        assert!(!names.contains_key("ActiveSupport::MessageVerifier::InvalidSignature"), "{names:?}");
        assert!(!names.contains_key("ActiveSupport::SecurityUtils"), "{names:?}");
        // active_support_ext.rb declares ActiveSupport::JSON (`encode`) for
        // every target; only the JSON port's Encoding is gated.
        assert!(!names.contains_key("ActiveSupport::JSON"), "{names:?}");
        // runtime/ruby/logger.rb declares these for every target.
        assert!(!names.contains_key("ActiveSupport::Logger"), "{names:?}");
        assert!(!names.contains_key("ActiveSupport::TaggedLogging"), "{names:?}");
        for name in ["ActiveSupport::MessageVerifier", "ActiveSupport::MessageEncryptor", "ActiveSupport::Cache::MemoryStore"] {
            assert!(is_runtime_declared_namespace(name), "{name}");
        }
        assert!(!is_runtime_declared_namespace("ActiveSupport::MessageVerifiers"));
    }

    #[test]
    fn the_job_callbacks_port_mixes_in_its_includes_class_methods() {
        let methods = mixed_in_class_methods("ActiveJob::Callbacks").expect("the port's class methods");
        for name in ["before_perform", "around_enqueue", "set_callback", "skip_callback", "define_callbacks", "rescue_from", "perform_later", "perform_now"] {
            assert!(methods.iter().any(|m| m == name), "{name}: {methods:?}");
        }
        let rescuable = mixed_in_class_methods("ActiveSupport::Rescuable").expect("Rescuable's class methods");
        assert!(rescuable.iter().any(|m| m == "rescue_from"), "{rescuable:?}");
        assert!(!rescuable.iter().any(|m| m == "set_callback"), "{rescuable:?}");
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
