//! The constants the gems the CRuby tree loads natively declare, read
//! from their installed source.
//!
//! A `#:` declared type naming a gem's class (`#: OpenTelemetry::Trace::Tracer`)
//! is decided against what exists at runtime. The RBIs Tapioca writes are
//! one description of that, but a tree need not carry `sorbet/`; the gem
//! home the tree's Bundler resolves its gems in always has the code itself.
//! So for each gem `GemBoundary::loaded` names (the Rails-free set the
//! emitted Bundler.require loads, `crate::project::lock_loaded_gems`) this
//! reads `<home>/gems/<name>-<locked version>/<require path>/**/*.rb` with
//! prism and records every constant it declares, fully qualified:
//!
//! * `module A; class B::C` nesting (`A`, `A::B`, `A::B::C`);
//! * constant writes, `X = ...`, `A::X = ...`, `X ||= ...`, multi-writes;
//! * `autoload :X, "..."` and `const_set(:X, ...)` with a literal name,
//!   on `self` or no receiver: the constant exists by that name.
//!
//! `const_set` under a computed name is the one dynamic creation recorded:
//! the namespace is noted as one the gem fills, so a constant only it can
//! account for is owned by namespace and the diagnostic says so.
//!
//! Each gem's read is cached for the process by its directory (a gem
//! home's `<name>-<version>` directory is that version's code), walked in
//! sorted order so the first declaring file is stable, and bounded per
//! gem by `MAX_FILES` and `MAX_BYTES`; a gem cut short is recorded as
//! incomplete, so absence from it is never read as a fact.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ruby_prism::Node;

use crate::gem_boundary::{GemSourceConstants, GemSourceScan, SourceKind, SourceMixin};
use crate::gems::Lockfile;
use crate::vfs::Vfs;

use super::util::constant_id_str;

/// Ruby files read per gem. The largest Rails-free gem Core loads
/// (stripe 18.4.2) has 1,026.
const MAX_FILES: usize = 4096;
/// Source bytes read per gem; stripe's `lib/` is 6.1 MB.
const MAX_BYTES: u64 = 48 << 20;
/// Directory depth under a require path.
const MAX_DEPTH: usize = 24;

/// What one gem directory declares.
#[derive(Debug, Default)]
struct DirScan {
    /// Constant -> first declaring file (relative to the gem directory).
    declared: BTreeMap<String, String>,
    /// Namespace -> first file filling it under a computed name.
    dynamic: BTreeMap<String, String>,
    /// Class or module -> how its first `class`/`module` statement opens it.
    kinds: BTreeMap<String, SourceKind>,
    /// Module -> what its bodies give it as a mixin (`SourceMixin`),
    /// names resolved once the whole gem is read.
    mixins: BTreeMap<String, SourceMixin>,
    /// Constants a namespace body makes private (`private_constant :X`),
    /// fully qualified.
    private: BTreeSet<String>,
    /// The mixin references the walk saw, resolved by `resolve_mixins`.
    pending: Vec<PendingMixin>,
    files: usize,
    bytes: u64,
    complete: bool,
    native: bool,
}

fn cache() -> &'static Mutex<BTreeMap<(PathBuf, Vec<String>), Arc<DirScan>>> {
    static CACHE: OnceLock<Mutex<BTreeMap<(PathBuf, Vec<String>), Arc<DirScan>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The gem home a tree's gems are read from: `roundhouse.yml`'s
/// `gem_home:`, else `ROUNDHOUSE_GEM_HOME` (what bin/emit-target sets to
/// the gem home the native run pins). `None` when neither names one.
pub fn gem_home(configured: Option<&Path>) -> Option<PathBuf> {
    configured
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("ROUNDHOUSE_GEM_HOME").filter(|v| !v.is_empty()).map(PathBuf::from))
}

/// Read the installed source of each gem in `gems` (lockfile spec names)
/// under `home`, at the version `lock` resolves.
pub fn scan<V: Vfs + ?Sized>(vfs: &V, home: &Path, lock: &Lockfile, gems: &BTreeSet<String>) -> GemSourceConstants {
    let mut out = GemSourceConstants { home: home.display().to_string(), ..Default::default() };
    let mut installed: Option<Vec<PathBuf>> = None;
    for gem in gems {
        let Some(version) = lock.version_of(gem) else { continue };
        let Some(dir) = gem_dir(vfs, home, lock, gem, version, &mut installed) else { continue };
        let (require_paths, extensions) = gemspec(vfs, home, &dir, gem, version);
        let key = (dir.clone(), require_paths.clone());
        let cached = cache().lock().ok().and_then(|c| c.get(&key).cloned());
        let read = match cached {
            Some(read) => read,
            None => {
                let mut read = scan_dir(vfs, &dir, &require_paths);
                read.native = extensions || vfs.is_dir(&dir.join("ext"));
                let read = Arc::new(read);
                if let Ok(mut c) = cache().lock() {
                    c.insert(key, read.clone());
                }
                read
            }
        };
        for (name, file) in &read.declared {
            out.declared.entry(name.clone()).or_default().insert(gem.clone(), file.clone());
        }
        for (name, file) in &read.dynamic {
            out.dynamic.entry(name.clone()).or_default().insert(gem.clone(), file.clone());
        }
        for (name, kind) in &read.kinds {
            out.kinds.entry(name.clone()).or_default().insert(gem.clone(), kind.clone());
        }
        for (name, mixin) in &read.mixins {
            out.mixins.entry(name.clone()).or_default().insert(gem.clone(), mixin.clone());
        }
        out.private.extend(read.private.iter().cloned());
        out.scans.insert(
            gem.clone(),
            GemSourceScan {
                version: version.to_string(),
                dir: dir.display().to_string(),
                files: read.files,
                bytes: read.bytes,
                complete: read.complete,
                native: read.native,
            },
        );
    }
    out
}

