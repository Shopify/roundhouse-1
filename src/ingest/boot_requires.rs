//! What the native boot itself requires: the app files Ruby loads from
//! the boot files the emitted tree does not carry (`config/boot.rb`,
//! `config/application.rb` with its `Bundler.require`,
//! `config/environment.rb`, and the selected `config/environments/
//! <env>.rb`), in the order it loads them ([`App::boot_loads`]).
//!
//! The emitted boot loads a support file's file program here only when
//! native boot does; every other program loads where a file requires
//! it, or when its constant is first referenced, as Zeitwerk loads it
//! (`project::apply_models_autoload`).
//!
//! THE WALK. Each boot file statement by statement, descending into
//! class and module bodies, depth-first, first load wins:
//! * `require "lit"` resolves against the path gems' and gem sources'
//!   `lib/` directories, then the app's `lib/`; `require_relative "lit"`
//!   against the requiring file. A name that resolves to no file of the
//!   tree (a gem, the standard library) is skipped.
//! * `Bundler.require(...)` requires each Gemfile gem whose source the
//!   tree carries (a path gem, a `gem_sources:` gem) and whose
//!   `require:` is not `false`: `lib/<require>.rb` of the gem, the
//!   gem's name by default, as Bundler's autorequire falls back to
//!   (`lib/<name>.rb`, then `lib/<name with / for ->.rb`). A gem inside
//!   `group :a, :b do` loads only in those environments
//!   (`Rails.groups`); `:default` in every one.
//! * `Rails.application.configure do … end`'s block runs as the file
//!   loads, and is walked the same way.
//! * `Rails.load_embedded_engines` requires the `all:` list of
//!   `config/contexts.yml`, as `ingest::initializers` reads it.
//! * A require inside an `if`/`unless` (or a modifier) runs only when
//!   its condition holds; it is recorded as conditional
//!   ([`App::boot_conditional_loads`]). In config/boot.rb,
//!   config/application.rb and config/environment.rb the statement is
//!   the boot program's (below), which runs the condition; elsewhere (an
//!   environment file without a program) the emitted boot does not load
//!   it and names it in the emitted `app/models.rb`.
//!
//! NOT MODELED: a computed require (`ENV["BOOT_PRELOAD"]`'s paths),
//! `$LOAD_PATH` changes the boot files make, Bundler's `:debug` group
//! argument to `Rails.groups`.
//!
//! THE BOOT PROGRAMS ([`boot_programs`]). Native boot runs more of these
//! files than their requires: Core's config/boot.rb writes ENV
//! (`ENV["DISABLE_DATABASE_ENVIRONMENT_CHECK"] = "1"`, the OTEL_* keys),
//! sets the encodings and `Warning[:deprecated]`, and its
//! config/application.rb writes `ENV["POD_IDS"] ||= "0;1" unless
//! Rails.env.production?`, which the shard registry reads when it builds
//! the pods. When a boot file has such a statement, each of
//! config/boot.rb, config/application.rb and config/environment.rb is
//! carried as a file program (`App::file_programs`), loaded first at
//! boot ([`BootLoad`]s ahead of the walk's), its items in source order:
//! * each require the walk follows, as a `ProgramItem::Require` of the
//!   file it resolves to (so the program loads it where native boot
//!   does); `Bundler.require`'s carried gem entries and
//!   `Rails.load_embedded_engines`' engines likewise, a grouped gem
//!   under its environments' condition; a require in a class or module
//!   body at that declaration (the body itself is the application
//!   reopen's: `ingest::app`);
//! * every other statement as Ruby runs it (`ProgramItem::Statement`,
//!   which names one that raises and goes on), conditionals on
//!   `Rails.env` included;
//! * by name, what cannot run (`ProgramItem::NotCarried`, reported as
//!   `file_statement_unsupported` and named again at boot): a computed
//!   require, a `$LOAD_PATH` change, a statement the expression ingest
//!   refuses.
//!
//! Consumed, not carried, because the emitted tree stands in for them:
//! Bundler's process setup (`gem "bundler"`, `require "bundler/setup"`,
//! `require "bootsnap/setup"`, `ENV["BUNDLE_GEMFILE"]`; the tree activates
//! its locked gems itself, runtime/bundled_gems.rb and gem_facades.rb, as
//! `project::boot_require_is_process_machinery` skips them there) and
//! config/environment.rb's `Rails.application.initialize!` (the emitted
//! app/models.rb is that initialization: the environment program, the
//! railties' initializers and config/initializers). A boot file whose
//! statements are only requires and these has no program: the walk's
//! boot loads already load it in its order.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use ruby_prism::Node;

use crate::app::{BootLoad, FileProgram, ProgramItem};
use crate::expr::Expr;
use crate::span::Span;
use crate::vfs::Vfs;

use super::util::{constant_id_str, flatten_statements};

