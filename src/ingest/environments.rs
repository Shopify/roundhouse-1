//! `config/environments/<env>.rb`: the per-environment boot program.
//!
//! Rails `require`s `config/environments/#{Rails.env}.rb` as the first
//! of the application's initializers (railties 8.2.0.alpha engine.rb:584
//! `load_environment_config`, `before: :load_environment_hook`, the first
//! Bootstrap initializer): after config/application.rb's body and
//! Bundler.require, before every railtie's initializers and the app's
//! `config/initializers`. With environment files present and none for
//! `Rails.env`, boot raises (application.rb:791 `missing_environment_file`).
//!
//! EAGER LOAD. Rails decides at boot whether to load every autoloadable
//! file (`Rails::Application::Finisher`'s `eager_load!`) or to load each
//! one when its constant is first referenced. Core writes
//! `config.eager_load = BootLib.test_eager_load?` in test.rb: lazy on a
//! local run, eager on main and in full CI. The emitted app makes the
//! same choice from the same expression, so each environment's
//! assignment is read here as app code ([`ingest_eager_load`]). An
//! assignment the emitted app cannot evaluate on its own is kept as a
//! refusal naming why, and the emitted boot raises it when that
//! environment is selected: under a condition (`if ENV[…]` around the
//! write), or reading a local the environment file computed earlier
//! (Core's development.rb: `config.eager_load = eager_load`).
//!
//! THE PROGRAM ([`collect_programs`]). Every other statement of each
//! environment file is carried, refused or consumed by the
//! `config/initializers` rule (`ingest::initializer_programs`): one
//! holder per file (`RoundhouseEnvironment<Env>.run_environment`), each
//! carried statement guarded (`error[initializer_statement_not_run]`
//! names one that raises and the next runs), deferred blocks
//! (`config.to_prepare`, `config.after_initialize`, `on_load`) carried
//! statement by statement. The emitted boot runs the program `Rails.env`
//! selects where Rails runs it (`project::environment_program_block`).
//! Differences from an initializer:
//!
//! - The body of `Rails.application.configure do … end` is carried in
//!   place, in source order with the file's other statements; its
//!   `self` is the application, so a bare `config` is
//!   `Rails.application.config`. Its locals are the program's locals.
//! - `config.*` writes take the application.rb/initializer path
//!   ([`classify`]): an app key (`config.<key>` that is not framework
//!   configuration, or `config.x.*`) and an applied framework key
//!   (`APPLIED_FRAMEWORK_CONFIG_KEYS`) are carried as writes through the
//!   writer ingest lifts on the `Rails::Application` reopen, so the
//!   environment's value replaces application.rb's, as in Rails; a key
//!   only environment files write is lifted with Rails' unset answer
//!   (`nil` under `x`, NoMethodError otherwise) for an environment that
//!   does not write it. A key a `config/initializers` file also writes is
//!   superseded: that later write is the one Rails ends with. An inert
//!   key (`INERT_FRAMEWORK_CONFIG_KEYS`) is consumed, and
//!   `config.eager_load` is read above. Any other framework write refuses
//!   its statement by name.
//! - `require_relative` resolves against the environment file: a
//!   support file boot already loads whole is a no-op; any other target
//!   is refused, naming the file.
//!
//! Each refused statement is an `initializer_unsupported` gap at its
//! line (`initializers::survey`), and boot names it again
//! (`error[environment_statement_not_carried]`) when it selects that
//! environment.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use ruby_prism::Node;

use crate::app::{App, EagerLoadSetting, EnvironmentProgram, FrameworkLoadHook};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::vfs::Vfs;

use super::initializer_programs::{self as programs, EnvironmentRules, FiredHooks, Inputs, Scan, Verdict};
use super::util::{constant_id_str, flatten_statements};