/// How deep under a require path [`engine_isolated_prefixes`] looks for
/// an `engine.rb`: Rails' engine generator writes `lib/<name>/engine.rb`
/// (depth 1), a namespaced one `lib/<org>/<name>/engine.rb`.
const MAX_ENGINE_DEPTH: usize = 3;

/// The `isolate_namespace` table prefixes the installed engines of
/// `gems` (lockfile spec names the boot's `Bundler.require` loads)
/// define, read from each gem's `engine.rb` files under its require
/// paths at the locked version: maintenance_tasks 2.17.0's
/// `lib/maintenance_tasks/engine.rb` runs `isolate_namespace
/// MaintenanceTasks` when Bundler requires the gem, and Rails then
/// defines `MaintenanceTasks.table_name_prefix` as `maintenance_tasks_`,
/// which an app model in the namespace (Core's
/// `MaintenanceTasks::CrossPodRun < ::ApplicationRecord`) reads too. The
/// gem is read, not carried. A gem the app carries as source is read with
/// the app's own files.
pub fn engine_isolated_prefixes<V: Vfs + ?Sized>(
    vfs: &V,
    home: &Path,
    lock: &Lockfile,
    gems: &BTreeSet<String>,
) -> super::model::TablePrefixes {
    let mut out = super::model::TablePrefixes::default();
    let mut installed: Option<Vec<PathBuf>> = None;
    for gem in gems {
        if lock.is_in_repo(gem) {
            continue;
        }
        let Some(version) = lock.version_of(gem) else { continue };
        let Some(dir) = gem_dir(vfs, home, lock, gem, version, &mut installed) else { continue };
        let (require_paths, _) = gemspec(vfs, home, &dir, gem, version);
        let mut engines = Vec::new();
        for path in &require_paths {
            engine_files(vfs, &dir.join(path), 0, &mut engines);
        }
        engines.sort();
        engines.dedup();
        for file in engines {
            let Ok(source) = vfs.read(&file) else { continue };
            for (module, prefix) in super::model::ingest_isolated_namespace_prefixes(&source, &file.display().to_string()) {
                out.entry(module).or_insert(prefix);
            }
        }
    }
    out
}

fn engine_files<V: Vfs + ?Sized>(vfs: &V, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(mut entries) = vfs.read_dir(dir) else { return };
    entries.sort();
    for entry in entries {
        if vfs.is_dir(&entry) {
            if depth < MAX_ENGINE_DEPTH {
                engine_files(vfs, &entry, depth + 1, out);
            }
        } else if entry.file_name().is_some_and(|f| f == "engine.rb") {
            out.push(entry);
        }
    }
}

/// The Rails gems the boot's gem-source dependency block refuses a
/// feature for depending on (`project::gem_source_dependency_block`,
/// `__rh_rails_gems`).
/// Gems whose lock closure reaches a Rails gem only through features the
/// runtime's ports answer, measured in the emitted Core tree (r19, CRuby on
/// PostgreSQL): tracking_number 3.0.0's `require 'active_support/all'`
/// loads runtime/features/active_support/all.rb and no Rails gem, and
/// `TrackingNumber.search` answers as natively. carriers_db 2.29.0's
/// analyzers/checksum.rb requires it in its compiled file program.
const PORT_ANSWERED_RAILS_DEPENDENTS: &[&str] = &["tracking_number"];

const RAILS_GEMS: &[&str] = &["activesupport", "activerecord", "activemodel", "actionpack", "actionview", "activejob", "railties"];

