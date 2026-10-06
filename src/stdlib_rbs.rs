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

use std::collections::{HashMap, HashSet};
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
        let mut namespaces = HashSet::new();
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
        let mut values = HashMap::new();
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
            Some(Ty::Class { id: crate::ident::ClassId(crate::ident::Symbol::from("Encoding")), args: vec![] }));
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
