//! Project extensions: gem facts an app supplies instead of the compiler
//! carrying them.
//!
//! `roundhouse.yml` lists extension directories:
//!
//! ```yaml
//! extensions:
//!   - config/roundhouse        # app-relative, or absolute
//! ```
//!
//! An extension directory holds, today, `gems.yml`, whose keys append to the
//! compiler's own gem tables (`project.rs`), and `macros.yml`:
//!
//! ```yaml
//! # (require path, lockfile gem) loaded by the emitted Bundler.require,
//! # where the app's Gemfile.lock resolves the gem.
//! lock_gated_requires:
//!   - {require: "fast_blank", gem: "fast_blank"}
//! # Gems whose load closure reaches Rails, with the evidence. Their
//! # constants are refused by name (error[gem_requires_rails]).
//! rails_dependent:
//!   - {gem: "some-engine", why: "requiring it loads 12 Rails framework files"}
//! # (constant an app/ body names, gem that defines it): the emitted
//! # Gemfile declares the gem on demand.
//! runtime_gems:
//!   - {constant: "Widget", gem: "widget-client"}
//!   - {any_text: ["Widget::Cache."], gem: "widget-cache"}
//! ```
//!
//! ```yaml
//! # macros.yml: class-body DSL calls that add attributes to a model. Each
//! # name the macro's symbol arguments gives is read and written untyped
//! # (the DSL's value object is not modeled).
//! attribute_macros:
//!   - {name: price_column, positional: true}   # price_column :a, :b, opt: 1
//!   - {name: bag_columns, hash_key: attributes} # bag_columns attributes: [:a, :b]
//! ```
//!
//! Unknown keys, an unreadable listed directory and a gem fact that
//! contradicts another are refused by name rather than ignored. An
//! extension never removes a compiler row; rows are appended in file
//! order after the compiler's.
//!
//! The loaded facts are process state (the tables they extend are read
//! from analysis and emit without an app in hand): ingest [`install`]s
//! them for the app it reads, replacing the previous app's.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::vfs::Vfs;

/// What a `runtime_gems` row looks for in an `app/` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemandMarker {
    /// A constant the body names (word-bounded, as `names_constant`).
    Constant(&'static str),
    /// Any of these literal texts.
    AnyText(&'static [&'static str]),
}

/// The gem facts of every extension an app lists, in listing order. Strings
/// are leaked: an extension is loaded once per ingested app and the
/// compiler's own tables are `&'static` too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extensions {
    pub(crate) lock_gated_requires: Vec<(&'static str, &'static str)>,
    pub(crate) rails_dependent: Vec<(&'static str, &'static str)>,
    pub(crate) runtime_gems: Vec<(DemandMarker, &'static str)>,
    pub(crate) attribute_macros: Vec<AttributeMacro>,
}

/// Where a model-body macro names the attributes it defines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MacroAttributes {
    /// The leading symbol arguments.
    Positional,
    /// The symbols in the array under this keyword argument.
    HashKey(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttributeMacro {
    pub(crate) name: &'static str,
    pub(crate) attributes: MacroAttributes,
}

#[derive(Debug)]
pub struct ExtensionError {
    pub file: String,
    pub message: String,
}

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "extension {}: {}", self.file, self.message)
    }
}

