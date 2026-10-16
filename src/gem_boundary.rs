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

/// How many modules [`GemBoundary::loaded_instance_surface`] visits
/// following `include`s before it stops: a gem's mixin chains are a few
/// modules deep, and a cycle or a pathological chain must not stall the
/// emit.
pub const MIXIN_WALK_LIMIT: usize = 32;

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
    /// What the installed sources of the `loaded` gems declare, read
    /// from the gem home the CRuby tree resolves them in
    /// (`crate::ingest::gem_source_constants`). Unlike the RBIs this needs
    /// no `sorbet/` directory: it is the code that runs.
    #[serde(default, skip_serializing_if = "GemSourceConstants::is_empty")]
    pub source_constants: GemSourceConstants,
}

/// The constants the installed `lib/**/*.rb` of each natively loaded gem
/// declares, fully qualified: `module`/`class` nesting, `X = ...` writes,
/// `autoload :X` and `const_set(:X, ...)` with a literal name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GemSourceConstants {
    /// The gem home the sources were read from.
    pub home: String,
    /// Each gem read, by lockfile spec name.
    pub scans: std::collections::BTreeMap<String, GemSourceScan>,
    /// Fully qualified constant -> gem -> the gem-relative file that
    /// first declares it (in sorted file order).
    pub declared: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// Namespace -> gem -> the file that creates constants in it under a
    /// computed name (`const_set(name, ...)`): what the gem puts there
    /// cannot be read from the source, only that it puts something.
    pub dynamic: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// Class or module -> gem -> how the gem's first `class`/`module`
    /// statement for it opens it.
    #[serde(default)]
    pub kinds: std::collections::BTreeMap<String, std::collections::BTreeMap<String, SourceKind>>,
    /// Module -> gem -> what the gem's source gives it as a mixin
    /// (`SourceMixin`): the source counterpart of an RBI's methods,
    /// includes and `mixes_in_class_methods`.
    #[serde(default)]
    pub mixins: std::collections::BTreeMap<String, std::collections::BTreeMap<String, SourceMixin>>,
    /// Fully qualified constants a gem's namespace body makes private
    /// (`private_constant :State` in `module Timeout`): Ruby reads one only
    /// lexically, so a read reached through a lexical scope stays written.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub private: BTreeSet<String>,
}

/// What a gem's installed source says about one of its modules as a
/// mixin, every body that opens it merged. Names are fully qualified,
/// resolved against the constants the gem declares as Ruby resolves them
/// from the body's lexical scope; one the gem does not declare is left out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceMixin {
    /// The `def`s and `attr_*` methods of its body: its instance methods.
    pub instance_methods: BTreeSet<String>,
    /// `include X` in its body.
    pub includes: Vec<String>,
    /// The `X` of each unconditional `base.extend(X)` statement of its
    /// `self.included(base)` hook (smart_properties' `base.extend
    /// (ClassMethods)`): what Tapioca writes as `mixes_in_class_methods`.
    pub mixes_in_class_methods: Vec<String>,
}

/// How a gem's source opens a class or module: `module`, or `class` with
/// the superclass as written (`Some("")` for an expression superclass,
/// `Struct.new(...)`, which no written name matches).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceKind {
    pub is_module: bool,
    pub parent: Option<String>,
}

/// One gem's read: where, how much, and whether the bounds cut it short.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GemSourceScan {
    pub version: String,
    /// The gem's directory under the gem home.
    pub dir: String,
    pub files: usize,
    pub bytes: u64,
    /// False when the file or byte bound stopped the walk: a constant
    /// the read did not see may still be declared.
    pub complete: bool,
    /// The gem builds a native extension (`ext/`, or its gemspec's
    /// `extensions`), whose classes no Ruby file declares
    /// (google-protobuf's `Google::Protobuf::DescriptorPool`).
    #[serde(default)]
    pub native: bool,
}

/// How a natively loaded gem provides a constant, by its source.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceProvider<'a> {
    /// The gem's source declares exactly this constant (`name`, the
    /// fully qualified form the written one resolved to).
    Declared { name: String, gem: &'a str, file: &'a str },
    /// The gem creates constants under a computed name in `namespace`.
    Dynamic { namespace: String, gem: &'a str, file: &'a str },
    /// The gem's native extension may define it: `namespace` is the
    /// gem's own (named after it, `Google::Protobuf` for google-protobuf)
    /// and no Ruby file declares the constant.
    Native { namespace: String, gem: &'a str },
}