/// Each `config/environments/<env>.rb` (not a nested helper), sorted,
/// with its environment name.
pub(super) fn environment_files<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<(String, PathBuf)> {
    let env_dir = dir.join("config/environments");
    if !vfs.is_dir(&env_dir) {
        return Vec::new();
    }
    let Ok(mut files) = super::app::read_rb_files(vfs, &env_dir) else {
        return Vec::new();
    };
    files.sort();
    files
        .into_iter()
        .filter(|path| path.parent() == Some(env_dir.as_path()))
        .filter_map(|path| Some((path.file_stem()?.to_str()?.to_string(), path)))
        .collect()
}

pub(super) fn ingest_eager_load<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<EagerLoadSetting> {
    let mut out = Vec::new();
    for (env, path) in environment_files(vfs, dir) {
        let env = env.as_str();
        let Ok(source) = vfs.read(&path) else { continue };
        let file = path.display().to_string();
        super::sources::register(&file, &String::from_utf8_lossy(&source));
        if let Some(value) = read_setting(&source, &file) {
            out.push(EagerLoadSetting { env: env.to_string(), value });
        }
    }
    out
}

fn read_setting(source: &[u8], file: &str) -> Option<Result<crate::expr::Expr, String>> {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let stmts = root
        .as_program_node()
        .map(|p| p.statements().body().iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut found: Option<Result<crate::expr::Expr, String>> = None;
    super::app::walk_config_statements(stmts, None, &mut |stmt, app_param| {
        if let Some(value) = direct_assignment(stmt, app_param) {
            found = Some(if reads_local(&value) {
                Err(format!(
                    "`config.eager_load = {}` reads a local of {file}",
                    slice(source, &value)
                ))
            } else {
                super::expr::ingest_expr(&value, file).map_err(|err| err.to_string())
            });
            return;
        }
        // A write anywhere else in the statement: under a condition or
        // in a block, so whether it runs is not this file's top level.
        if writes_eager_load(stmt) {
            let line = 1 + source[..stmt.location().start_offset()].iter().filter(|b| **b == b'\n').count();
            found = Some(Err(format!("`config.eager_load` is assigned under a condition at {file}:{line}")));
        }
    });
    found
}

/// `config.eager_load = <value>` as a statement of the configure block.
fn direct_assignment<'pr>(
    stmt: &ruby_prism::Node<'pr>,
    app_param: Option<&str>,
) -> Option<ruby_prism::Node<'pr>> {
    let call = stmt.as_call_node()?;
    if super::util::constant_id_str(&call.name()) != "eager_load=" {
        return None;
    }
    let path = super::app::config_path_in(&call.receiver()?, 0, app_param)?;
    if !path.is_empty() {
        return None;
    }
    call.arguments()?.arguments().iter().next()
}