/// Add to `app`'s source constants the gems the gem sources (`gem_sources:`,
/// compiled with the app) require from the gem home as they load
/// (`project::gem_source_dependencies`), which the boot loads there too:
/// elasticsearch-shopify 6.0.1's `require "elastic/transport"` loads
/// elastic-transport, whose `Elastic::Transport::Transport::Errors::*`
/// its errors.rb lists at load. So do the gem sources' other files, as
/// they load ([`gem_source_file_requires`]): hedwig 1.68.0's
/// configuration.rb, which its entry reaches only through `autoload
/// :Configuration`, requires mono_logger for `MonoLogger.new`. A
/// required feature is a gem's when that gem, a lock dependency of a gem
/// source (transitively), has the file under a require path. Not read: a
/// gem whose lock closure reaches a Rails gem (the boot refuses its
/// feature), a ported gem (its runtime port answers,
/// `project::gem_port_files`), a gem the app carries, and a gem already
/// read. Runs after the sources are registered, which the feature walk
/// reads. A gem owning a feature required for certain (not only under an
/// `if`) joins `GemBoundary::loaded`.
pub fn scan_gem_source_requires<V: Vfs + ?Sized>(vfs: &V, home: &Path, dir: &Path, app: &mut crate::app::App) {
    let Some(lock) = app.gem_lock.as_ref() else { return };
    let (features, _, _) = crate::project::gem_source_dependencies_with(app, true);
    // What the gem sources require for certain: the entry walk's, which
    // the boot's gem facades require in the CRuby tree
    // (`project::gem_source_dependencies`), and each other file's, which
    // its compiled program requires where Ruby would.
    let (certain, _, _) = crate::project::gem_source_dependencies(app);
    let (file_features, file_certain) = gem_source_file_requires(app, dir);
    let features: BTreeSet<String> = features.into_iter().chain(file_features).collect();
    let certain: BTreeSet<String> = certain.into_iter().chain(file_certain.iter().cloned()).collect();
    if features.is_empty() {
        return;
    }
    fn deps<'a>(lock: &'a Lockfile, gem: &str) -> impl Iterator<Item = &'a str> {
        lock.spec_deps.get(gem).into_iter().flatten().map(String::as_str)
    }
    let reaches_rails = |gem: &str| {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&str> = deps(lock, gem).collect();
        if RAILS_GEMS.contains(&gem) {
            return true;
        }
        while let Some(next) = stack.pop() {
            if RAILS_GEMS.contains(&next) {
                return true;
            }
            if seen.insert(next) {
                stack.extend(deps(lock, next));
            }
        }
        false
    };
    let mut candidates = BTreeSet::new();
    let mut stack: Vec<&str> = lock.in_repo.iter().flat_map(|g| deps(lock, g)).collect();
    while let Some(gem) = stack.pop() {
        if candidates.insert(gem) {
            stack.extend(deps(lock, gem));
        }
    }
    let sources = &app.gem_boundary.source_constants;
    let mut installed = None;
    let mut found = BTreeSet::new();
    let mut loads = BTreeSet::new();
    for gem in candidates {
        // A gem measured to load over the runtime's Rails ports is loaded
        // where a compiled file program requires it itself, which no boot
        // guard stands in front of.
        let port_answered = PORT_ANSWERED_RAILS_DEPENDENTS.contains(&gem);
        if sources.scans.contains_key(gem)
            || lock.is_in_repo(gem)
            || crate::project::gem_requires_rails(gem).is_some()
            || crate::project::gem_port_files(gem).is_some()
            || (reaches_rails(gem) && !port_answered)
        {
            continue;
        }
        let Some(version) = lock.version_of(gem) else { continue };
        let Some(dir) = gem_dir(vfs, home, lock, gem, version, &mut installed) else { continue };
        let (require_paths, _) = gemspec(vfs, home, &dir, gem, version);
        let owns = |feature: &str| {
            let rb = if feature.ends_with(".rb") { feature.to_string() } else { format!("{feature}.rb") };
            require_paths.iter().any(|path| vfs.exists(&dir.join(path).join(&rb)))
        };
        if port_answered && reaches_rails(gem) {
            if file_certain.iter().any(|f| owns(f)) {
                loads.insert(gem.to_string());
            }
            continue;
        }
        if features.iter().any(|f| owns(f)) {
            found.insert(gem.to_string());
        }
        if certain.iter().any(|f| owns(f)) {
            loads.insert(gem.to_string());
        }
    }
    // Such a gem loads natively in the CRuby tree as surely as a
    // Bundler.require entry does: a constant its RBI declares
    // (elastic-transport's `Elastic::Transport::Transport::Errors::Gone`,
    // read by elasticsearch-shopify's errors.rb at load) runs as the gem's.
    // Its RBI, which the boundary read before the gem sources were
    // registered skipped, types those constants (`Decision::DROP`).
    super::rbi::read_more_gem_rbis(vfs, dir, lock, &mut app.gem_boundary, &loads);
    app.gem_boundary.loaded.extend(loads);
    if found.is_empty() {
        return;
    }
    let read = scan(vfs, home, lock, &found);
    let sources = &mut app.gem_boundary.source_constants;
    for (name, gems) in read.declared {
        sources.declared.entry(name).or_default().extend(gems);
    }
    for (name, gems) in read.dynamic {
        sources.dynamic.entry(name).or_default().extend(gems);
    }
    for (name, gems) in read.kinds {
        sources.kinds.entry(name).or_default().extend(gems);
    }
    for (name, gems) in read.mixins {
        sources.mixins.entry(name).or_default().extend(gems);
    }
    sources.private.extend(read.private);
    sources.scans.extend(read.scans);
}