impl SourceProvider<'_> {
    pub fn gem(&self) -> &str {
        match self {
            SourceProvider::Declared { gem, .. } | SourceProvider::Dynamic { gem, .. } | SourceProvider::Native { gem, .. } => gem,
        }
    }

    /// The fact, for a diagnostic about `written`, with the gem's version.
    pub fn describe(&self, written: &str, sources: &GemSourceConstants) -> String {
        let version = |gem: &str| sources.scans.get(gem).map(|s| s.version.clone()).unwrap_or_default();
        match self {
            SourceProvider::Declared { name, gem, file } => format!("{gem} {} declares {name} ({file})", version(gem)),
            SourceProvider::Dynamic { namespace, gem, file } => format!(
                "{gem} {} declares no {written}, but creates constants in {namespace} under computed names ({file}: const_set), so {written} is taken as that gem's by namespace ownership",
                version(gem)
            ),
            SourceProvider::Native { namespace, gem } => format!(
                "{gem} {} declares no {written} in Ruby, but builds a native extension and owns {namespace}, so {written} is taken as that gem's by namespace ownership",
                version(gem)
            ),
        }
    }
}

/// Whether `namespace` is named after `gem`, ignoring case, `::`, `_`
/// and `-` (`Google::Protobuf` and google-protobuf; `Google` is not).
fn namespace_named_after(namespace: &str, gem: &str) -> bool {
    let fold = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let space = fold(namespace);
    !space.is_empty() && fold(gem) == space
}

impl GemSourceConstants {
    pub fn is_empty(&self) -> bool {
        self.scans.is_empty()
    }