/// `(unconditional loads, conditional loads)`, each in load order.
pub(super) fn boot_loads<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    path_gems: &[PathBuf],
    gem_sources: &[(String, PathBuf)],
) -> (Vec<BootLoad>, Vec<BootLoad>) {
    let mut load_path: Vec<PathBuf> =
        path_gems.iter().map(|gem| gem.join("lib")).filter(|lib| vfs.is_dir(lib)).collect();
    load_path.push(dir.join("lib"));
    let gems = bundler_entries(vfs, dir, path_gems, gem_sources);
    let mut walk = Walk {
        vfs,
        dir,
        load_path,
        gems,
        visited: BTreeSet::new(),
        envs: Vec::new(),
        conditional: false,
        loads: Vec::new(),
        conditional_loads: Vec::new(),
        bundler_require: false,
    };
    for seed in ["config/boot.rb", "config/application.rb", "config/environment.rb"] {
        walk.file(&dir.join(seed), None);
    }
    // Rails loads the selected environment's file during bootstrap:
    // each environment's file walks over the common boot, by itself.
    let env_dir = dir.join("config/environments");
    let mut env_files = super::app::read_rb_files(vfs, &env_dir).unwrap_or_default();
    env_files.sort();
    let common = walk.visited.clone();
    for file in env_files {
        let Some(env) = file.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        walk.visited = common.clone();
        walk.envs = vec![env];
        walk.file(&file, None);
    }
    (walk.loads, walk.conditional_loads)
}

/// The native boot files, in the order a test's `require
/// "config/environment"` runs them.
const BOOT_SEEDS: [&str; 3] = ["config/boot.rb", "config/application.rb", "config/environment.rb"];

/// The boot files' own programs and the [`BootLoad`]s that load them
/// ahead of the walk's (module doc, THE BOOT PROGRAMS): empty when no
/// boot file has a statement beyond its requires. Each program comes
/// with its source, registered for spans.
pub(super) fn boot_programs<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    path_gems: &[PathBuf],
    gem_sources: &[(String, PathBuf)],
) -> (Vec<BootLoad>, Vec<(FileProgram, Vec<u8>)>) {
    let mut load_path: Vec<PathBuf> =
        path_gems.iter().map(|gem| gem.join("lib")).filter(|lib| vfs.is_dir(lib)).collect();
    load_path.push(dir.join("lib"));
    let walk = Walk {
        vfs,
        dir,
        load_path,
        gems: bundler_entries(vfs, dir, path_gems, gem_sources),
        visited: BTreeSet::new(),
        envs: Vec::new(),
        conditional: false,
        loads: Vec::new(),
        conditional_loads: Vec::new(),
        bundler_require: false,
    };
    let mut programs = Vec::new();
    for seed in BOOT_SEEDS {
        let path = dir.join(seed);
        let Ok(source) = vfs.read(&path) else { continue };
        let file = path.display().to_string();
        super::sources::register(&file, &String::from_utf8_lossy(&source));
        let result = super::prism::parse(&source, &file);
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        let program_walk = BootProgram { walk: &walk, path: normalize(&path), file: file.clone(), source: &source };
        let mut items = Vec::new();
        program_walk.items(&program.statements().body().iter().collect::<Vec<_>>(), &mut items);
        programs.push((FileProgram { source: file, items }, source.to_vec()));
    }
    fn runs(items: &[ProgramItem]) -> bool {
        items.iter().any(|item| match item {
            ProgramItem::Statement(_) | ProgramItem::NotCarried { .. } => true,
            ProgramItem::Branch { then_items, else_items, .. } => runs(then_items) || runs(else_items),
            _ => false,
        })
    }
    if !programs.iter().any(|(program, _)| runs(&program.items)) {
        return (Vec::new(), Vec::new());
    }
    let loads = programs
        .iter()
        .map(|(program, _)| {
            let rel = walk.rel(Path::new(&program.source));
            BootLoad { source: rel.clone(), envs: Vec::new(), site: format!("{rel} (native boot)") }
        })
        .collect();
    (loads, programs)
}

/// One boot file's program items (module doc, THE BOOT PROGRAMS).
struct BootProgram<'w, 'a, V: Vfs + ?Sized> {
    walk: &'w Walk<'a, V>,
    /// The boot file, normalized as the walk's targets are.
    path: PathBuf,
    /// The boot file as registered (`sources::register`).
    file: String,
    source: &'w [u8],
}

impl<V: Vfs + ?Sized> BootProgram<'_, '_, V> {
    fn span(&self, node: &Node<'_>) -> Span {
        let loc = node.location();
        Span { file: super::sources::file_id(&self.file), start: loc.start_offset() as u32, end: loc.end_offset() as u32 }
    }

    fn not_carried(&self, node: &Node<'_>, message: impl Into<String>) -> ProgramItem {
        ProgramItem::NotCarried {
            span: self.span(node),
            construct: super::initializers::construct_of(node, &String::from_utf8_lossy(self.source)),
            message: message.into(),
        }
    }