/// The features every file of a gem source (`App::gem_source_dirs`)
/// requires from outside the compiled tree as it loads, beyond the files
/// its entry reaches by `require` (`project::gem_source_dependencies`):
/// a file Ruby loads on a constant reference, through `autoload` (hedwig
/// 1.68.0's `autoload :Configuration` under `eager_autoload`), or that
/// the app requires itself. Its compiled program carries the require
/// (`RoundhouseAutoload.file_statement`), so the gem is loaded before
/// any of the file's own code reads its constants, as in Ruby. `(may,
/// certain)`: a require at the file's top level or in a `class`/`module`
/// body is certain; one under an `if`/`unless`, a `case` or in a `rescue`
/// or `else` body is only possible. Not listed: `require_relative`, a
/// Rails framework feature (the runtime's ports answer those), and a
/// feature some compiled `lib/` answers.
fn gem_source_file_requires(app: &crate::app::App, dir: &Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut may = BTreeSet::new();
    let mut certain = BTreeSet::new();
    if app.gem_source_dirs.is_empty() {
        return (may, certain);
    }
    let root = dir.display().to_string();
    let relative = |path: &str| -> String {
        path.strip_prefix(root.as_str()).map(|p| p.trim_start_matches('/')).unwrap_or(path).to_string()
    };
    // Each compiled `lib/` file by the feature it answers, under every
    // `lib/` its path has.
    let mut compiled: BTreeSet<String> = BTreeSet::new();
    for source in &app.sources {
        let rel = relative(&source.path);
        let Some(stem) = rel.strip_suffix(".rb") else { continue };
        if let Some(feature) = stem.strip_prefix("lib/") {
            compiled.insert(feature.to_string());
        }
        for (at, _) in stem.match_indices("/lib/") {
            compiled.insert(stem[at + "/lib/".len()..].to_string());
        }
    }
    for source in &app.sources {
        let rel = relative(&source.path);
        if !rel.ends_with(".rb") || !app.gem_source_dirs.iter().any(|d| rel.starts_with(d.as_str())) {
            continue;
        }
        if !source.text.contains("require") {
            continue;
        }
        let mut found = Vec::new();
        file_requires(source.text.as_bytes(), &mut found);
        for (feature, conditional) in found {
            let stem = feature.strip_suffix(".rb").unwrap_or(&feature);
            if crate::project::is_framework_feature(&feature) || compiled.contains(stem) {
                continue;
            }
            if !conditional {
                certain.insert(feature.clone());
            }
            may.insert(feature);
        }
    }
    (may, certain)
}

/// A file's load-level `require "feature"` calls (no receiver, one
/// string literal), each with whether it runs only under a condition.
/// A method, block or lambda body runs when called, and is not read.
fn file_requires(source: &[u8], out: &mut Vec<(String, bool)>) {
    fn statements(node: Option<ruby_prism::StatementsNode<'_>>, conditional: bool, out: &mut Vec<(String, bool)>) {
        for node in node.iter().flat_map(|s| s.body().iter()) {
            walk(&node, conditional, out);
        }
    }
    fn body(node: Option<Node<'_>>, conditional: bool, out: &mut Vec<(String, bool)>) {
        match node {
            Some(node) => match node.as_statements_node() {
                Some(s) => statements(Some(s), conditional, out),
                None => walk(&node, conditional, out),
            },
            None => {}
        }
    }
    fn walk(node: &Node<'_>, conditional: bool, out: &mut Vec<(String, bool)>) {
        if let Some(call) = node.as_call_node() {
            if call.name().as_slice() != b"require" || call.receiver().is_some() || call.block().is_some() {
                return;
            }
            let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
            if let [arg] = args.as_slice() {
                if let Some(s) = arg.as_string_node() {
                    out.push((String::from_utf8_lossy(s.unescaped()).into_owned(), conditional));
                }
            }
        } else if let Some(n) = node.as_module_node() {
            body(n.body(), conditional, out);
        } else if let Some(n) = node.as_class_node() {
            body(n.body(), conditional, out);
        } else if let Some(n) = node.as_if_node() {
            statements(n.statements(), true, out);
            body(n.subsequent(), true, out);
        } else if let Some(n) = node.as_else_node() {
            statements(n.statements(), true, out);
        } else if let Some(n) = node.as_unless_node() {
            statements(n.statements(), true, out);
            if let Some(e) = n.else_clause() {
                statements(e.statements(), true, out);
            }
        } else if let Some(n) = node.as_case_node() {
            for when in n.conditions().iter() {
                if let Some(when) = when.as_when_node() {
                    statements(when.statements(), true, out);
                }
            }
            if let Some(e) = n.else_clause() {
                statements(e.statements(), true, out);
            }
        } else if let Some(n) = node.as_begin_node() {
            statements(n.statements(), conditional, out);
            let mut rescue = n.rescue_clause();
            while let Some(r) = rescue {
                statements(r.statements(), true, out);
                rescue = r.subsequent();
            }
            if let Some(e) = n.else_clause() {
                statements(e.statements(), true, out);
            }
            if let Some(e) = n.ensure_clause() {
                statements(e.statements(), conditional, out);
            }
        } else if let Some(n) = node.as_parentheses_node() {
            body(n.body(), conditional, out);
        }
    }
    let result = ruby_prism::parse(source);
    let root = result.node();
    if let Some(program) = root.as_program_node() {
        statements(Some(program.statements()), false, out);
    }
}

