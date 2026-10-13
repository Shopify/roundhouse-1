//! Ruby core and stdlib signatures: the rbs that ships with the pinned
//! Ruby (`rbs/README.md`, `scripts/vendor-rbs`), plus the reviewed
//! `rbs/supplement.rbs`.
//!
//! A signature is a type, not an implementation. The constant resolver
//! indexes these documents so a written `Encoding::UTF_8` or `URI::HTTPS`
//! resolves by Ruby's own name, and value constants take their declared
//! types. Only the Ruby-family trees run on the interpreter that defines
//! them; `project` refuses the names elsewhere, and writes the `require`
//! a stdlib library needs (`requires`) on the CRuby tree.

use crate::det::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::ty::Ty;

/// `(path relative to the repository root, content)`, sorted by path.
static FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/rbs_files.rs"));

/// Every vendored and supplementary signature document.
pub(crate) fn documents() -> impl Iterator<Item = (&'static str, &'static str)> {
    FILES.iter().filter(|(path, _)| path.ends_with(".rbs")).map(|(path, text)| (*path, *text))
}

/// The documents' names and constant types, read once per process.
struct Index {
    aliases: Vec<crate::rbs::NamespaceAlias>,
    values: HashMap<String, Arc<Ty>>,
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut namespaces = HashSet::default();
        let mut aliases = Vec::new();
        let mut constants = Vec::new();
        for (path, text) in documents() {
            // `stdlib_documents_parse` holds every document to this.
            let Ok(names) = crate::rbs::declared_names(text) else {
                debug_assert!(false, "{path} does not parse");
                continue;
            };
            namespaces.extend(names.namespaces);
            namespaces.extend(names.aliases.iter().map(|alias| alias.full_name.clone()));
            aliases.extend(names.aliases);
            constants.extend(names.constants);
        }
        // A declared type the reader cannot map, or one naming a class no
        // document declares (the reader qualifies bare names by a lexical
        // guess), is untyped: the constant exists, its type is gradual.
        let known = |ty: &Ty| {
            let missing = std::cell::Cell::new(false);
            ty.map_class_ids(&|id| {
                missing.set(missing.get() || !namespaces.contains(id.0.as_str()));
                id.clone()
            });
            !missing.get()
        };
        let mut values = HashMap::default();
        for (name, ty) in constants {
            let ty = ty.ok().filter(|ty| known(ty)).unwrap_or(Ty::Untyped);
            values.insert(name, Arc::new(ty));
        }
        Index { aliases, values }
    })
}

/// The declared type of a value constant, by its full declaration name
/// (`File::Constants::FNM_EXTGLOB`).
pub(crate) fn value_type(name: &str) -> Option<&'static Arc<Ty>> {
    index().values.get(name)
}

/// Whether a vendored class declaration includes `module`, as written
/// (`Gem::Version` includes `Comparable`). Read once per process.
pub(crate) fn declares_include(class: &str, module: &str) -> bool {
    static INCLUDES: OnceLock<HashMap<crate::ident::ClassId, Vec<crate::ident::ClassId>>> = OnceLock::new();
    INCLUDES
        .get_or_init(|| {
            let mut out: HashMap<crate::ident::ClassId, Vec<crate::ident::ClassId>> = HashMap::default();
            for (_, text) in documents() {
                for (class, modules) in crate::rbs::parse_app_includes(text).unwrap_or_default() {
                    out.entry(class).or_default().extend(modules);
                }
            }
            out
        })
        .get(&crate::ident::ClassId(crate::ident::Symbol::from(class)))
        .is_some_and(|modules| modules.iter().any(|m| m.0.as_str() == module))
}

/// A class or module the vendored signatures declare, for
/// [`core_instance_method`]: its instance methods (`def`, `alias`) and
/// the names it inherits from, as written, with the namespaces they
/// are written in.
#[derive(Default)]
struct CoreDecl {
    is_module: bool,
    superclass: Option<(String, Vec<String>)>,
    includes: Vec<(String, Vec<String>)>,
    methods: HashSet<String>,
}