    /// How the scanned gems provide `written`, a constant as written in
    /// lexical `scope` (`"A::B"`, or `""` at the top level): the nearest
    /// lexical candidate a gem declares exactly, else the nearest
    /// enclosing namespace of one that a gem fills dynamically.
    pub fn provider(&self, written: &str, scope: &str) -> Option<SourceProvider<'_>> {
        let written = written.strip_prefix("::").unwrap_or(written);
        let mut candidates = Vec::new();
        let mut outer = scope;
        while !outer.is_empty() {
            candidates.push(format!("{outer}::{written}"));
            outer = outer.rsplit_once("::").map_or("", |(o, _)| o);
        }
        candidates.push(written.to_string());
        for name in &candidates {
            if let Some((gem, file)) = self.declared.get(name).and_then(|gems| gems.iter().next()) {
                return Some(SourceProvider::Declared { name: name.clone(), gem, file });
            }
        }
        for name in &candidates {
            let mut namespace = name.as_str();
            while let Some((outer, _)) = namespace.rsplit_once("::") {
                namespace = outer;
                if let Some((gem, file)) = self.dynamic.get(namespace).and_then(|gems| gems.iter().next()) {
                    return Some(SourceProvider::Dynamic { namespace: namespace.to_string(), gem, file });
                }
            }
        }
        for name in &candidates {
            let mut namespace = name.as_str();
            while let Some((outer, _)) = namespace.rsplit_once("::") {
                namespace = outer;
                let Some(gems) = self.declared.get(namespace) else { continue };
                let native = gems.iter().find_map(|(gem, file)| {
                    let owns = self.scans.get(gem).is_some_and(|s| s.native)
                        && (namespace.match_indices("::").map(|(i, _)| &namespace[..i]).chain([namespace])
                            .any(|owned| namespace_named_after(owned, gem))
                            // The gem's entry file opens the top-level
                            // namespace: msgpack's `lib/msgpack.rb` is
                            // `module MessagePack`, whose UnpackError the
                            // C extension defines.
                            || (!namespace.contains("::") && *file == format!("lib/{gem}.rb")));
                    owns.then_some(gem)
                });
                if let Some(gem) = native {
                    return Some(SourceProvider::Native { namespace: namespace.to_string(), gem });
                }
            }
        }
        None
    }

    /// The scanned gem whose source declares exactly `name` (fully
    /// qualified).
    pub fn gem_of(&self, name: &str) -> Option<&str> {
        self.declared.get(name.strip_prefix("::").unwrap_or(name))?.keys().next().map(String::as_str)
    }

    /// The scanned gem whose source declares the nearest enclosing
    /// namespace of `name`, when every gem declaring it was read whole:
    /// then that the constant is absent is a fact about the installed
    /// code, not a gap in the read.
    pub fn complete_namespace_owner(&self, name: &str) -> Option<&str> {
        let mut namespace = name.strip_prefix("::").unwrap_or(name);
        while let Some((outer, _)) = namespace.rsplit_once("::") {
            namespace = outer;
            if let Some(gems) = self.declared.get(namespace) {
                let complete = gems.keys().all(|gem| self.scans.get(gem).is_some_and(|s| s.complete));
                return complete.then(|| gems.keys().next().map(String::as_str)).flatten();
            }
        }
        None
    }
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

    /// The declared class or module a constant `written` inside `scope`
    /// names, looked up as Ruby does from the scope outwards
    /// (`ClassMethods` written in `SmartProperties` is
    /// `SmartProperties::ClassMethods`); a rooted `::X` names only `X`.
    pub fn resolve_written(&self, scope: &str, written: &ClassId) -> Option<ClassId> {
        let text = written.0.as_str();
        if let Some(rooted) = text.strip_prefix("::") {
            let id = ClassId(Symbol::from(rooted));
            return self.classes.contains_key(&id).then_some(id);
        }
        let mut scope: Vec<&str> = scope.split("::").filter(|s| !s.is_empty()).collect();
        loop {
            let candidate = if scope.is_empty() { text.to_string() } else { format!("{}::{text}", scope.join("::")) };
            let id = ClassId(Symbol::from(candidate.as_str()));
            if self.classes.contains_key(&id) {
                return Some(id);
            }
            scope.pop()?;
        }
    }

    /// Whether `name` is a module of a gem the tree loads, outside Ruby's
    /// core and standard library: by its RBI ([`Self::loaded_reopen`]),
    /// else by its installed source ([`Self::loaded_source_reopen`]).
    pub fn is_loaded_gem_module(&self, name: &ClassId) -> bool {
        match self.loaded_reopen(name) {
            Some(class) => class.is_module,
            None => self.loaded_source_reopen(name).is_some_and(|(_, kind)| kind.is_module),
        }
    }

    /// The loaded gem module `name` as a mixin, names resolved: its
    /// instance methods, the modules it includes, and its
    /// `mixes_in_class_methods`. The RBI decides when it declares the
    /// module; otherwise the gem's installed source (`SourceMixin`).
    /// `None` when `name` is no loaded gem's module.
    pub fn loaded_mixin(&self, name: &ClassId) -> Option<LoadedMixin> {
        if let Some(class) = self.loaded_reopen(name) {
            if !class.is_module {
                return None;
            }
            let resolve = |list: &[ClassId]| -> Vec<ClassId> {
                list.iter().filter_map(|w| self.resolve_written(name.0.as_str(), w)).collect()
            };
            return Some(LoadedMixin {
                instance_methods: class.instance_methods.keys().cloned().collect(),
                includes: resolve(&class.includes),
                mixes_in_class_methods: resolve(&class.mixes_in_class_methods),
            });
        }
        let (gem, kind) = self.loaded_source_reopen(name)?;
        if !kind.is_module {
            return None;
        }
        let text = name.0.as_str().trim_start_matches("::");
        let Some(mixin) = self.source_constants.mixins.get(text).and_then(|gems| gems.get(gem)) else {
            return Some(LoadedMixin::default());
        };
        let ids = |list: &[String]| -> Vec<ClassId> { list.iter().map(|n| ClassId(Symbol::from(n.as_str()))).collect() };
        Some(LoadedMixin {
            instance_methods: mixin.instance_methods.iter().map(|m| Symbol::from(m.as_str())).collect(),
            includes: ids(&mixin.includes),
            mixes_in_class_methods: ids(&mixin.mixes_in_class_methods),
        })
    }

    /// The instance methods of the loaded gem module `module` and of the
    /// loaded gem modules it includes, transitively: what `extend module`
    /// gives the extended object as its singleton methods. Modules outside
    /// a loaded gem ([`Self::loaded_mixin`]) contribute nothing; the walk
    /// stops after [`MIXIN_WALK_LIMIT`] modules.
    pub fn loaded_instance_surface(&self, module: &ClassId) -> BTreeSet<Symbol> {
        let mut out = BTreeSet::new();
        let mut seen: Vec<ClassId> = Vec::new();
        let mut queue = vec![module.clone()];
        while let Some(id) = queue.pop() {
            if seen.contains(&id) || seen.len() >= MIXIN_WALK_LIMIT {
                continue;
            }
            seen.push(id.clone());
            let Some(mixin) = self.loaded_mixin(&id) else { continue };
            out.extend(mixin.instance_methods);
            queue.extend(mixin.includes.into_iter().rev());
        }
        out
    }

    /// The class-side methods `include module` gives the includer when
    /// `module` is a loaded gem module that mixes class methods in
    /// (`mixes_in_class_methods M` in its RBI, `base.extend(M)` in its
    /// source's `included(base)` hook): Ruby runs the gem's hook, which
    /// extends `base` with `M`, so `M`'s instance surface
    /// ([`Self::loaded_instance_surface`]) becomes the includer's class
    /// methods. Empty for anything else.
    pub fn loaded_mixed_in_class_methods(&self, module: &ClassId) -> BTreeSet<Symbol> {
        let Some(mixin) = self.loaded_mixin(module) else { return BTreeSet::new() };
        mixin.mixes_in_class_methods.iter().flat_map(|m| self.loaded_instance_surface(m)).collect()
    }

    /// [`Self::loaded_reopen`] for a tree with no RBI for the gem: the
    /// class or module the installed source of a gem the CRuby tree loads
    /// opens (`GemSourceConstants::kinds`), with the gem. Every gem read
    /// there is one the tree loads (Bundler.require's closure, or a gem a
    /// gem source requires).
    pub fn loaded_source_reopen(&self, name: &ClassId) -> Option<(&str, &SourceKind)> {
        let text = name.0.as_str().trim_start_matches("::");
        let top = text.split("::").next().unwrap_or(text);
        if RUBY_CORE.contains(&top) || RUBY_STDLIB.contains(&top) {
            return None;
        }
        self.source_constants.kinds.get(text)?.iter().next().map(|(gem, kind)| (gem.as_str(), kind))
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

/// A loaded gem module as a mixin ([`GemBoundary::loaded_mixin`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedMixin {
    pub instance_methods: Vec<Symbol>,
    /// Included modules, fully qualified.
    pub includes: Vec<ClassId>,
    /// `mixes_in_class_methods` modules, fully qualified.
    pub mixes_in_class_methods: Vec<ClassId>,
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
    /// `mixes_in_class_methods` targets: the modules whose instance
    /// methods become the class-side surface of whatever includes this
    /// module, which the gem's `self.included(base)` hook gives the
    /// includer (`base.extend(ClassMethods)`); Tapioca writes the hook
    /// as this annotation. A rooted name (`::SmartProperties::
    /// ClassMethods`) keeps its leading `::`; any other is as written,
    /// resolved from this module's scope outwards
    /// ([`GemBoundary::resolve_written`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixes_in_class_methods: Vec<ClassId>,
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

#[cfg(test)]
mod native_provider_tests {
    use super::*;

    fn sources(gem: &str, namespace: &str, file: &str, native: bool) -> GemSourceConstants {
        let mut out = GemSourceConstants::default();
        out.declared.entry(namespace.to_string()).or_default().insert(gem.to_string(), file.to_string());
        out.scans.insert(gem.to_string(), GemSourceScan { native, complete: true, ..GemSourceScan::default() });
        out
    }

    #[test]
    fn a_native_gems_entry_namespace_owns_what_its_extension_defines() {
        // msgpack's `lib/msgpack.rb` opens `module MessagePack`; the C
        // extension defines `MessagePack::UnpackError`.
        let owned = sources("msgpack", "MessagePack", "lib/msgpack.rb", true);
        let found = owned.provider("MessagePack::UnpackError", "Paquito");
        assert_eq!(found, Some(SourceProvider::Native { namespace: "MessagePack".into(), gem: "msgpack" }));
    }

    #[test]
    fn a_namespace_a_non_entry_file_opens_is_not_owned() {
        let core = sources("msgpack", "Hash", "lib/msgpack/core_ext.rb", true);
        assert_eq!(core.provider("Hash::Missing", ""), None);
        let pure = sources("msgpack", "MessagePack", "lib/msgpack.rb", false);
        assert_eq!(pure.provider("MessagePack::UnpackError", ""), None);
    }
}