/// `<home>/gems/<gem>-<version>` (or a platform build of it,
/// `nokogiri-1.19.4-x86_64-linux-gnu`); a gem the lock takes from git is
/// Bundler's checkout `<home>/bundler/gems/<repo>-<rev12>`, or the gem's
/// own subdirectory of it.
fn gem_dir<V: Vfs + ?Sized>(
    vfs: &V,
    home: &Path,
    lock: &Lockfile,
    gem: &str,
    version: &str,
    installed: &mut Option<Vec<PathBuf>>,
) -> Option<PathBuf> {
    if let Some(git) = lock.git_source_of(gem) {
        let checkout = home.join("bundler/gems").join(git.checkout_dir());
        if vfs.exists(&checkout.join(format!("{gem}.gemspec"))) {
            return Some(checkout);
        }
        let nested = checkout.join(gem);
        return vfs.exists(&nested.join(format!("{gem}.gemspec"))).then_some(nested);
    }
    let exact = home.join("gems").join(format!("{gem}-{version}"));
    if vfs.is_dir(&exact) {
        return Some(exact);
    }
    let entries = installed.get_or_insert_with(|| {
        let mut entries = vfs.read_dir(&home.join("gems")).unwrap_or_default();
        entries.sort();
        entries
    });
    let platform = format!("{gem}-{version}-");
    entries
        .iter()
        .find(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.starts_with(&platform)))
        .cloned()
}

/// The gemspec's `require_paths` (`<home>/specifications/<gem>-<version>.gemspec`,
/// as RubyGems writes it: `s.require_paths = ["lib".freeze]`), `lib`
/// when it names none, and whether it builds `extensions`. A gem the lock
/// takes from git has no specification there: Bundler loads the
/// checkout's own `<gem>.gemspec` (`dir`, [`gem_dir`]), as the gem home
/// installs it in the same form (grpc's
/// `s.require_paths = ["src/ruby/lib".freeze, ...]`).
fn gemspec<V: Vfs + ?Sized>(vfs: &V, home: &Path, dir: &Path, gem: &str, version: &str) -> (Vec<String>, bool) {
    let spec = home.join("specifications").join(format!("{gem}-{version}.gemspec"));
    let text = vfs
        .read_to_string(&spec)
        .ok()
        .or_else(|| vfs.read(&dir.join(format!("{gem}.gemspec"))).ok().map(|b| String::from_utf8_lossy(&b).into_owned()));
    let extensions = text.as_deref().is_some_and(|t| t.lines().any(|l| l.trim_start().starts_with("s.extensions")));
    let paths: Vec<String> = text
        .and_then(|text| {
            let line = text.lines().find(|l| l.trim_start().starts_with("s.require_paths"))?;
            let list = line.split_once('=')?.1;
            Some(
                list.split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|p| !p.is_empty() && !p.contains("..") && !p.starts_with('/'))
                    .map(str::to_string)
                    .collect(),
            )
        })
        .unwrap_or_default();
    (if paths.is_empty() { vec!["lib".to_string()] } else { paths }, extensions)
}

fn scan_dir<V: Vfs + ?Sized>(vfs: &V, dir: &Path, require_paths: &[String]) -> DirScan {
    let mut files = Vec::new();
    let mut complete = true;
    for path in require_paths {
        collect(vfs, &dir.join(path), 0, &mut files, &mut complete);
    }
    files.sort();
    files.dedup();
    let mut out = DirScan { complete, ..DirScan::default() };
    for file in files {
        if out.files >= MAX_FILES {
            out.complete = false;
            break;
        }
        let Ok(source) = vfs.read(&file) else { continue };
        if out.bytes + source.len() as u64 > MAX_BYTES {
            out.complete = false;
            break;
        }
        out.files += 1;
        out.bytes += source.len() as u64;
        let relative = file.strip_prefix(dir).unwrap_or(&file).display().to_string();
        read_file(&source, &relative, &mut out);
    }
    resolve_mixins(&mut out);
    out
}

/// A constant a module body names as a mixin (`include X`, or the `X` of
/// its `included` hook's `base.extend(X)`), with the body's lexical
/// scopes, innermost first.
#[derive(Debug)]
struct PendingMixin {
    module: String,
    extend_from_hook: bool,
    rooted: bool,
    written: String,
    scopes: Vec<String>,
}

/// Resolve each pending mixin reference against the gem's declared
/// constants, as Ruby looks the name up from the body's scope outwards
/// (`ClassMethods` in `module SmartProperties` is
/// `SmartProperties::ClassMethods`); one the gem does not declare is
/// another gem's or the app's, and is left out.
fn resolve_mixins(out: &mut DirScan) {
    for pending in std::mem::take(&mut out.pending) {
        let candidates: Vec<String> = if pending.rooted {
            vec![pending.written.clone()]
        } else {
            pending
                .scopes
                .iter()
                .map(|scope| format!("{scope}::{}", pending.written))
                .chain(std::iter::once(pending.written.clone()))
                .collect()
        };
        let Some(found) = candidates.into_iter().find(|c| out.declared.contains_key(c)) else { continue };
        let entry = out.mixins.entry(pending.module).or_default();
        let list = if pending.extend_from_hook { &mut entry.mixes_in_class_methods } else { &mut entry.includes };
        if !list.contains(&found) {
            list.push(found);
        }
    }
}

