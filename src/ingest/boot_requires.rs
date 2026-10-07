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
//!   ([`App::boot_conditional_loads`]) and not loaded by the emitted
//!   boot, which cannot evaluate the boot file's condition. Those are
//!   named in the emitted `app/models.rb`.
//!
//! NOT MODELED: a computed require (`ENV["BOOT_PRELOAD"]`'s paths),
//! `$LOAD_PATH` changes the boot files make, Bundler's `:debug` group
//! argument to `Rails.groups`.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use ruby_prism::Node;

use crate::app::BootLoad;
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
        let Some(require) = require else { return };
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
    });
    out
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