    /// A `ProgramItem::Require` of `target`, a file of the tree, from
    /// this boot file: relative to it, so the emit resolves it to that
    /// file and no other (`program_require_source`).
    fn require_of(&self, node: &Node<'_>, target: &Path) -> ProgramItem {
        let dir = normalize(self.walk.dir);
        let target = normalize(target);
        let base = self.path.parent().unwrap_or(Path::new(""));
        match (target.strip_prefix(&dir), base.strip_prefix(&dir)) {
            (Ok(rel_target), Ok(rel_base)) => {
                let up = rel_base.components().count();
                let mut feature = "../".repeat(up);
                feature.push_str(&rel_target.display().to_string());
                let feature = feature.strip_suffix(".rb").unwrap_or(&feature).to_string();
                ProgramItem::Require { span: self.span(node), feature, relative: true }
            }
            // Outside the app root: the feature under its `lib/`.
            _ => {
                let text = target.display().to_string();
                let feature = text.rsplit_once("/lib/").map(|(_, f)| f).unwrap_or(&text);
                let feature = feature.strip_suffix(".rb").unwrap_or(feature).to_string();
                ProgramItem::Require { span: self.span(node), feature, relative: false }
            }
        }
    }

    /// `require "lit"` / `require_relative "lit"` with no receiver:
    /// `Some(item)` (or `Some(None)`, consumed), `None` for any other
    /// statement.
    fn literal_require(&self, stmt: &Node<'_>) -> Option<Option<ProgramItem>> {
        let call = stmt.as_call_node()?;
        let name = constant_id_str(&call.name());
        if call.receiver().is_some() || !matches!(name, "require" | "require_relative") || call.block().is_some() {
            return None;
        }
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let [one] = args.as_slice() else { return None };
        let feature = String::from_utf8_lossy(one.as_string_node()?.unescaped()).into_owned();
        if name == "require" && process_machinery_feature(&feature) {
            return Some(None);
        }
        let target = if name == "require" {
            self.walk.resolve(&feature)
        } else {
            self.path.parent().map(|p| normalize(&p.join(with_rb(&feature)))).filter(|p| self.walk.vfs.exists(p))
        };
        Some(Some(match target {
            Some(target) => self.require_of(stmt, &target),
            None => ProgramItem::Require { span: self.span(stmt), feature, relative: name == "require_relative" },
        }))
    }

    fn items(&self, stmts: &[Node<'_>], out: &mut Vec<ProgramItem>) {
        for stmt in stmts {
            if let Some(item) = self.literal_require(stmt) {
                out.extend(item);
                continue;
            }
            if let Some(call) = stmt.as_call_node() {
                let name = constant_id_str(&call.name());
                let on = |constant: &str| receiver_is_constant(&call, constant);
                if name == "require" && on("Bundler") {
                    self.bundler_require(stmt, out);
                    continue;
                }
                if name == "load_embedded_engines" && on("Rails") {
                    for engine in embedded_engine_names(self.walk.vfs, self.walk.dir) {
                        if let Some(target) = self.walk.resolve(&engine) {
                            out.push(self.require_of(stmt, &target));
                        }
                    }
                    continue;
                }
                // Bundler's own activation (`gem "bundler", ">= 0.a"`).
                if name == "gem" && call.receiver().is_none() {
                    continue;
                }
                // config/environment.rb: the emitted boot is Rails'
                // initialization (module doc).
                if name == "initialize!" && call.receiver().is_some_and(|r| is_application(&r)) {
                    continue;
                }
                // `Rails.application.configure do … end` in a boot file:
                // its config writes are the application's (`ingest::app`);
                // its requires load here.
                if name == "configure" && call.receiver().is_some_and(|r| is_application(&r)) {
                    if let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
                        self.body_requires(&flatten_statements(body), out);
                    }
                    continue;
                }
                if load_path_change(&call) {
                    out.push(ProgramItem::NotCarried {
                        span: self.span(stmt),
                        construct: format!("$LOAD_PATH.{name}"),
                        message: "a `$LOAD_PATH` change is not carried: the emitted tree resolves the app's requires when it is emitted (`boot_requires`), and its load path is the runtime's".into(),
                    });
                    continue;
                }
            }
            if let Some(class) = stmt.as_class_node() {
                if let Some(body) = class.body() {
                    self.body_requires(&flatten_statements(body), out);
                }
                continue;
            }
            if let Some(module) = stmt.as_module_node() {
                if let Some(body) = module.body() {
                    self.body_requires(&flatten_statements(body), out);
                }
                continue;
            }
            if writes_bundle_gemfile(stmt) {
                continue;
            }
            // `class << Rails.application` in config/application.rb: its defs are
            // `Rails::Application` methods (`ingest::app`), not a boot statement.
            if self.path.ends_with("config/application.rb") && super::library_class::is_rails_application_singleton(stmt) {
                continue;
            }
            out.push(self.statement(stmt));
        }
    }