fn collect<V: Vfs + ?Sized>(vfs: &V, dir: &Path, depth: usize, out: &mut Vec<PathBuf>, complete: &mut bool) {
    if depth > MAX_DEPTH {
        *complete = false;
        return;
    }
    let Ok(mut entries) = vfs.read_dir(dir) else { return };
    entries.sort();
    for entry in entries {
        if out.len() > MAX_FILES {
            *complete = false;
            return;
        }
        if vfs.is_dir(&entry) {
            collect(vfs, &entry, depth + 1, out, complete);
        } else if entry.extension().is_some_and(|e| e == "rb") {
            out.push(entry);
        }
    }
}

/// One file's declarations, merged into `out` (an earlier file keeps
/// the constants it declared first).
fn read_file(source: &[u8], file: &str, out: &mut DirScan) {
    let result = ruby_prism::parse(source);
    let mut walker = Walker { scope: Vec::new(), singleton: 0, in_def: 0, file, out };
    ruby_prism::Visit::visit(&mut walker, &result.node());
}

struct Walker<'a> {
    /// Lexical nesting, each entry fully qualified.
    scope: Vec<String>,
    /// Inside `class << self`: constants there are the singleton
    /// class's, not reachable as `Owner::X`.
    singleton: usize,
    /// Inside a `def`: its body runs when called, not at load.
    in_def: usize,
    file: &'a str,
    out: &'a mut DirScan,
}

/// The written path of a constant read or path node: `(rooted, "A::B")`,
/// `None` when a segment is not a constant (`foo::Bar`).
fn written_path(node: &Node<'_>) -> Option<(bool, String)> {
    if let Some(read) = node.as_constant_read_node() {
        return Some((false, constant_id_str(&read.name()).to_string()));
    }
    let path = node.as_constant_path_node()?;
    let name = constant_id_str(&path.name()?).to_string();
    match path.parent() {
        None => Some((true, name)),
        Some(parent) => {
            let (rooted, outer) = written_path(&parent)?;
            Some((rooted, format!("{outer}::{name}")))
        }
    }
}

impl Walker<'_> {
    fn current(&self) -> Option<&str> {
        self.scope.last().map(String::as_str)
    }

    fn qualify(&self, rooted: bool, written: &str) -> String {
        match self.current() {
            Some(scope) if !rooted => format!("{scope}::{written}"),
            _ => written.to_string(),
        }
    }

    /// Record `name` and each namespace it sits in: `A::B::C` declared
    /// means `A` and `A::B` exist too.
    fn declare(&mut self, name: &str) {
        if self.singleton > 0 || self.in_def > 0 {
            return;
        }
        let mut end = name.len();
        loop {
            self.out.declared.entry(name[..end].to_string()).or_insert_with(|| self.file.to_string());
            match name[..end].rfind("::") {
                Some(i) => end = i,
                None => break,
            }
        }
    }

    fn declare_path(&mut self, node: &Node<'_>) {
        if let Some((rooted, written)) = written_path(node) {
            let name = self.qualify(rooted, &written);
            self.declare(&name);
        }
    }

    /// The namespace a call on `receiver` acts on: the lexical scope for
    /// none or `self`, the constant's path for a constant.
    fn receiver_namespace(&self, receiver: Option<Node<'_>>) -> Option<String> {
        match receiver {
            None => Some(self.current().unwrap_or("Object").to_string()),
            Some(node) if node.as_self_node().is_some() => Some(self.current().unwrap_or("Object").to_string()),
            Some(node) => {
                let (rooted, written) = written_path(&node)?;
                Some(self.qualify(rooted, &written))
            }
        }
    }

    /// The module whose body the walk is directly in, if it is a module
    /// (not a class, a singleton class or a method).
    fn module_body(&self) -> Option<&str> {
        let current = self.current()?;
        (self.in_def == 0 && self.out.kinds.get(current).is_some_and(|k| k.is_module)).then_some(current)
    }

    fn pending(&mut self, module: &str, extend_from_hook: bool, node: &Node<'_>) {
        let Some((rooted, written)) = written_path(node) else { return };
        let scopes = self.scope.iter().rev().cloned().collect();
        self.out.pending.push(PendingMixin { module: module.to_string(), extend_from_hook, rooted, written, scopes });
    }

    /// `def self.included(base)` (or `def included(base)` in `class <<
    /// self`): each statement of its body that is `base.extend(X, ...)`
    /// with constant arguments, unconditionally, names a module Ruby
    /// extends every includer with. A conditional one (`base.extend
    /// (ModuleMethods) unless base.is_a?(Class)`) is not read.
    fn included_hook(&mut self, module: &str, node: &ruby_prism::DefNode<'_>) {
        let Some(params) = node.parameters() else { return };
        let required: Vec<_> = params.requireds().iter().collect();
        let [base] = required.as_slice() else { return };
        let Some(base) = base.as_required_parameter_node() else { return };
        let base = constant_id_str(&base.name()).to_string();
        let Some(body) = node.body() else { return };
        let Some(statements) = body.as_statements_node() else { return };
        for statement in statements.body().iter() {
            let Some(call) = statement.as_call_node() else { continue };
            if call.name().as_slice() != b"extend" || call.block().is_some() {
                continue;
            }
            let on_base = call
                .receiver()
                .and_then(|r| r.as_local_variable_read_node())
                .is_some_and(|r| constant_id_str(&r.name()) == base);
            if !on_base {
                continue;
            }
            for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
                self.pending(module, true, &arg);
            }
        }
    }

    fn nested(&mut self, path: &Node<'_>, body: Option<Node<'_>>, kind: SourceKind) {
        let Some((rooted, written)) = written_path(path) else { return };
        let name = self.qualify(rooted, &written);
        self.declare(&name);
        if self.singleton == 0 && self.in_def == 0 {
            self.out.kinds.entry(name.clone()).or_insert(kind);
        }
        if let Some(body) = body {
            self.scope.push(name);
            ruby_prism::Visit::visit(self, &body);
            self.scope.pop();
        }
    }
}

