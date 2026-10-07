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

use crate::gem_boundary::{GemSourceConstants, GemSourceScan};
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
        let (require_paths, extensions) = gemspec(vfs, home, gem, version);
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
/// when it names none, and whether it builds `extensions`.
fn gemspec<V: Vfs + ?Sized>(vfs: &V, home: &Path, gem: &str, version: &str) -> (Vec<String>, bool) {
    let spec = home.join("specifications").join(format!("{gem}-{version}.gemspec"));
    let text = vfs.read_to_string(&spec).ok();
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
    out
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

    fn nested(&mut self, path: &Node<'_>, body: Option<Node<'_>>) {
        let Some((rooted, written)) = written_path(path) else { return };
        let name = self.qualify(rooted, &written);
        self.declare(&name);
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
        self.nested(&node.constant_path(), node.body());
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        if let Some(superclass) = node.superclass() {
            ruby_prism::Visit::visit(self, &superclass);
        }
        self.nested(&node.constant_path(), node.body());
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        self.singleton += 1;
        ruby_prism::visit_singleton_class_node(self, node);
        self.singleton -= 1;
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
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

    #[test]
    fn a_computed_const_set_marks_its_namespace_dynamic() {
        let out = read("module Codes\n  %w[A B].each { |n| const_set(n, 1) }\n  const_set(:Fixed, 2)\nend\n");
        assert!(out.declared.contains_key("Codes::Fixed"));
        assert_eq!(out.dynamic.keys().collect::<Vec<_>>(), ["Codes"]);
    }
}