    /// A class or module body (or a `configure` block) of a boot file:
    /// its requires, as the walk follows them; the rest is the
    /// application's (`ingest::app`).
    fn body_requires(&self, stmts: &[Node<'_>], out: &mut Vec<ProgramItem>) {
        for stmt in stmts {
            if let Some(item) = self.literal_require(stmt) {
                out.extend(item);
            } else if let Some(class) = stmt.as_class_node() {
                if let Some(body) = class.body() {
                    self.body_requires(&flatten_statements(body), out);
                }
            } else if let Some(module) = stmt.as_module_node() {
                if let Some(body) = module.body() {
                    self.body_requires(&flatten_statements(body), out);
                }
            }
        }
    }

    /// `Bundler.require(...)`: each carried gem entry, in Gemfile order,
    /// a grouped one under `envs.include?(Rails.env.to_s)` (module doc).
    fn bundler_require(&self, stmt: &Node<'_>, out: &mut Vec<ProgramItem>) {
        for gem in &self.walk.gems {
            let require = self.require_of(stmt, &gem.entry);
            if gem.envs.is_empty() {
                out.push(require);
                continue;
            }
            if let Some(ProgramItem::Branch { cond, then_items, .. }) = out.last_mut() {
                if branch_envs(cond).as_deref() == Some(gem.envs.as_slice()) {
                    then_items.push(require);
                    continue;
                }
            }
            match envs_condition(&gem.envs, self.span(stmt)) {
                Some(cond) => out.push(ProgramItem::Branch { cond, negate: false, then_items: vec![require], else_items: Vec::new() }),
                None => out.push(self.not_carried(stmt, format!("the environments condition of {} is not carried", gem.site))),
            }
        }
    }

    /// Any other statement, as Ruby runs it.
    fn statement(&self, stmt: &Node<'_>) -> ProgramItem {
        if let Some(why) = computed_require(stmt) {
            return self.not_carried(
                stmt,
                format!("a computed require is not carried ({why}): the emitted tree loads the files it resolves when it is emitted, and this one names its file only at boot"),
            );
        }
        if defines_method(stmt) {
            return self.not_carried(stmt, "a method this boot-file statement defines is not carried; Ruby defines it when the file loads");
        }
        // `$VERBOSE = true`: the expression ingest reads a global and
        // writes none (as `library_class::program_items` carries it).
        if let Some(write) = stmt.as_global_variable_write_node() {
            return match super::expr::ingest_expr(&write.value(), &self.file) {
                Ok(value) => ProgramItem::Statement(Expr::new(
                    self.span(stmt),
                    crate::expr::ExprNode::Assign {
                        target: crate::expr::LValue::Var {
                            id: crate::ident::VarId(0),
                            name: crate::ident::Symbol::from(constant_id_str(&write.name())),
                        },
                        value,
                    },
                )),
                Err(err) => self.not_carried(stmt, format!("the ingest cannot carry this global variable's value: {err}")),
            };
        }
        match super::expr::ingest_expr(stmt, &self.file) {
            Ok(expr) => ProgramItem::Statement(expr),
            Err(err) => self.not_carried(stmt, format!("the ingest cannot carry this boot-file statement: {err}")),
        }
    }
}

/// The receiver of `call` is the top-level constant `constant`.
fn receiver_is_constant(call: &ruby_prism::CallNode<'_>, constant: &str) -> bool {
    call.receiver().is_some_and(|r| {
        r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == constant)
            || r.as_constant_path_node().is_some_and(|p| {
                p.parent().is_none() && p.name().is_some_and(|n| constant_id_str(&n) == constant)
            })
    })
}

/// `<Const>.application` (`Rails.application`, `Shopify::Application`'s
/// `Rails.application`).
fn is_application(node: &Node<'_>) -> bool {
    node.as_call_node()
        .is_some_and(|c| constant_id_str(&c.name()) == "application" && c.arguments().is_none() && c.receiver().is_some())
}

/// Bundler's and Bootsnap's own setup, which the emitted tree's gem
/// activation stands in for (module doc;
/// `project::boot_require_is_process_machinery`).
fn process_machinery_feature(feature: &str) -> bool {
    ["bundler", "bootsnap"].iter().any(|gem| feature == *gem || feature.starts_with(&format!("{gem}/")))
}

/// `ENV["BUNDLE_GEMFILE"] = …` / `||= …`: Bundler's configuration.
fn writes_bundle_gemfile(stmt: &Node<'_>) -> bool {
    let env_key = |receiver: Option<Node<'_>>, arguments: Option<ruby_prism::ArgumentsNode<'_>>| {
        receiver.is_some_and(|r| r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == "ENV"))
            && arguments.is_some_and(|a| {
                a.arguments().iter().next().and_then(|k| k.as_string_node()).is_some_and(|k| k.unescaped() == b"BUNDLE_GEMFILE")
            })
    };
    if let Some(write) = stmt.as_index_or_write_node() {
        return env_key(write.receiver(), write.arguments());
    }
    if let Some(write) = stmt.as_index_operator_write_node() {
        return env_key(write.receiver(), write.arguments());
    }
    stmt.as_call_node()
        .is_some_and(|c| constant_id_str(&c.name()) == "[]=" && env_key(c.receiver(), c.arguments()))
}

