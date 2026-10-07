//! The typed boundary to gems the analyzer does not otherwise model.
//!
//! A gem's public surface is not in the app's tree, but it is often on
//! disk anyway: Tapioca writes one RBI per locked gem under
//! `sorbet/rbi/gems/`. Those files are declarations only -- classes,
//! their ancestry, and `sig`-typed method headers -- which is exactly
//! what a boundary needs and nothing more. This is the table
//! [`crate::ingest::rbi`] reads them into.
//!
//! It is kept apart from `App::rbs_signatures` on purpose. That table
//! is the app's OWN sidecar (`sig/**/*.rbs`) and every consumer of it,
//! the RBS emitters included, treats its entries as things the app
//! declares. A gem's methods are not the app's to declare, so they are
//! carried here and only the analyzer's dispatch registry reads them.

use std::collections::BTreeSet;
use crate::det::HashMap;

use serde::{Deserialize, Serialize};

use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

/// Ruby's own top-level names. An RBI that mentions them is patching or
/// extending the language, not declaring a gem class.
pub const RUBY_CORE: &[&str] = &[
    "BasicObject", "Object", "Module", "Class", "Kernel", "Comparable", "Enumerable", "Enumerator",
    "Numeric", "Integer", "Float", "Rational", "Complex", "String", "Symbol", "Array", "Hash",
    "Range", "Regexp", "MatchData", "Struct", "Proc", "Method", "UnboundMethod", "Time", "Date",
    "DateTime", "IO", "File", "Dir", "Exception", "StandardError", "RuntimeError", "NilClass",
    "TrueClass", "FalseClass", "Set", "Random", "Process", "Signal", "Math", "Marshal", "GC",
    "ObjectSpace", "Thread", "Mutex", "Fiber", "Ractor", "Data", "Encoding", "Binding",
    "BigDecimal", "Pathname", "StringIO", "URI", "JSON", "Logger", "Singleton", "Forwardable",
];

/// Ruby's standard-library namespaces that a gem's RBI reopens (json-jwt
/// patches `OpenSSL::PKey::RSA`). The name is the stdlib's, not the
/// gem's, and loading the gem would not make the stdlib part load, so
/// none of them are registered: such a constant is unknown, like every
/// other stdlib constant the analysis does not model. `DRb` and `Prism`
/// are not here: they are the bundled drb and default prism gems, which
/// the CRuby tree loads, so their own RBIs declare them
/// (`project.rs::RUNTIME_GEMS`).
pub const RUBY_STDLIB: &[&str] = &[
    "OpenSSL", "Net", "Digest", "Socket", "Zlib", "Psych", "YAML", "ERB", "CGI", "Base64",
    "SecureRandom", "Open3", "Timeout", "Tempfile", "FileUtils", "Benchmark", "Coverage",
    "Etc", "Find", "Shellwords", "OptionParser", "IPAddr", "Resolv", "PP", "Ripper", "Monitor",
    "WeakRef", "Gem", "Bundler", "Errno",
];

/// Every gem class or module an RBI declares, by qualified name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GemBoundary {
    pub classes: HashMap<ClassId, GemClass>,
    /// For each gem whose RBI was read: the qualified names of the
    /// classes and modules it actually defines something in (methods,
    /// constants, ancestry). A gem in here has told us exactly what it
    /// owns; a bare namespace container (`module Shopify` opened only
    /// to nest a class) owns nothing beside what it nests.
    #[serde(default)]
    pub namespaces: HashMap<String, BTreeSet<String>>,
    /// The locked gems the app's Bundler.require loads natively
    /// (`crate::project::lock_loaded_gems`), with their dependency
    /// closure. Read from the lock, so a gem the app does not resolve is
    /// never in here.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub loaded: BTreeSet<String>,
    /// Each `include` of a module only a Rails-dependent gem declares
    /// (error[gem_requires_rails]): no emitted tree loads that gem, so
    /// the include cannot run. Written by analysis, which decides it
    /// against the whole registry; the Ruby family emits each as a
    /// named raise where the source has the include.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_includes: Vec<RefusedInclude>,
}