fn literal_name(node: &Node<'_>) -> Option<String> {
    if let Some(sym) = node.as_symbol_node() {
        return std::str::from_utf8(sym.unescaped()).ok().map(str::to_string);
    }
    if let Some(s) = node.as_string_node() {
        return std::str::from_utf8(s.unescaped()).ok().map(str::to_string);
    }
    None
}

fn is_constant_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl<'pr> ruby_prism::Visit<'pr> for Walker<'_> {
    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        self.nested(&node.constant_path(), node.body(), SourceKind { is_module: true, parent: None });
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        if let Some(superclass) = node.superclass() {
            ruby_prism::Visit::visit(self, &superclass);
        }
        // The superclass as written (`Tag`, `::Liquid::Tag`); an
        // expression (`Struct.new(...)`) is recorded as dynamic.
        let parent = node.superclass().map(|sup| match written_path(&sup) {
            Some((_, written)) => Some(written),
            None => None,
        });
        let kind = SourceKind { is_module: false, parent: parent.map(|p| p.unwrap_or_default()) };
        self.nested(&node.constant_path(), node.body(), kind);
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        // `class << self` directly in a module body: its `def included`
        // is the module's hook.
        if node.expression().as_self_node().is_some() && self.singleton == 0 {
            if let Some(module) = self.module_body().map(str::to_string) {
                let statements = node.body().and_then(|b| b.as_statements_node());
                for statement in statements.iter().flat_map(|s| s.body().iter()) {
                    if let Some(def) = statement.as_def_node() {
                        if def.receiver().is_none() && def.name().as_slice() == b"included" {
                            self.included_hook(&module, &def);
                        }
                    }
                }
            }
        }
        self.singleton += 1;
        ruby_prism::visit_singleton_class_node(self, node);
        self.singleton -= 1;
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        if self.singleton == 0 {
            if let Some(module) = self.module_body().map(str::to_string) {
                match node.receiver() {
                    None => {
                        let name = constant_id_str(&node.name()).to_string();
                        self.out.mixins.entry(module).or_default().instance_methods.insert(name);
                    }
                    Some(r) if r.as_self_node().is_some() && node.name().as_slice() == b"included" => {
                        self.included_hook(&module, node);
                    }
                    Some(_) => {}
                }
            }
        }
        self.in_def += 1;
        ruby_prism::visit_def_node(self, node);
        self.in_def -= 1;
    }

    fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
        let name = self.qualify(false, constant_id_str(&node.name()));
        self.declare(&name);
        ruby_prism::visit_constant_write_node(self, node);
    }

    fn visit_constant_or_write_node(&mut self, node: &ruby_prism::ConstantOrWriteNode<'pr>) {
        let name = self.qualify(false, constant_id_str(&node.name()));
        self.declare(&name);
        ruby_prism::visit_constant_or_write_node(self, node);
    }

    fn visit_constant_target_node(&mut self, node: &ruby_prism::ConstantTargetNode<'pr>) {
        let name = self.qualify(false, constant_id_str(&node.name()));
        self.declare(&name);
    }

    fn visit_constant_path_write_node(&mut self, node: &ruby_prism::ConstantPathWriteNode<'pr>) {
        self.declare_path(&node.target().as_node());
        ruby_prism::Visit::visit(self, &node.value());
    }

    fn visit_constant_path_or_write_node(&mut self, node: &ruby_prism::ConstantPathOrWriteNode<'pr>) {
        self.declare_path(&node.target().as_node());
        ruby_prism::Visit::visit(self, &node.value());
    }

    fn visit_constant_path_target_node(&mut self, node: &ruby_prism::ConstantPathTargetNode<'pr>) {
        self.declare_path(&node.as_node());
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let method = node.name().as_slice();
        // A module body's `include X` and `attr_*` methods.
        if self.singleton == 0 && node.receiver().is_none() {
            if let Some(module) = self.module_body().map(str::to_string) {
                let args: Vec<Node<'_>> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                match method {
                    b"include" => {
                        for arg in &args {
                            self.pending(&module, false, arg);
                        }
                    }
                    b"attr_reader" | b"attr_writer" | b"attr_accessor" => {
                        let entry = self.out.mixins.entry(module).or_default();
                        for name in args.iter().filter_map(literal_name) {
                            if method != b"attr_writer" {
                                entry.instance_methods.insert(name.clone());
                            }
                            if method != b"attr_reader" {
                                entry.instance_methods.insert(format!("{name}="));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // `private_constant :X, :Y` in a namespace body: `X` is read only
        // lexically, never as `Owner::X`.
        if method == b"private_constant" && self.singleton == 0 && self.in_def == 0 {
            let on_self = node.receiver().is_none_or(|r| r.as_self_node().is_some());
            if let (true, Some(namespace)) = (on_self, self.current().map(str::to_string)) {
                for arg in node.arguments().iter().flat_map(|a| a.arguments().iter()) {
                    if let Some(name) = literal_name(&arg).filter(|n| is_constant_name(n)) {
                        self.out.private.insert(format!("{namespace}::{name}"));
                    }
                }
            }
        }
        if (method == b"autoload" || method == b"const_set") && self.singleton == 0 {
            let first = node.arguments().and_then(|a| a.arguments().iter().next());
            if let (Some(first), Some(namespace)) = (first, self.receiver_namespace(node.receiver())) {
                match literal_name(&first).filter(|n| is_constant_name(n)) {
                    Some(name) if self.in_def == 0 => {
                        let name = if namespace == "Object" { name } else { format!("{namespace}::{name}") };
                        self.declare(&name);
                    }
                    // A literal name set inside a method, or any name a
                    // `const_set` computes: the namespace gets a constant
                    // the source cannot name ahead of the call.
                    _ if method == b"const_set" => {
                        self.out.dynamic.entry(namespace).or_insert_with(|| self.file.to_string());
                    }
                    _ => {}
                }
            }
        }
        ruby_prism::visit_call_node(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> DirScan {
        let mut out = DirScan::default();
        read_file(source.as_bytes(), "lib/x.rb", &mut out);
        out
    }

    #[test]
    fn nesting_writes_and_autoloads_are_qualified() {
        let out = read(
            "module OpenTelemetry\n  module Trace\n    class Tracer; end\n    INVALID = 1\n    autoload :Span, 'x'\n  end\n  class Trace::Link; end\n  ::Top = 1\n  Trace::Status ||= 2\n  class << self\n    HIDDEN = 1\n  end\n  def self.x; INNER = 1; end\nend\n",
        );
        let names: Vec<&str> = out.declared.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "OpenTelemetry",
                "OpenTelemetry::Trace",
                "OpenTelemetry::Trace::INVALID",
                "OpenTelemetry::Trace::Link",
                "OpenTelemetry::Trace::Span",
                "OpenTelemetry::Trace::Status",
                "OpenTelemetry::Trace::Tracer",
                "Top",
            ]
        );
    }

    /// smart_properties 1.17.0's shape: the hook in `class << self`
    /// extends with `ClassMethods` unconditionally and `ModuleMethods`
    /// only for a module includer, which is not read.
    #[test]
    fn an_included_hook_s_extend_is_the_module_s_mixed_in_class_methods() {
        let mut out = read(
            "module SmartProperties\n  module ClassMethods\n    def property(name, **o); end\n    def property!(name, **o); end\n    protected :property\n    attr_reader :props\n  end\n  module ModuleMethods\n    def included(t); end\n  end\n  class << self\n    private\n    def included(base)\n      base.extend(ClassMethods)\n      base.extend(ModuleMethods) unless base.is_a?(Class)\n    end\n  end\n  include Comparable\n  def initialize(*args); end\nend\nmodule Other\n  def self.included(base)\n    base.extend(::SmartProperties::ClassMethods)\n    other.extend(Unrelated)\n  end\nend\n",
        );
        resolve_mixins(&mut out);
        let sp = &out.mixins["SmartProperties"];
        assert_eq!(sp.mixes_in_class_methods, ["SmartProperties::ClassMethods"]);
        assert!(sp.includes.is_empty(), "Comparable is not the gem's: {:?}", sp.includes);
        assert_eq!(sp.instance_methods.iter().collect::<Vec<_>>(), ["initialize"]);
        let class_methods = &out.mixins["SmartProperties::ClassMethods"];
        assert_eq!(class_methods.instance_methods.iter().collect::<Vec<_>>(), ["property", "property!", "props"]);
        assert_eq!(out.mixins["Other"].mixes_in_class_methods, ["SmartProperties::ClassMethods"]);
    }

    #[test]
    fn private_constant_marks_the_namespace_s_constant_private() {
        let out = read("module Timeout\n  class State; end\n  Error = 1\n  private_constant :State, 'Error'\n  def self.x; private_constant :Hidden; end\nend\nprivate_constant :Top\n");
        assert_eq!(out.private.iter().collect::<Vec<_>>(), ["Timeout::Error", "Timeout::State"]);
    }

    #[test]
    fn a_computed_const_set_marks_its_namespace_dynamic() {
        let out = read("module Codes\n  %w[A B].each { |n| const_set(n, 1) }\n  const_set(:Fixed, 2)\nend\n");
        assert!(out.declared.contains_key("Codes::Fixed"));
        assert_eq!(out.dynamic.keys().collect::<Vec<_>>(), ["Codes"]);
    }
}