/// `$LOAD_PATH.unshift(…)` and kin (`$:`).
fn load_path_change(call: &ruby_prism::CallNode<'_>) -> bool {
    call.receiver().is_some_and(|r| {
        r.as_global_variable_read_node().is_some_and(|g| matches!(constant_id_str(&g.name()), "$LOAD_PATH" | "$:"))
    }) && matches!(constant_id_str(&call.name()), "unshift" | "push" | "<<" | "prepend" | "concat" | "insert" | "append")
}

/// A `require`/`require_relative`/`load` anywhere in `stmt` whose
/// argument is not a literal: the call as written.
fn computed_require(stmt: &Node<'_>) -> Option<String> {
    use ruby_prism::Visit;
    struct Finds(Option<String>);
    impl<'pr> Visit<'pr> for Finds {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let name = constant_id_str(&node.name());
            if self.0.is_none() && node.receiver().is_none() && matches!(name, "require" | "require_relative" | "load") {
                let args: Vec<Node<'_>> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                if !matches!(args.as_slice(), [one] if one.as_string_node().is_some()) {
                    self.0 = Some(format!("`{name}` of a computed path"));
                }
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut finds = Finds(None);
    finds.visit(stmt);
    finds.0
}

/// Whether `node` holds a `def`.
fn defines_method(node: &Node<'_>) -> bool {
    use ruby_prism::Visit;
    struct Finds(bool);
    impl<'pr> Visit<'pr> for Finds {
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {
            self.0 = true;
        }
    }
    let mut finds = Finds(false);
    finds.visit(node);
    finds.0
}

/// `["test", "development"].include?(Rails.env.to_s)`, at `span`: the
/// condition `Bundler.require(*Rails.groups)` loads a grouped gem under.
fn envs_condition(envs: &[String], span: Span) -> Option<Expr> {
    let list = envs.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>().join(", ");
    let text = format!("[{list}].include?(Rails.env.to_s)\n");
    let result = ruby_prism::parse(text.as_bytes());
    let root = result.node();
    let stmt = root.as_program_node()?.statements().body().iter().next()?;
    let mut expr = super::expr::ingest_expr(&stmt, "<bundler_groups>").ok()?;
    expr.span = span;
    Some(expr)
}

/// The environments an [`envs_condition`] names.
fn branch_envs(cond: &Expr) -> Option<Vec<String>> {
    use crate::expr::{ExprNode, Literal};
    let ExprNode::Send { recv: Some(recv), method, .. } = &*cond.node else { return None };
    if method.as_str() != "include?" {
        return None;
    }
    let ExprNode::Array { elements, .. } = &*recv.node else { return None };
    elements
        .iter()
        .map(|e| match &*e.node {
            ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// config/boot.rb's top-level, unconditional `require "lit"` calls that
/// resolve to no file of the tree (a gem from the gem home, the standard
/// library), as `(feature, site)` in source order: what native boot
/// requires before `require "rails"` and Bundler.require
/// ([`App::boot_native_requires`]).
pub(super) fn boot_native_requires<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    path_gems: &[PathBuf],
    gem_sources: &[(String, PathBuf)],
) -> Vec<(String, String)> {
    let mut load_path: Vec<PathBuf> =
        path_gems.iter().map(|gem| gem.join("lib")).filter(|lib| vfs.is_dir(lib)).collect();
    load_path.extend(gem_sources.iter().map(|(_, gem)| gem.join("lib")).filter(|lib| vfs.is_dir(lib)));
    load_path.push(dir.join("lib"));
    let walk = Walk {
        vfs,
        dir,
        load_path,
        gems: Vec::new(),
        visited: BTreeSet::new(),
        envs: Vec::new(),
        conditional: false,
        loads: Vec::new(),
        conditional_loads: Vec::new(),
        bundler_require: false,
    };
    let file = dir.join("config/boot.rb");
    let Ok(source) = vfs.read(&file) else { return Vec::new() };
    let result = ruby_prism::parse(&source);
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    let mut out = Vec::new();
    for stmt in program.statements().body().iter() {
        let Some(call) = stmt.as_call_node() else { continue };
        if call.receiver().is_some() || constant_id_str(&call.name()) != "require" || call.block().is_some() {
            continue;
        }
        let Some(arguments) = call.arguments() else { continue };
        let args: Vec<_> = arguments.arguments().iter().collect();
        let [one] = args.as_slice() else { continue };
        let Some(string) = one.as_string_node() else { continue };
        let feature = String::from_utf8_lossy(string.unescaped()).into_owned();
        if walk.resolve(&feature).is_none() {
            out.push((feature, walk.site(&file, &source, stmt.location().start_offset())));
        }
    }
    out
}

/// A Gemfile gem whose source the tree carries: its entry file, the
/// environments its group limits it to (empty: every one), and the
/// Gemfile line.
struct BundlerEntry {
    entry: PathBuf,
    envs: Vec<String>,
    site: String,
}

struct Walk<'a, V: Vfs + ?Sized> {
    vfs: &'a V,
    dir: &'a Path,
    load_path: Vec<PathBuf>,
    gems: Vec<BundlerEntry>,
    visited: BTreeSet<PathBuf>,
    envs: Vec<String>,
    conditional: bool,
    loads: Vec<BootLoad>,
    conditional_loads: Vec<BootLoad>,
    /// Whether a walked file calls `Bundler.require`.
    bundler_require: bool,
}

impl<V: Vfs + ?Sized> Walk<'_, V> {
    fn file(&mut self, path: &Path, site: Option<String>) {
        let path = normalize(path);
        if let Some(site) = site {
            let rel = self.rel(&path);
            let load = BootLoad { source: rel, envs: self.envs.clone(), site };
            if self.visited.contains(&path) {
                return;
            }
            if self.conditional {
                if !self.conditional_loads.contains(&load) {
                    self.conditional_loads.push(load);
                }
                // What it requires in turn is conditional too, and is not
                // followed: the emitted boot does not load it.
                return;
            }
            self.loads.push(load);
        }
        if !self.visited.insert(path.clone()) {
            return;
        }
        let Ok(source) = self.vfs.read(&path) else { return };
        let result = ruby_prism::parse(&source);
        let root = result.node();
        let Some(program) = root.as_program_node() else { return };
        self.statements(program.statements().body().iter().collect(), &path, &source);
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(self.dir).unwrap_or(path).display().to_string()
    }

    fn site(&self, file: &Path, source: &[u8], offset: usize) -> String {
        let line = source[..offset.min(source.len())].iter().filter(|b| **b == b'\n').count() + 1;
        format!("{}:{line}", self.rel(file))
    }

    fn statements(&mut self, stmts: Vec<Node<'_>>, file: &Path, source: &[u8]) {
        for stmt in stmts {
            if let Some(call) = stmt.as_call_node() {
                let name = constant_id_str(&call.name());
                let receiver = call.receiver();
                let site = self.site(file, source, stmt.location().start_offset());
                if receiver.is_none() && matches!(name, "require" | "require_relative") {
                    let arg = call.arguments().and_then(|a| {
                        let args: Vec<_> = a.arguments().iter().collect();
                        match args.as_slice() {
                            [one] => one.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()),
                            _ => None,
                        }
                    });
                    let Some(arg) = arg else { continue };
                    let target = if name == "require" {
                        self.resolve(&arg)
                    } else {
                        file.parent().map(|p| normalize(&p.join(with_rb(&arg)))).filter(|p| self.vfs.exists(p))
                    };
                    if let Some(target) = target {
                        self.file(&target, Some(site));
                    }
                    continue;
                }
                let on = |constant: &str| {
                    receiver.as_ref().is_some_and(|r| {
                        r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == constant)
                            || r.as_constant_path_node().is_some_and(|p| {
                                p.parent().is_none() && p.name().is_some_and(|n| constant_id_str(&n) == constant)
                            })
                    })
                };
                if name == "require" && on("Bundler") {
                    self.bundler_require = true;
                    let gems: Vec<(PathBuf, Vec<String>, String)> =
                        self.gems.iter().map(|g| (g.entry.clone(), g.envs.clone(), g.site.clone())).collect();
                    for (entry, envs, gem_site) in gems {
                        let outer = self.envs.clone();
                        if !envs.is_empty() {
                            if !outer.is_empty() && !outer.iter().any(|e| envs.contains(e)) {
                                continue;
                            }
                            if outer.is_empty() {
                                self.envs = envs;
                            }
                        }
                        self.file(&entry, Some(format!("{site} Bundler.require ({gem_site})")));
                        self.envs = outer;
                    }
                    continue;
                }
                // `Rails.application.configure do … end` runs its block as
                // the file loads.
                if name == "configure" {
                    if let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
                        self.statements(flatten_statements(body), file, source);
                    }
                    continue;
                }
                if name == "load_embedded_engines" && on("Rails") {
                    for engine in embedded_engine_names(self.vfs, self.dir) {
                        if let Some(target) = self.resolve(&engine) {
                            self.file(&target, Some(format!("{site} Rails.load_embedded_engines")));
                        }
                    }
                }
                continue;
            }
            if let Some(class) = stmt.as_class_node() {
                if let Some(body) = class.body() {
                    self.statements(flatten_statements(body), file, source);
                }
                continue;
            }
            if let Some(module) = stmt.as_module_node() {
                if let Some(body) = module.body() {
                    self.statements(flatten_statements(body), file, source);
                }
                continue;
            }
            // A branch's requires run only when its condition holds.
            let branches: Vec<Node<'_>> = if let Some(n) = stmt.as_if_node() {
                let mut out: Vec<Node<'_>> = n.statements().map(|s| s.body().iter().collect()).unwrap_or_default();
                if let Some(sub) = n.subsequent() {
                    match sub.as_else_node() {
                        Some(e) => out.extend(e.statements().map(|s| s.body().iter().collect::<Vec<_>>()).unwrap_or_default()),
                        None => out.push(sub),
                    }
                }
                out
            } else if let Some(n) = stmt.as_unless_node() {
                let mut out: Vec<Node<'_>> = n.statements().map(|s| s.body().iter().collect()).unwrap_or_default();
                if let Some(e) = n.else_clause() {
                    out.extend(e.statements().map(|s| s.body().iter().collect::<Vec<_>>()).unwrap_or_default());
                }
                out
            } else {
                Vec::new()
            };
            if !branches.is_empty() {
                let outer = self.conditional;
                self.conditional = true;
                self.statements(branches, file, source);
                self.conditional = outer;
            }
        }
    }

    /// `require "name"` against the load path, first match.
    fn resolve(&self, name: &str) -> Option<PathBuf> {
        let file = with_rb(name);
        self.load_path.iter().map(|lib| lib.join(&file)).find(|p| self.vfs.exists(p) && !self.vfs.is_dir(p))
    }
}