fn core_decls() -> &'static HashMap<String, CoreDecl> {
    static DECLS: OnceLock<HashMap<String, CoreDecl>> = OnceLock::new();
    DECLS.get_or_init(|| {
        let mut out: HashMap<String, CoreDecl> = HashMap::default();
        for (_, text) in documents() {
            if let Ok(signature) = ruby_rbs::node::parse(text) {
                for declaration in signature.declarations().iter() {
                    scan_core_decl(&declaration, &mut Vec::new(), &mut out);
                }
            }
        }
        out
    })
}

fn rbs_written(name: &ruby_rbs::node::TypeNameNode<'_>) -> String {
    let namespace = name.namespace();
    let mut segments: Vec<String> = namespace
        .path()
        .iter()
        .filter_map(|segment| match segment {
            ruby_rbs::node::Node::Symbol(symbol) => Some(symbol.as_str().to_string()),
            _ => None,
        })
        .collect();
    segments.push(name.name().as_str().to_string());
    let written = segments.join("::");
    if namespace.absolute() { format!("::{written}") } else { written }
}

fn scan_core_decl(node: &ruby_rbs::node::Node<'_>, cref: &mut Vec<String>, out: &mut HashMap<String, CoreDecl>) {
    use ruby_rbs::node::{AliasKind, MethodDefinitionKind, Node};
    let (is_module, name, superclass, members) = match node {
        Node::Class(class) => (false, rbs_written(&class.name()), class.super_class().map(|s| rbs_written(&s.name())), class.members()),
        Node::Module(module) => (true, rbs_written(&module.name()), None, module.members()),
        _ => return,
    };
    let full = match (name.strip_prefix("::"), cref.last()) {
        (Some(absolute), _) => absolute.to_string(),
        (None, Some(scope)) => format!("{scope}::{name}"),
        (None, None) => name,
    };
    let superclass = superclass.map(|written| (written, cref.clone()));
    cref.push(full.clone());
    let mut includes = Vec::new();
    let mut methods = HashSet::default();
    for member in members.iter() {
        match &member {
            Node::Include(include) => includes.push((rbs_written(&include.name()), cref.clone())),
            Node::MethodDefinition(method)
                if matches!(method.kind(), MethodDefinitionKind::Instance | MethodDefinitionKind::SingletonInstance) =>
            {
                methods.insert(method.name().as_str().to_string());
            }
            Node::Alias(alias) if matches!(alias.kind(), AliasKind::Instance) => {
                methods.insert(alias.new_name().as_str().to_string());
            }
            Node::Class(_) | Node::Module(_) => scan_core_decl(&member, cref, out),
            _ => {}
        }
    }
    cref.pop();
    let entry = out.entry(full).or_default();
    entry.is_module |= is_module;
    if superclass.is_some() {
        entry.superclass = superclass;
    }
    entry.includes.extend(includes);
    entry.methods.extend(methods);
}

/// The declared name `written` (in the namespaces `cref`) names: the
/// innermost enclosing namespace that declares it, else the top level.
fn resolve_core_name(written: &str, cref: &[String]) -> Option<String> {
    let decls = core_decls();
    if let Some(absolute) = written.strip_prefix("::") {
        return decls.contains_key(absolute).then(|| absolute.to_string());
    }
    cref.iter()
        .rev()
        .map(|scope| format!("{scope}::{written}"))
        .chain(std::iter::once(written.to_string()))
        .find(|candidate| decls.contains_key(candidate))
}

/// Whether the vendored signatures declare the class or module `name`.
pub(crate) fn declares_core_namespace(name: &str) -> bool {
    core_decls().contains_key(name.trim_start_matches("::"))
}

/// Whether an instance of core or stdlib class `name` (a module: an
/// includer of it) answers `method` by the vendored signatures: a `def`
/// or `alias` of the class, of a module it includes, or of an ancestor
/// (`Object` when no superclass is written). `Hash` answers `size`
/// (`alias size length`), activemerchant's `Errors < Hash` aliases it.
pub(crate) fn core_instance_method(name: &str, method: &str) -> bool {
    let decls = core_decls();
    let mut seen: HashSet<String> = HashSet::default();
    let mut pending = vec![(name.trim_start_matches("::").to_string(), true)];
    while let Some((node, as_class)) = pending.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        let Some(decl) = decls.get(&node) else { continue };
        if decl.methods.contains(method) {
            return true;
        }
        for (written, cref) in &decl.includes {
            if let Some(module) = resolve_core_name(written, cref) {
                pending.push((module, false));
            }
        }
        if !as_class || decl.is_module {
            continue;
        }
        match &decl.superclass {
            Some((written, cref)) => {
                if let Some(superclass) = resolve_core_name(written, cref) {
                    pending.push((superclass, true));
                }
            }
            None if node == "BasicObject" => {}
            None => pending.push(("Object".to_string(), true)),
        }
    }
    false
}