impl std::error::Error for ExtensionError {}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct GemsFile {
    lock_gated_requires: Vec<LockGatedRow>,
    rails_dependent: Vec<RailsDependentRow>,
    runtime_gems: Vec<RuntimeGemRow>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct MacrosFile {
    attribute_macros: Vec<AttributeMacroRow>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AttributeMacroRow {
    name: String,
    #[serde(default)]
    positional: bool,
    hash_key: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LockGatedRow {
    require: String,
    gem: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RailsDependentRow {
    gem: String,
    why: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeGemRow {
    gem: String,
    constant: Option<String>,
    #[serde(default)]
    any_text: Vec<String>,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Names a directory entry other than `gems.yml` that a later format
/// revision gives meaning to; refused now so a typo and an unimplemented
/// feature both fail loudly.
const RESERVED_ENTRIES: &[&str] = &["types", "overlay"];

/// Read the extension directories `dirs` (already resolved), in order.
pub fn load<V: Vfs + ?Sized>(vfs: &V, dirs: &[PathBuf]) -> Result<Extensions, ExtensionError> {
    let mut loaded = Extensions::default();
    for dir in dirs {
        let name = dir.display().to_string();
        if !vfs.is_dir(dir) {
            return Err(ExtensionError { file: name, message: "not a directory".into() });
        }
        for reserved in RESERVED_ENTRIES {
            if vfs.exists(&dir.join(reserved)) {
                return Err(ExtensionError {
                    file: dir.join(reserved).display().to_string(),
                    message: format!("{reserved} is not supported by this compiler (only gems.yml is read)"),
                });
            }
        }
        let macros_path = dir.join("macros.yml");
        if vfs.exists(&macros_path) {
            let file = macros_path.display().to_string();
            let text = vfs
                .read_to_string(&macros_path)
                .map_err(|e| ExtensionError { file: file.clone(), message: e.to_string() })?;
            let parsed: Option<MacrosFile> = serde_yaml_ng::from_str(&text)
                .map_err(|e| ExtensionError { file: file.clone(), message: e.to_string() })?;
            for row in parsed.unwrap_or_default().attribute_macros {
                let attributes = match (row.positional, row.hash_key) {
                    (true, None) => MacroAttributes::Positional,
                    (false, Some(key)) if !key.is_empty() => MacroAttributes::HashKey(leak(key)),
                    _ => {
                        return Err(ExtensionError {
                            file,
                            message: format!("attribute_macros row {:?} needs exactly one of positional and hash_key", row.name),
                        });
                    }
                };
                if row.name.is_empty() {
                    return Err(ExtensionError { file, message: "attribute_macros rows need a name".into() });
                }
                loaded.attribute_macros.push(AttributeMacro { name: leak(row.name), attributes });
            }
        }
        let gems_path = dir.join("gems.yml");
        if !vfs.exists(&gems_path) {
            continue;
        }
        let file = gems_path.display().to_string();
        let text = vfs
            .read_to_string(&gems_path)
            .map_err(|e| ExtensionError { file: file.clone(), message: e.to_string() })?;
        let parsed: Option<GemsFile> = serde_yaml_ng::from_str(&text)
            .map_err(|e| ExtensionError { file: file.clone(), message: e.to_string() })?;
        let gems = parsed.unwrap_or_default();
        let bad = |message: String| ExtensionError { file: file.clone(), message };
        for row in gems.lock_gated_requires {
            if row.require.is_empty() || row.gem.is_empty() {
                return Err(bad("lock_gated_requires rows need a non-empty require and gem".into()));
            }
            loaded.lock_gated_requires.push((leak(row.require), leak(row.gem)));
        }
        for row in gems.rails_dependent {
            if row.gem.is_empty() || row.why.is_empty() {
                return Err(bad("rails_dependent rows need a gem and the evidence (why)".into()));
            }
            loaded.rails_dependent.push((leak(row.gem), leak(row.why)));
        }
        for row in gems.runtime_gems {
            let marker = match (row.constant, row.any_text.is_empty()) {
                (Some(constant), true) if !constant.is_empty() => DemandMarker::Constant(leak(constant)),
                (None, false) => {
                    let texts: Vec<&'static str> = row.any_text.into_iter().map(leak).collect();
                    DemandMarker::AnyText(Box::leak(texts.into_boxed_slice()))
                }
                _ => {
                    return Err(bad(format!(
                        "runtime_gems row for {:?} needs exactly one of constant and any_text",
                        row.gem
                    )));
                }
            };
            if row.gem.is_empty() {
                return Err(bad("runtime_gems rows need a gem".into()));
            }
            loaded.runtime_gems.push((marker, leak(row.gem)));
        }
    }
    loaded.validate()?;
    Ok(loaded)
}

impl Extensions {
    /// A gem that reaches Rails is compiled from source or refused, never
    /// required natively, so no fact may also say the tree loads it or that
    /// a runtime port stands in for it.
    fn validate(&self) -> Result<(), ExtensionError> {
        for (gem, _) in &self.rails_dependent {
            let loaded = self.lock_gated_requires.iter().any(|(_, g)| g == gem)
                || self.runtime_gems.iter().any(|(_, g)| g == gem)
                || crate::project::base_tree_loads_gem(gem)
                || crate::project::loaded_by_lock_gated_gem(gem);
            if loaded {
                return Err(ExtensionError {
                    file: "gems.yml".into(),
                    message: format!("{gem:?} is rails_dependent but the tree also loads it"),
                });
            }
            if crate::project::gem_port_files(gem).is_some() {
                return Err(ExtensionError {
                    file: "gems.yml".into(),
                    message: format!("{gem:?} is rails_dependent but a runtime port stands in for it"),
                });
            }
        }
        Ok(())
    }
}

/// An `extensions:` entry as a directory: absolute, or relative to `root`
/// (the directory holding roundhouse.yml).
pub fn resolve_dirs(root: &Path, entries: &[PathBuf]) -> Vec<PathBuf> {
    entries.iter().map(|e| if e.is_absolute() { e.clone() } else { root.join(e) }).collect()
}

static ACTIVE: RwLock<Option<Arc<Extensions>>> = RwLock::new(None);

/// Make `extensions` the facts the compiler's gem tables append, replacing
/// the previous app's.
pub fn install(extensions: Extensions) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(extensions));
}

/// The installed extension facts; empty before any ingest.
pub(crate) fn active() -> Arc<Extensions> {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::MapVfs;

    fn vfs(files: &[(&str, &str)]) -> MapVfs {
        MapVfs::new(files.iter().map(|(path, text)| (PathBuf::from(path), text.as_bytes().to_vec())).collect())
    }

    const TOY: &str = r#"
lock_gated_requires:
  - {require: "widgets/all", gem: "widgets"}
rails_dependent:
  - {gem: "widget-engine", why: "requiring it loads 3 Rails framework files"}
runtime_gems:
  - {constant: "Widget", gem: "widgets"}
  - {any_text: ["Widget::Cache.", "Widget::Store."], gem: "widget-cache"}
"#;

    #[test]
    fn gem_facts_load_in_file_order() {
        let loaded = load(&vfs(&[("ext/gems.yml", TOY)]), &[PathBuf::from("ext")]).unwrap();
        assert_eq!(loaded.lock_gated_requires, vec![("widgets/all", "widgets")]);
        assert_eq!(loaded.rails_dependent, vec![("widget-engine", "requiring it loads 3 Rails framework files")]);
        assert_eq!(loaded.runtime_gems[0], (DemandMarker::Constant("Widget"), "widgets"));
        assert_eq!(loaded.runtime_gems[1].0, DemandMarker::AnyText(&["Widget::Cache.", "Widget::Store."]));
    }

    #[test]
    fn extensions_append_in_listing_order() {
        let one = "lock_gated_requires:\n  - {require: a, gem: a}\n";
        let two = "lock_gated_requires:\n  - {require: b, gem: b}\n";
        let loaded = load(&vfs(&[("one/gems.yml", one), ("two/gems.yml", two)]), &[PathBuf::from("one"), PathBuf::from("two")])
            .unwrap();
        assert_eq!(loaded.lock_gated_requires, vec![("a", "a"), ("b", "b")]);
    }

    #[test]
    fn a_directory_without_gems_yml_loads_empty() {
        let files = vfs(&[("ext/readme.txt", "hi")]);
        assert_eq!(load(&files, &[PathBuf::from("ext")]).unwrap(), Extensions::default());
    }

    #[test]
    fn unknown_keys_are_refused() {
        let err = load(&vfs(&[("ext/gems.yml", "gem_facts: []\n")]), &[PathBuf::from("ext")]).unwrap_err();
        assert!(err.message.contains("unknown field"), "{err}");
        let err = load(&vfs(&[("ext/gems.yml", "rails_dependent:\n  - {gem: g, why: w, extra: 1}\n")]), &[PathBuf::from("ext")])
            .unwrap_err();
        assert!(err.message.contains("unknown field"), "{err}");
    }

    #[test]
    fn a_missing_directory_is_refused() {
        let err = load(&vfs(&[]), &[PathBuf::from("nowhere")]).unwrap_err();
        assert!(err.message.contains("not a directory"), "{err}");
    }

    #[test]
    fn formats_not_yet_implemented_are_refused_by_name() {
        let err = load(&vfs(&[("ext/types/widget.rbs", "class Widget; end\n")]), &[PathBuf::from("ext")]).unwrap_err();
        assert!(err.message.contains("types is not supported"), "{err}");
    }

    #[test]
    fn attribute_macros_name_where_the_attributes_are() {
        let text = "attribute_macros:\n  - {name: price_column, positional: true}\n  - {name: bag_columns, hash_key: attributes}\n";
        let loaded = load(&vfs(&[("ext/macros.yml", text)]), &[PathBuf::from("ext")]).unwrap();
        assert_eq!(
            loaded.attribute_macros,
            vec![
                AttributeMacro { name: "price_column", attributes: MacroAttributes::Positional },
                AttributeMacro { name: "bag_columns", attributes: MacroAttributes::HashKey("attributes") },
            ]
        );
    }

    #[test]
    fn an_attribute_macro_names_exactly_one_argument_shape() {
        for row in ["{name: m}", "{name: m, positional: true, hash_key: k}"] {
            let text = format!("attribute_macros:\n  - {row}\n");
            let err = load(&vfs(&[("ext/macros.yml", &text)]), &[PathBuf::from("ext")]).unwrap_err();
            assert!(err.message.contains("exactly one of positional and hash_key"), "{row}: {err}");
        }
        let err = load(&vfs(&[("ext/macros.yml", "macros: []\n")]), &[PathBuf::from("ext")]).unwrap_err();
        assert!(err.message.contains("unknown field"), "{err}");
    }

    #[test]
    fn a_runtime_gem_row_names_exactly_one_marker() {
        for row in ["{gem: g}", "{gem: g, constant: C, any_text: [x]}"] {
            let text = format!("runtime_gems:\n  - {row}\n");
            let err = load(&vfs(&[("ext/gems.yml", &text)]), &[PathBuf::from("ext")]).unwrap_err();
            assert!(err.message.contains("exactly one of constant and any_text"), "{row}: {err}");
        }
    }

    #[test]
    fn a_rails_dependent_gem_the_tree_loads_is_refused() {
        let text = "rails_dependent:\n  - {gem: g, why: w}\nlock_gated_requires:\n  - {require: g, gem: g}\n";
        let err = load(&vfs(&[("ext/gems.yml", text)]), &[PathBuf::from("ext")]).unwrap_err();
        assert!(err.message.contains("also loads it"), "{err}");
        let err = load(&vfs(&[("ext/gems.yml", "rails_dependent:\n  - {gem: semian, why: w}\n")]), &[PathBuf::from("ext")])
            .unwrap_err();
        assert!(err.message.contains("runtime port"), "{err}");
    }

    #[test]
    fn relative_entries_resolve_against_the_config_directory() {
        let dirs = resolve_dirs(Path::new("/app"), &[PathBuf::from("config/ext"), PathBuf::from("/abs/ext")]);
        assert_eq!(dirs, vec![PathBuf::from("/app/config/ext"), PathBuf::from("/abs/ext")]);
    }
}