/// The `all:` list of `config/contexts.yml` (gurgel's
/// `Rails.load_embedded_engines` with the default context).
fn embedded_engine_names<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<String> {
    let Ok(text) = vfs.read_to_string(&dir.join("config/contexts.yml")) else { return Vec::new() };
    let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) else { return Vec::new() };
    value
        .get("all")
        .and_then(|all| all.as_sequence())
        .map(|seq| seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// The Gemfile's gems whose source the tree carries, in Gemfile order,
/// with their entry files (module doc). A gem is a `gem "name", ...`
/// call, or a call to a method the Gemfile defines whose first
/// parameter is the gem's name and which forwards to `gem` (Core's
/// `in_repo_gem(short_name, require: short_name)`): its `require:`
/// default is that parameter, or the literal the definition gives.
fn bundler_entries<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    path_gems: &[PathBuf],
    gem_sources: &[(String, PathBuf)],
) -> Vec<BundlerEntry> {
    let mut out = Vec::new();
    for GemfileGem { name, require, path, groups, line } in gemfile_gems(vfs, dir) {
        let name = name.as_str();
        let path = path.as_deref();
        let groups = &groups;
        let Some(require) = require else { continue };
        let explicit_dir = path.map(|p| normalize(&dir.join(p)));
        let dirs: Vec<&PathBuf> = explicit_dir
            .iter()
            .chain(gem_sources
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, d)| d))
            .chain(path_gems.iter().filter(|gem| {
                gem.file_name().and_then(|f| f.to_str()).is_some_and(|base| {
                    base == name
                        || base.strip_prefix(name).and_then(|r| r.strip_prefix('-')).is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
                })
            }))
            .collect();
        let candidates: Vec<String> = match &require {
            Some(explicit) => vec![explicit.clone()],
            None => {
                let mut c = vec![name.to_string()];
                if name.contains('-') {
                    c.push(name.replace('-', "/"));
                }
                c
            }
        };
        let envs: Vec<String> = if groups.is_empty() || groups.iter().any(|g| g == "default") { Vec::new() } else { groups.to_vec() };
        for gem_dir in dirs {
            let gem_dir = if gem_dir.is_absolute() { gem_dir.clone() } else { dir.join(gem_dir) };
            if let Some(entry) = candidates.iter().map(|c| gem_dir.join("lib").join(with_rb(c))).find(|p| vfs.exists(p)) {
                out.push(BundlerEntry { entry, envs: envs.clone(), site: format!("Gemfile:{line}") });
                break;
            }
        }
    }
    out
}