/// Rubydex's RBS indexer skips `class Queue = Thread::Queue`. The same
/// aliases as a Ruby document, in their lexical scopes, so the resolver
/// declares them.
pub(crate) fn alias_document() -> &'static str {
    static DOCUMENT: OnceLock<String> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let mut out = String::new();
        for alias in &index().aliases {
            for (depth, (is_module, name)) in alias.nesting.iter().enumerate() {
                let keyword = if *is_module { "module" } else { "class" };
                out.push_str(&format!("{}{keyword} {name}\n", "  ".repeat(depth)));
            }
            let depth = alias.nesting.len();
            out.push_str(&format!("{}{} = {}\n", "  ".repeat(depth), alias.new_name, alias.old_name));
            for depth in (0..depth).rev() {
                out.push_str(&format!("{}end\n", "  ".repeat(depth)));
            }
        }
        out
    })
}

/// One `rbs/requires.tsv` row: a stdlib constant that needs a require.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct Require {
    pub constant: &'static str,
    pub feature: &'static str,
    /// The bundled gem the feature loads from, which a Gemfile must name
    /// under bundler. `None` for a feature that loads without one.
    pub bundled_gem: Option<&'static str>,
}

/// The constant → feature rows, each probed on the pinned interpreter
/// when the signatures were vendored.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn requires() -> &'static [Require] {
    static ROWS: OnceLock<Vec<Require>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let text = FILES
            .iter()
            .find(|(path, _)| *path == "rbs/requires.tsv")
            .map_or("", |(_, text)| *text);
        text.lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let constant = fields.next()?;
                let feature = fields.next()?;
                let _library = fields.next()?;
                let kind = fields.next()?;
                Some(Require { constant, feature, bundled_gem: kind.strip_prefix("bundled:") })
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All vendored documents, and the supplement, parse with the RBS
    /// reader the analyzer uses.
    #[test]
    fn stdlib_documents_parse() {
        let mut count = 0;
        for (path, text) in documents() {
            ruby_rbs::node::parse(text).unwrap_or_else(|e| panic!("{path}: {e}"));
            crate::rbs::declared_names(text).unwrap_or_else(|e| panic!("{path}: {e}"));
            count += 1;
        }
        // 84 core + 159 stdlib files of rbs 3.10.0, and the supplement.
        assert_eq!(count, 244);
    }

    #[test]
    fn value_constants_carry_their_declared_types() {
        assert_eq!(value_type("Encoding::UTF_8").map(|ty| (**ty).clone()),
            Some(Ty::Class { id: crate::ident::ClassId(crate::ident::Symbol::from("Encoding")), args: Default::default() }));
        assert_eq!(value_type("File::Constants::FNM_EXTGLOB").map(|ty| (**ty).clone()), Some(Ty::Int));
        assert_eq!(value_type("Math::PI").map(|ty| (**ty).clone()), Some(Ty::Float));
        assert!(alias_document().contains("Queue = Thread::Queue\n"));
    }

    #[test]
    fn the_require_table_names_a_feature_per_constant() {
        let feature = |name: &str| requires().iter().find(|row| row.constant == name).map(|row| row.feature);
        assert_eq!(feature("Benchmark"), Some("benchmark"));
        assert_eq!(feature("OpenSSL"), Some("openssl"));
        assert_eq!(feature("Net::SMTP"), Some("net/smtp"));
        // Core in Ruby 4.0: no require.
        assert_eq!(feature("Set"), None);
        assert_eq!(feature("Encoding"), None);
        assert!(requires().iter().find(|row| row.constant == "Logger").is_some_and(|row| row.bundled_gem == Some("logger")));
    }
}