/// A `….eager_load = …` call anywhere inside `node`.
fn writes_eager_load(node: &ruby_prism::Node<'_>) -> bool {
    struct Writes(bool);
    impl<'pr> ruby_prism::Visit<'pr> for Writes {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if super::util::constant_id_str(&node.name()) == "eager_load=" {
                self.0 = true;
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut v = Writes(false);
    ruby_prism::Visit::visit(&mut v, node);
    v.0
}

fn reads_local(node: &ruby_prism::Node<'_>) -> bool {
    struct Locals(bool);
    impl<'pr> ruby_prism::Visit<'pr> for Locals {
        fn visit_local_variable_read_node(&mut self, _: &ruby_prism::LocalVariableReadNode<'pr>) {
            self.0 = true;
        }
    }
    let mut v = Locals(false);
    ruby_prism::Visit::visit(&mut v, node);
    v.0
}

fn slice<'a>(source: &'a [u8], node: &ruby_prism::Node<'_>) -> std::borrow::Cow<'a, str> {
    let loc = node.location();
    String::from_utf8_lossy(&source[loc.start_offset()..loc.end_offset()])
}

/// The singleton method each environment holder carries.
pub const RUN_METHOD: &str = "run_environment";

/// What one `config.*` write does in the emitted app (module doc).
#[derive(Debug, PartialEq)]
pub(super) enum ConfigWrite {
    /// `config.eager_load`: read by [`ingest_eager_load`].
    EagerLoad,
    /// Inert in the emitted app (`INERT_FRAMEWORK_CONFIG_KEYS`).
    Inert,
    /// An app key, lifted to this reader on the `Rails::Application`
    /// reopen.
    AppKey(String),
    /// A framework key the runtime applies, lifted to this reader.
    Applied(String),
    /// Framework configuration the emitted runtime does not apply.
    Framework,
}

/// The config path a statement writes: `config.x.cdn.host = v` →
/// `["x", "cdn", "host"]`, `config.paths["log"] = v` → `["paths"]`,
/// `config.list << v` and `config.list += v` → `["list"]`.
pub(super) fn written_config_path(node: &Node<'_>, app_param: Option<&str>) -> Option<Vec<String>> {
    let path_of = |recv: Option<Node<'_>>| recv.and_then(|r| super::app::config_path_in(&r, 0, app_param));
    if let Some(call) = node.as_call_node() {
        let name = constant_id_str(&call.name());
        let mut path = path_of(call.receiver())?;
        match name {
            "[]=" | "<<" => {}
            n if n.ends_with('=') && !matches!(n, "==" | "!=" | "===" | "<=" | ">=") => {
                path.push(n.trim_end_matches('=').to_string());
            }
            _ => return None,
        }
        return Some(path);
    }
    let (recv, name) = if let Some(w) = node.as_call_operator_write_node() {
        (w.receiver(), constant_id_str(&w.read_name()).to_string())
    } else if let Some(w) = node.as_call_or_write_node() {
        (w.receiver(), constant_id_str(&w.read_name()).to_string())
    } else if let Some(w) = node.as_call_and_write_node() {
        (w.receiver(), constant_id_str(&w.read_name()).to_string())
    } else {
        return None;
    };
    let mut path = path_of(recv)?;
    path.push(name);
    Some(path)
}

/// What `Rails.application.config` answers itself, not through its
/// `method_missing` store (railties 8.2.0.alpha
/// application/configuration.rb:15-30 and :409-642,
/// engine/configuration.rb, railtie/configuration.rb). A write of one is
/// framework configuration: lifting it as an app key would shadow the
/// runtime's own method (`Rails::Engine#paths`) with an unset reader.
const RAILS_CONFIGURATION_METHODS: &[&str] = &[
    // Application::Configuration attr_accessor / attr_reader.
    "allow_concurrency", "asset_host", "assume_ssl", "autoflush_log", "cache_classes", "cache_store",
    "consider_all_requests_local", "console", "eager_load", "exceptions_app", "file_watcher",
    "filter_parameters", "precompile_filter_parameters", "force_ssl", "helpers_paths", "hosts",
    "host_authorization", "logger", "log_formatter", "log_tags", "silence_healthcheck_path",
    "railties_order", "relative_url_root", "ssl_options", "public_file_server", "session_options",
    "time_zone", "reload_classes_only_on_change", "beginning_of_week", "filter_redirect", "x",
    "content_security_policy_report_only", "content_security_policy_nonce_generator",
    "content_security_policy_nonce_directives", "content_security_policy_nonce_auto",
    "require_master_key", "credentials", "disable_sandbox", "sandbox_by_default",
    "add_autoload_paths_to_load_path", "rake_eager_load", "server_timing", "log_file_size",
    "dom_testing_default_html_version", "yjit", "action_on_early_load_hook", "encoding", "api_only",
    "loaded_config_version", "log_level", "broadcast_log_level",
    // Application::Configuration methods.
    "load_defaults", "reloading_enabled?", "enable_reloading", "debug_exception_response_format",
    "paths", "load_database_yaml", "database_configuration", "autoload_lib", "autoload_lib_once",
    "colorize_logging", "secret_key_base", "revision", "session_store", "session_store?",
    "annotations", "content_security_policy", "permissions_policy", "default_log_file",
    // Engine::Configuration.
    "root", "middleware", "javascript_path", "route_set_class", "default_scope", "eager_load_paths",
    "autoload_once_paths", "autoload_paths", "generators", "all_autoload_paths",
    "all_autoload_once_paths", "all_eager_load_paths",
    // Railtie::Configuration.
    "eager_load_namespaces", "watchable_files", "watchable_dirs", "app_middleware", "app_generators",
    "before_configuration", "before_eager_load", "before_initialize", "after_initialize",
    "after_routes_loaded", "to_prepare_blocks", "to_prepare",
];

/// The application.rb/initializer rule for a written config path
/// (`ingest::app::extract_config_assignments`, `config_receiver_path`).
pub(super) fn classify(path: &[String]) -> ConfigWrite {
    if path == ["eager_load"] {
        return ConfigWrite::EagerLoad;
    }
    if super::app::inert_framework_config(path) {
        return ConfigWrite::Inert;
    }
    let dotted = path.join(".");
    if super::app::APPLIED_FRAMEWORK_CONFIG_KEYS.iter().any(|(key, _)| *key == dotted) {
        return ConfigWrite::Applied(super::app::applied_framework_config_reader(&dotted));
    }
    match path {
        [key] if super::app::FRAMEWORK_CONFIG_KEYS.contains(&key.as_str()) => ConfigWrite::Framework,
        [key] if RAILS_CONFIGURATION_METHODS.contains(&key.trim_end_matches('=')) => ConfigWrite::Framework,
        [key] => ConfigWrite::AppKey(key.clone()),
        [x, _, ..] if x == "x" => ConfigWrite::AppKey(path.join("_")),
        _ => ConfigWrite::Framework,
    }
}

/// The railtie default of an applied framework key, for an environment
/// that does not write it: activerecord 8.2.0.alpha railtie.rb:32.
fn applied_default(reader: &str) -> Option<&'static str> {
    match reader {
        "active_record_maintain_test_schema" => Some("true"),
        _ => None,
    }
}

/// Every app key (and applied framework key) an environment file
/// writes, anywhere in it, with the value its reader answers until a
/// program writes it: Rails' unset answer (`nil` under `x`,
/// NoMethodError for any other key, the railtie default for an applied
/// one). The lift gives each a reader and writer on the
/// `Rails::Application` reopen (`ingest::app`), after application.rb's
/// and the initializers', whose readers win.
pub(super) fn written_app_keys<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<(Vec<String>, String)> {
    struct Writes(Vec<Vec<String>>);
    impl<'pr> ruby_prism::Visit<'pr> for Writes {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            self.take(&node.as_node());
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_call_operator_write_node(&mut self, node: &ruby_prism::CallOperatorWriteNode<'pr>) {
            self.take(&node.as_node());
            ruby_prism::visit_call_operator_write_node(self, node);
        }
        fn visit_call_or_write_node(&mut self, node: &ruby_prism::CallOrWriteNode<'pr>) {
            self.take(&node.as_node());
            ruby_prism::visit_call_or_write_node(self, node);
        }
    }
    impl Writes {
        fn take(&mut self, node: &Node<'_>) {
            if let Some(path) = written_config_path(node, None) {
                let lifted = match classify(&path) {
                    ConfigWrite::AppKey(_) => true,
                    ConfigWrite::Applied(reader) => applied_default(&reader).is_some(),
                    _ => false,
                };
                if lifted && !self.0.contains(&path) {
                    self.0.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    for (_, path) in environment_files(vfs, dir) {
        let Ok(source) = vfs.read(&path) else { continue };
        let result = ruby_prism::parse(&source);
        let mut writes = Writes(Vec::new());
        ruby_prism::Visit::visit(&mut writes, &result.node());
        for segments in writes.0 {
            let value = match classify(&segments) {
                ConfigWrite::Applied(reader) => applied_default(&reader).unwrap_or("nil").to_string(),
                _ if segments[0] == "x" => "nil".to_string(),
                _ => format!(
                    "raise NoMethodError, {:?}",
                    format!(
                        "undefined method '{}' for Rails.application.config: config/environments/#{{Rails.env}}.rb does not write it",
                        segments.join("_")
                    )
                ),
            };
            if !out.iter().any(|(p, _)| *p == segments) {
                out.push((segments, value));
            }
        }
    }
    out
}

/// What the lift left for an environment file's config writes.
pub(super) struct LiftedConfig {
    /// Every reader on the `Rails::Application` reopen.
    pub readers: BTreeSet<String>,
    /// App keys a `config/initializers` file writes: Rails runs it after
    /// the environment file, so its value is the one that stands.
    pub superseded: BTreeSet<String>,
}

/// One environment program per `config/environments/<env>.rb` (module
/// doc), after [`programs::collect`] so the framework hooks merge.
pub(super) fn collect_programs<V: Vfs + ?Sized>(
    app: &mut App,
    inputs: &Inputs<'_, V>,
    fired: &FiredHooks,
    lifted: &LiftedConfig,
) {
    let mut used_names: crate::det::HashSet<String> =
        app.library_classes.iter().map(|lc| lc.name.0.as_str().to_string()).collect();
    let mut framework: BTreeMap<String, FrameworkLoadHook> =
        app.framework_load_hooks.drain(..).map(|h| (h.hook.clone(), h)).collect();
    for (env, entry) in environment_files(inputs.vfs, inputs.dir) {
        let Ok(bytes) = inputs.vfs.read(&entry) else { continue };
        let file = entry.display().to_string();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        super::sources::register(&file, &text);
        let rel = programs::relative(inputs.dir, &entry);
        let result = ruby_prism::parse(text.as_bytes());
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        let mut program_out = EnvironmentProgram {
            env: env.clone(),
            file: rel.clone(),
            source: file.clone(),
            holder: None,
            carried: 0,
            consumed: 0,
            refused: Vec::new(),
        };
        let refuse = |out: &mut EnvironmentProgram, start: usize, end: usize, reason: String| {
            out.refused.push((programs::label_range(&rel, &text, start, end), reason.clone()));
            super::initializers::refuse(&file, start, end, reason);
        };
        // The file's statements, with the `configure` block's in place.
        let mut units: Vec<(Node<'_>, bool)> = Vec::new();
        for stmt in program.statements().body().iter() {
            match application_configure(&stmt) {
                Some(Ok(body)) => units.extend(body.into_iter().map(|s| (s, true))),
                Some(Err(reason)) => {
                    let loc = stmt.location();
                    refuse(&mut program_out, loc.start_offset(), loc.end_offset(), reason);
                }
                None => units.push((stmt, false)),
            }
        }
        let mut kept: Vec<Expr> = Vec::new();
        for (stmt, in_configure) in units {
            let loc = stmt.location();
            let (start, end) = (loc.start_offset(), loc.end_offset());
            let line = |offset: usize| text[..offset].matches('\n').count() + 1;
            // A write the statement is: consumed, or carried below.
            if let Some(path) = written_config_path(&stmt, None) {
                match config_verdict(&path, lifted) {
                    Ok(true) => {}
                    Ok(false) => {
                        super::initializers::consume(&file, start, end);
                        program_out.consumed += 1;
                        continue;
                    }
                    Err(reason) => {
                        refuse(&mut program_out, start, end, reason);
                        continue;
                    }
                }
            }
            // Writes inside it: an inert or superseded one becomes `nil`,
            // a refused one refuses the statement.
            let nil_out: RefCell<Vec<(usize, usize)>> = RefCell::new(Vec::new());
            let config_write = |node: &Node<'_>| -> Option<String> {
                let path = written_config_path(node, None)?;
                let loc = node.location();
                match config_verdict(&path, lifted) {
                    Ok(true) => None,
                    Ok(false) => {
                        nil_out.borrow_mut().push((loc.start_offset(), loc.end_offset()));
                        None
                    }
                    Err(reason) => Some(format!("line {}: {reason}", line(loc.start_offset()))),
                }
            };
            let rules = EnvironmentRules { in_configure, config_write: &config_write };
            let verdict = Scan::environment(inputs, fired, &mut framework, rules).statement(&stmt);
            let finish = |expr: &mut Expr| -> Result<(), String> {
                let nil_out = nil_out.borrow();
                programs::walk(expr, &mut |e| {
                    if nil_out.contains(&(e.span.start as usize, e.span.end as usize)) {
                        *e.node = ExprNode::Lit { value: Literal::Nil };
                    }
                });
                if in_configure {
                    programs::walk(expr, &mut application_config);
                }
                rewrite_require_relative(expr, inputs, &entry)?;
                programs::rewrite_requires(expr, inputs)
            };
            match verdict {
                // The adapter hooks a runtime port lifts are read from
                // `config/initializers` only (`ingest::on_load_hooks`).
                Verdict::Lifted => refuse(
                    &mut program_out,
                    start,
                    end,
                    "an adapter load hook the runtime lifts from config/initializers only".into(),
                ),
                Verdict::Refused(reason) => refuse(&mut program_out, start, end, reason),
                Verdict::Whole => match programs::ingest(&stmt, &file) {
                    Ok(mut expr) => match finish(&mut expr) {
                        Ok(()) => {
                            kept.push(programs::guard(expr, &programs::label(&rel, &text, &stmt)));
                            super::initializers::consume(&file, start, end);
                            program_out.carried += 1;
                        }
                        Err(reason) => refuse(&mut program_out, start, end, reason),
                    },
                    Err(reason) => refuse(&mut program_out, start, end, reason),
                },
                Verdict::Deferred { refused: inner_refused, kept: inner_kept } => {
                    if inner_kept.is_empty() {
                        let reason = inner_refused
                            .first()
                            .map(|(_, _, r)| r.clone())
                            .unwrap_or_else(|| "a deferred block with no statements".into());
                        refuse(&mut program_out, start, end, reason);
                        continue;
                    }
                    match programs::ingest(&stmt, &file) {
                        Ok(mut expr) => {
                            let refused_starts: Vec<u32> = inner_refused.iter().map(|(s, _, _)| *s as u32).collect();
                            let labels: BTreeMap<u32, String> = inner_kept
                                .iter()
                                .map(|&(s, e)| (s as u32, programs::label_range(&rel, &text, s, e)))
                                .collect();
                            if let Err(reason) = finish(&mut expr)
                                .and_then(|()| programs::guard_block_body(&mut expr, &refused_starts, &labels))
                            {
                                refuse(&mut program_out, start, end, reason);
                                continue;
                            }
                            for (s, e, reason) in inner_refused {
                                refuse(&mut program_out, s, e, reason);
                            }
                            for &(s, e) in &inner_kept {
                                super::initializers::consume(&file, s, e);
                            }
                            program_out.carried += inner_kept.len();
                            kept.push(programs::guard(expr, &programs::label(&rel, &text, &stmt)));
                        }
                        Err(reason) => refuse(&mut program_out, start, end, reason),
                    }
                }
            }
        }
        if !kept.is_empty() {
            let holder = programs::prefixed_holder_name("RoundhouseEnvironment", &env, &mut used_names);
            if let Ok(mut synth) = crate::runtime_src::parse_methods(&format!("def self.{RUN_METHOD}\n  nil\nend\n")) {
                if let Some(mut method) = synth.pop() {
                    let span = kept[0].span;
                    method.body = Expr::new(span, ExprNode::Seq { exprs: kept });
                    app.library_classes.push(programs::holder_class(&holder, method));
                    program_out.holder = Some(ClassId(Symbol::from(holder.as_str())));
                }
            }
        }
        app.environment_programs.push(program_out);
    }
    app.framework_load_hooks = framework.into_values().collect();
}

/// `Ok(true)`: carried (an app key or applied framework key with a
/// reader); `Ok(false)`: consumed (inert, `eager_load`, superseded);
/// `Err`: why the write cannot run.
fn config_verdict(path: &[String], lifted: &LiftedConfig) -> Result<bool, String> {
    let written = format!("config.{}", path.join("."));
    match classify(path) {
        ConfigWrite::EagerLoad | ConfigWrite::Inert => Ok(false),
        ConfigWrite::AppKey(key) if lifted.superseded.contains(&key) => Ok(false),
        ConfigWrite::AppKey(key) | ConfigWrite::Applied(key) => {
            if lifted.readers.contains(&key) {
                Ok(true)
            } else {
                Err(format!("`{written}` has no reader on the Rails::Application reopen (no config/application.rb)"))
            }
        }
        ConfigWrite::Framework => {
            Err(format!("`{written}` is framework configuration the emitted runtime does not apply"))
        }
    }
}

/// `Rails.application.configure do … end` (or `<X>.application`): the
/// statements of its block, or why they cannot be carried in place.
fn application_configure<'pr>(stmt: &Node<'pr>) -> Option<Result<Vec<Node<'pr>>, String>> {
    let call = stmt.as_call_node()?;
    if constant_id_str(&call.name()) != "configure" || call.arguments().is_some() {
        return None;
    }
    let receiver = call.receiver()?;
    let on_application = receiver
        .as_call_node()
        .is_some_and(|c| constant_id_str(&c.name()) == "application" && c.arguments().is_none() && c.receiver().is_some());
    if !on_application {
        return None;
    }
    let block = call.block()?.as_block_node()?;
    if block.parameters().is_some() {
        return Some(Err("a `configure` block parameter: the block's statements run in place".into()));
    }
    Some(Ok(block.body().map(flatten_statements).unwrap_or_default()))
}

/// A bare `config` (the `configure` block's `self.config`) →
/// `Rails.application.config`.
fn application_config(e: &mut Expr) {
    let ExprNode::Send { recv: recv @ None, method, args, block: None, .. } = &mut *e.node else { return };
    if method.as_str() != "config" || !args.is_empty() {
        return;
    }
    let span = e.span;
    let mut rails = Expr::new(span, ExprNode::Const { path: vec![Symbol::from("Rails")] });
    // Synthesized, not written at the borrowed span: resolve by name.
    rails.decisions |= crate::expr::GENERATED_CONST_REF;
    *recv = Some(Expr::new(
        span,
        ExprNode::Send { recv: Some(rails), method: Symbol::from("application"), args: Vec::new(), block: None, parenthesized: false },
    ));
}

/// `require_relative "p"` against the environment file: a support file
/// boot already loads whole is a no-op; any other target refuses.
fn rewrite_require_relative<V: Vfs + ?Sized>(expr: &mut Expr, inputs: &Inputs<'_, V>, file: &Path) -> Result<(), String> {
    let mut failure: Option<String> = None;
    programs::walk(expr, &mut |e| {
        if failure.is_some() {
            return;
        }
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*e.node else { return };
        if method.as_str() != "require_relative" {
            return;
        }
        let feature = match args.as_slice() {
            [arg] => match &*arg.node {
                ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
                _ => {
                    failure = Some("`require_relative` of a computed name".into());
                    return;
                }
            },
            _ => {
                failure = Some("`require_relative` with other than one argument".into());
                return;
            }
        };
        let rb = if feature.ends_with(".rb") { feature.clone() } else { format!("{feature}.rb") };
        let target = normalize(&file.parent().unwrap_or(Path::new("")).join(&rb));
        let shown = programs::relative(inputs.dir, &target);
        if inputs.loaded_support.contains(&target) {
            *e.node = ExprNode::Lit { value: Literal::Nil };
            return;
        }
        failure = Some(if inputs.vfs.exists(&target) {
            format!("`require_relative {feature:?}` loads {shown}, which boot does not load")
        } else {
            format!("`require_relative {feature:?}`: {shown} is not in the tree")
        });
    });
    match failure {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