/// A `gem` call of the Gemfile (or of a Gemfile-local helper that
/// forwards to `gem`).
struct GemfileGem {
    name: String,
    /// None: `require: false`; Some(None): the default; Some(Some(p)):
    /// an explicit require path.
    require: Option<Option<String>>,
    path: Option<String>,
    groups: Vec<String>,
    line: usize,
}

fn gemfile_gems<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<GemfileGem> {
    let Ok(source) = vfs.read(&dir.join("Gemfile")) else { return Vec::new() };
    let result = ruby_prism::parse(&source);
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    let stmts: Vec<Node<'_>> = program.statements().body().iter().collect();
    // Gemfile-local helpers: name => require default (None: the name).
    let mut helpers: Vec<(String, Option<Option<String>>)> = Vec::new();
    for stmt in &stmts {
        let Some(def) = stmt.as_def_node() else { continue };
        let forwards = def.body().is_some_and(|b| {
            flatten_statements(b).iter().any(|s| s.as_call_node().is_some_and(|c| constant_id_str(&c.name()) == "gem"))
        });
        let Some(params) = def.parameters().filter(|_| forwards) else { continue };
        let first = params.requireds().iter().next().and_then(|p| p.as_required_parameter_node()).map(|p| constant_id_str(&p.name()).to_string());
        let Some(first) = first else { continue };
        let mut default: Option<Option<String>> = None;
        for kw in params.keywords().iter() {
            let Some(opt) = kw.as_optional_keyword_parameter_node() else { continue };
            if constant_id_str(&opt.name()) != "require" {
                continue;
            }
            let value = opt.value();
            default = if value.as_local_variable_read_node().is_some_and(|l| constant_id_str(&l.name()) == first) {
                None
            } else if value.as_false_node().is_some() {
                Some(None)
            } else {
                value.as_string_node().map(|s| Some(String::from_utf8_lossy(s.unescaped()).into_owned()))
            };
        }
        helpers.push((constant_id_str(&def.name()).to_string(), default));
    }
    let mut out = Vec::new();
    gem_calls(&stmts, &helpers, &[], &source, &mut |name, require, path, groups, line| {
        out.push(GemfileGem {
            name: name.to_string(),
            require,
            path: path.map(str::to_string),
            groups: groups.to_vec(),
            line,
        });
    });
    out
}