/// One refused include: the including class or module, the module as
/// written, and the refusal's text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefusedInclude {
    pub class: ClassId,
    pub include: ClassId,
    pub detail: String,
}

impl GemBoundary {
    /// The refusal for `class`'s `include` of `include`, if analysis
    /// refused it.
    pub fn refused_include(&self, class: &ClassId, include: &ClassId) -> Option<&RefusedInclude> {
        self.refused_includes.iter().find(|r| &r.class == class && &r.include == include)
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Whether an emitted tree loads `gem`, so the constants its RBI
    /// declares run as the gem's: one the runtime loads on every tree
    /// with that gem (`crate::project::ruby_tree_loads_gem`), or one this
    /// app's lock routes to Bundler.require.
    pub fn loads(&self, gem: &str) -> bool {
        crate::project::ruby_tree_loads_gem(gem) || self.loaded.contains(gem)
    }

    /// The gem class or module an app declaration of `name` reopens: a
    /// name the RBI of a gem some emitted tree loads declares, outside
    /// Ruby's core and standard library (an RBI that patches `String`
    /// does not make `String` the gem's). Ruby runs such a declaration's
    /// body on the gem's own class object, after the gem has loaded.
    pub fn loaded_reopen(&self, name: &ClassId) -> Option<&GemClass> {
        let text = name.0.as_str();
        let top = text.split("::").next().unwrap_or(text);
        if RUBY_CORE.contains(&top) || RUBY_STDLIB.contains(&top) {
            return None;
        }
        self.classes.get(name).filter(|class| self.loads(&class.gem))
    }

    /// The gem whose RBI declares the constant `path`: the class or
    /// module itself, else the value constant its owner's body assigns.
    pub fn gem_of(&self, path: &str) -> Option<&str> {
        if let Some(class) = self.classes.get(&ClassId(Symbol::from(path))) {
            return Some(class.gem.as_str());
        }
        let (owner, name) = path.rsplit_once("::")?;
        let class = self.classes.get(&ClassId(Symbol::from(owner)))?;
        class.constants.iter().any(|c| c.as_str() == name).then_some(class.gem.as_str())
    }

    /// Whether `gem`'s RBI accounts for the constant `path`: it defines
    /// it, defines something inside it, or `path` sits inside something
    /// it defines. `None` when its RBI was not read: nothing is known
    /// either way.
    pub fn declares_path(&self, gem: &str, path: &str) -> Option<bool> {
        let owned = self.namespaces.get(gem)?;
        let inside = format!("{path}::");
        if owned.contains(path) || owned.range(inside.clone()..).next().is_some_and(|n| n.starts_with(&inside)) {
            return Some(true);
        }
        Some(path.match_indices("::").any(|(i, _)| owned.contains(&path[..i])))
    }
}

/// One class or module as an RBI declares it.
///
/// Class names inside `Ty`s are as WRITTEN: resolving them needs the
/// registry of every class the analysis knows, which only exists once
/// the analyzer is built.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GemClass {
    /// The gem the declaration came from (a lockfile spec name).
    pub gem: String,
    pub is_module: bool,
    /// The superclass as written, `None` when the RBI names none.
    pub parent: Option<ClassId>,
    /// `include` / `prepend` targets, as written.
    pub includes: Vec<ClassId>,
    /// `extend` targets, as written: their instance methods are this
    /// class's class-side surface.
    pub extends: Vec<ClassId>,
    pub instance_methods: HashMap<Symbol, Ty>,
    pub class_methods: HashMap<Symbol, Ty>,
    /// Constants the body assigns (`DEFAULT = ...`), which are values,
    /// not classes.
    pub constants: Vec<Symbol>,
    /// The surface cannot be enumerated from the declaration: the class
    /// answers `method_missing`, or its superclass is an expression
    /// rather than a name. A lookup that misses is then unknown, not
    /// wrong.
    pub dynamic: bool,
}