/// The Gemfile gems the boot's `Bundler.require` requires, whatever
/// their source (installed, git, or carried): each one not `require:
/// false`, with its require path (None: Bundler's default, the name and
/// then the name with `-` as `/`) and the environments its groups limit
/// it to (empty: every one; `Bundler.require(*Rails.groups)` requires
/// `:default` and the environment's group). Empty when no boot file
/// calls `Bundler.require`.
pub(crate) fn bundler_required_gems<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<crate::app::BundlerGem> {
    let mut walk = Walk {
        vfs,
        dir,
        load_path: vec![dir.join("lib")],
        gems: Vec::new(),
        visited: BTreeSet::new(),
        envs: Vec::new(),
        conditional: false,
        loads: Vec::new(),
        conditional_loads: Vec::new(),
        bundler_require: false,
    };
    for seed in ["config/boot.rb", "config/application.rb", "config/environment.rb"] {
        walk.file(&dir.join(seed), None);
    }
    if !walk.bundler_require {
        return Vec::new();
    }
    gemfile_gems(vfs, dir)
        .into_iter()
        .filter_map(|gem| {
            let require = gem.require?;
            let envs = if gem.groups.is_empty() || gem.groups.iter().any(|g| g == "default") { Vec::new() } else { gem.groups };
            Some(crate::app::BundlerGem { name: gem.name, require, envs, site: format!("Gemfile:{}", gem.line) })
        })
        .collect()
}

/// Each gem call in `stmts`: `(name, require (None: `require: false`;
/// Some(None): the default), groups, line)`, descending into `group`,
/// `source`, `platforms` and other blocks.
fn gem_calls(
    stmts: &[Node<'_>],
    helpers: &[(String, Option<Option<String>>)],
    groups: &[String],
    source: &[u8],
    f: &mut dyn FnMut(&str, Option<Option<String>>, Option<&str>, &[String], usize),
) {
    for stmt in stmts {
        let Some(call) = stmt.as_call_node() else { continue };
        let method = constant_id_str(&call.name());
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if let Some(block) = call.block().and_then(|b| b.as_block_node()) {
            let mut inner = groups.to_vec();
            if method == "group" {
                inner = args.iter().filter_map(|a| a.as_symbol_node()).map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()).collect();
            }
            if let Some(body) = block.body() {
                gem_calls(&flatten_statements(body), helpers, &inner, source, f);
            }
            continue;
        }
        if call.receiver().is_some() {
            continue;
        }
        let helper = helpers.iter().find(|(h, _)| h == method);
        if method != "gem" && helper.is_none() {
            continue;
        }
        let Some(name) = args.first().and_then(|a| a.as_string_node()).map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()) else {
            continue;
        };
        let mut require: Option<Option<String>> = match helper {
            Some((_, Some(None))) => None,
            Some((_, Some(Some(literal)))) => Some(Some(literal.clone())),
            _ => Some(None),
        };
        let mut path: Option<String> = None;
        let mut own_groups: Vec<String> = groups.to_vec();
        for arg in &args {
            let Some(hash) = arg.as_keyword_hash_node() else { continue };
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else { continue };
                let Some(key) = assoc.key().as_symbol_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()) else { continue };
                let value = assoc.value();
                match key.as_str() {
                    "require" => {
                        require = if value.as_false_node().is_some() {
                            None
                        } else if let Some(s) = value.as_string_node() {
                            Some(Some(String::from_utf8_lossy(s.unescaped()).into_owned()))
                        } else {
                            Some(None)
                        };
                    }
                    "path" => path = value.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()),
                    "group" | "groups" => {
                        let mut named: Vec<String> = Vec::new();
                        if let Some(s) = value.as_symbol_node() {
                            named.push(String::from_utf8_lossy(s.unescaped()).into_owned());
                        } else if let Some(array) = value.as_array_node() {
                            named.extend(array.elements().iter().filter_map(|e| e.as_symbol_node()).map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()));
                        }
                        if !named.is_empty() {
                            own_groups = named;
                        }
                    }
                    _ => {}
                }
            }
        }
        let offset = stmt.location().start_offset();
        let line = source[..offset.min(source.len())].iter().filter(|b| **b == b'\n').count() + 1;
        f(&name, require, path.as_deref(), &own_groups, line);
    }
}

fn with_rb(name: &str) -> String {
    if name.ends_with(".rb") { name.to_string() } else { format!("{name}.rb") }
}

/// Lexical `.`/`..` folding.
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
