//! The resolved database-configuration manifest (`roundhouse.yml`'s
//! `database_configurations:` key) — see [`crate::database`].
//!
//! The manifest is DATA: Rails' own `configs_for(env_name:,
//! include_hidden: true)` for one environment, reduced to the keys the
//! schema selection needs, as `scripts/rails-database-manifest.rb`
//! writes it. Unknown keys are refused, so a credential, a host, or a
//! code body cannot ride along. Nothing here reads models, routing, or
//! any other source: which database a model uses is
//! `ingest::database_ownership`'s, from the app's own classes.
//!
//! Validation is against the tree being ingested: the manifest version,
//! the environment (the emitted harness runs the test environment), the
//! adapters, the extractor that wrote it (version and sha256 of the one
//! shipped beside this binary), and the sha256 of the REQUIRED resolution
//! inputs (the database configuration file Rails loaded, `Gemfile.lock`,
//! each selected schema file) and of every other configuration file
//! named. A required pin that is absent, missing from the tree, or
//! mismatched is a named error at the manifest, never a fallback to the
//! file-existence order. The recorded Rails version is checked against
//! the `activerecord` the lockfile locks, and the Ruby version against
//! its `RUBY VERSION` when it has one. A credential-shaped input or
//! option (`password`, `*_TOKEN`, `*_URL`, a URL with userinfo) is an
//! error: the manifest is credential-free data.
//!
//! What cannot be checked from the tree is reported as recorded but
//! unverified (a warning, never presented as verified): the source
//! revision and tree, a Ruby version the lockfile does not lock, and
//! every input ERB reads. The hashes do NOT prove freshness: ERB in a
//! configuration file can read environment variables and files the
//! manifest does not hash. Regenerate the manifest whenever any input of
//! the resolution changes.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::database::{Database, DatabaseAdapter, DatabaseRefusal, SchemaFormat, SchemaSource};
use crate::span::{FileId, Span};
use crate::vfs::Vfs;
use crate::App;

/// The manifest version this ingest reads.
pub const MANIFEST_VERSION: u32 = 1;
/// The extractor version, and the extractor itself: a manifest must name
/// both, so one written by an older or edited script is refused.
pub const EXTRACTOR_VERSION: u32 = 1;
pub const EXTRACTOR_SOURCE: &str = include_str!("../../scripts/rails-database-manifest.rb");
/// The environment the emitted test harness runs.
const HARNESS_ENV: &str = "test";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    roundhouse_database_manifest: u32,
    env: String,
    #[serde(default)]
    inputs: BTreeMap<String, Option<String>>,
    configurations: Vec<Configuration>,
    provenance: Provenance,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    name: String,
    adapter: String,
    primary: bool,
    replica: bool,
    database_tasks: bool,
    schema_format: String,
    /// As configured: `null` (Rails' default name), a filename, or `false`.
    #[serde(default)]
    #[allow(dead_code)]
    schema_dump: serde_json::Value,
    /// Rails' `DatabaseTasks.schema_dump_path`, recorded for the reader.
    #[serde(default)]
    #[allow(dead_code)]
    schema_dump_path: Option<String>,
    schema_source: Option<DeclaredSchemaSource>,
    #[serde(default)]
    schema_search_path: Option<String>,
    #[serde(default)]
    pragmas: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    migrations_paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredSchemaSource {
    path: String,
    format: String,
    sha256: Option<String>,
    resolved_by: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Provenance {
    source_revision: Option<String>,
    source_tree: Option<String>,
    gemfile_lock: Option<HashedFile>,
    /// The database configuration file Rails loaded; `null` when it
    /// resolved from `DATABASE_URL` alone, which no file pins.
    database_config: Option<HashedFile>,
    extractor: Extractor,
    ruby_version: String,
    rails_version: String,
    command: String,
    config_files: Vec<HashedFile>,
    #[serde(default)]
    withheld_inputs: Vec<String>,
    #[serde(default)]
    unhashed_inputs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Extractor {
    path: String,
    sha256: String,
    version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HashedFile {
    path: String,
    sha256: Option<String>,
}

/// Where a refusal points: the manifest text, at the first occurrence of
/// `needle` (a quoted value), else the file's start.
struct Spans {
    file: FileId,
    text: String,
}

impl Spans {
    fn at(&self, needle: &str) -> Span {
        let quoted = format!("\"{needle}\"");
        match self.text.find(&quoted) {
            Some(i) => Span { file: self.file, start: i as u32, end: (i + quoted.len()) as u32 },
            None => Span { file: self.file, start: 0, end: 0 },
        }
    }

    /// The configuration entry named `name`: its `"name": "<name>"` pair.
    fn config(&self, name: &str) -> Span {
        let quoted = format!("\"{name}\"");
        let mut from = 0;
        while let Some(i) = self.text[from..].find(&quoted).map(|i| i + from) {
            let before = self.text[..i].trim_end();
            if before.ends_with(':') && before[..before.len() - 1].trim_end().ends_with("\"name\"") {
                return Span { file: self.file, start: i as u32, end: (i + quoted.len()) as u32 };
            }
            from = i + quoted.len();
        }
        self.at(name)
    }
}

/// An app-relative path without `..` or a root, as written in the manifest.
fn app_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => out.push(segment),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

const REGENERATE: &str = "regenerate it with scripts/rails-database-manifest.rb";

/// A name whose value is a credential or locates a server with one; the
/// extractor's `CREDENTIAL_NAME`, which withholds the same names.
fn credential_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["PASSWORD", "PASSWD", "PASSPHRASE", "TOKEN", "SECRET", "CREDENTIAL", "PRIVATE"]
        .iter()
        .any(|w| upper.contains(w))
        || ["KEY", "URL", "DSN"].iter().any(|w| upper == *w || upper.ends_with(&format!("_{w}")))
}

/// `scheme://user[:password]@` anywhere in `value`: the extractor's
/// `USERINFO`.
fn carries_userinfo(value: &str) -> bool {
    let mut from = 0;
    while let Some(i) = value[from..].find("://").map(|i| i + from) {
        let scheme = value[..i].rsplit(|c: char| !(c.is_ascii_alphanumeric() || "+.-".contains(c))).next().unwrap_or("");
        let authority = value[i + 3..].split(|c: char| "/?#".contains(c) || c.is_whitespace()).next().unwrap_or("");
        if scheme.starts_with(|c: char| c.is_ascii_alphabetic()) && authority.contains('@') {
            return true;
        }
        from = i + 3;
    }
    false
}

/// The version `Gemfile.lock` locks for gem `name` (a `    name (x)`
/// spec line), and the Ruby its `RUBY VERSION` section names.
fn locked_version(lock: &str, name: &str) -> Option<String> {
    let prefix = format!("    {name} (");
    lock.lines()
        .find_map(|l| l.strip_prefix(&prefix).and_then(|rest| rest.strip_suffix(')')).map(str::to_string))
}

fn locked_ruby(lock: &str) -> Option<String> {
    let mut lines = lock.lines().skip_while(|l| *l != "RUBY VERSION");
    lines.next()?;
    let ruby = lines.next()?.trim().strip_prefix("ruby ")?;
    // `3.4.1p0` → `3.4.1`.
    Some(ruby.split(|c: char| c == 'p' || c.is_whitespace()).next().unwrap_or(ruby).to_string())
}

/// Read and validate the manifest at `manifest_rel` (as `roundhouse.yml`
/// names it), recording `app.databases` and every refusal. Returns the
/// primary's schema file and format when the manifest names one in the
/// tree; the caller ingests it. `config_span` locates the key in
/// `roundhouse.yml`.
pub(super) fn load<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    manifest_rel: &Path,
    config_span: Span,
    app: &mut App,
) -> Option<(PathBuf, SchemaFormat)> {
    let refuse = |app: &mut App, span: Span, detail: String| {
        app.database_refusals.push(DatabaseRefusal {
            span,
            construct: "database_configurations".to_string(),
            detail,
            warning: false,
        });
    };
    let manifest_path = dir.join(manifest_rel);
    let shown = manifest_rel.display().to_string();
    let Ok(bytes) = vfs.read(&manifest_path) else {
        refuse(
            app,
            config_span,
            format!("`roundhouse.yml` names the manifest `{shown}`, which is not in the tree"),
        );
        return None;
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let file = super::sources::register(&manifest_path.display().to_string(), &text);
    let spans = Spans { file, text };
    let whole = Span { file, start: 0, end: 0 };
    let manifest: Manifest = match serde_json::from_str(&spans.text) {
        Ok(m) => m,
        Err(e) => {
            refuse(app, whole, format!("manifest `{shown}` is not a manifest this version reads: {e}"));
            return None;
        }
    };

    if manifest.roundhouse_database_manifest != MANIFEST_VERSION {
        refuse(
            app,
            whole,
            format!(
                "manifest `{shown}` is manifest version {} (this version reads {MANIFEST_VERSION}); {REGENERATE}",
                manifest.roundhouse_database_manifest
            ),
        );
        return None;
    }
    if manifest.env != HARNESS_ENV {
        refuse(
            app,
            spans.at(&manifest.env),
            format!(
                "manifest `{shown}` describes environment `{}`; the emitted harness runs `{HARNESS_ENV}`, so {REGENERATE} under RAILS_ENV={HARNESS_ENV}",
                manifest.env
            ),
        );
    }
    if let Some(Some(rails_env)) = manifest.inputs.get("RAILS_ENV") {
        if *rails_env != manifest.env {
            refuse(
                app,
                spans.at(rails_env),
                format!("manifest `{shown}` was resolved under RAILS_ENV={rails_env} but describes `{}`", manifest.env),
            );
        }
    }
    // Credential-free data: a credential-named input with a value, and
    // any recorded value carrying URL userinfo, are refused.
    for (name, value) in &manifest.inputs {
        if credential_name(name) && value.is_some() {
            refuse(
                app,
                spans.at(name),
                format!("manifest `{shown}` records the value of input `{name}`, which is credential-shaped; a manifest is credential-free data, so {REGENERATE} (it withholds such inputs by name)"),
            );
        } else if value.as_deref().is_some_and(carries_userinfo) {
            refuse(
                app,
                spans.at(name),
                format!("manifest `{shown}` records input `{name}` with URL userinfo (a credential); {REGENERATE}"),
            );
        }
    }
    let extractor = &manifest.provenance.extractor;
    if extractor.version != EXTRACTOR_VERSION {
        refuse(
            app,
            spans.at(&extractor.sha256),
            format!(
                "manifest `{shown}` was written by extractor version {} (this version reads {EXTRACTOR_VERSION}); {REGENERATE}",
                extractor.version
            ),
        );
    } else if extractor.sha256 != crate::sha256::hex(EXTRACTOR_SOURCE.as_bytes()) {
        refuse(
            app,
            spans.at(&extractor.sha256),
            format!(
                "manifest `{shown}` was written by an extractor whose sha256 {} is not the one this version ships; {REGENERATE}",
                extractor.sha256
            ),
        );
    }

    // Pinned files: each must be in the tree with the recorded digest.
    let check_pin = |app: &mut App, path: &str, sha: Option<&str>, what: &str| -> Option<Vec<u8>> {
        let Some(rel) = app_relative(path) else {
            refuse(app, spans.at(path), format!("manifest `{shown}` names {what} `{path}`, which is not app-relative"));
            return None;
        };
        let Ok(bytes) = vfs.read(&dir.join(&rel)) else {
            refuse(app, spans.at(path), format!("manifest `{shown}` pins {what} `{path}`, which is not in the tree; {REGENERATE}"));
            return None;
        };
        let actual = crate::sha256::hex(&bytes);
        match sha {
            Some(expected) if expected == actual => {}
            Some(expected) => refuse(
                app,
                spans.at(expected),
                format!(
                    "manifest `{shown}` is stale: {what} `{path}` has sha256 {actual}, the manifest pins {expected}; {REGENERATE}"
                ),
            ),
            None => refuse(
                app,
                spans.at(path),
                format!("manifest `{shown}` names {what} `{path}` without a sha256; {REGENERATE}"),
            ),
        }
        Some(bytes)
    };
    // Required: the configuration file Rails loaded, and the lockfile.
    match &manifest.provenance.database_config {
        Some(f) => {
            check_pin(app, &f.path, f.sha256.as_deref(), "the database configuration file");
        }
        None => refuse(
            app,
            spans.at("database_config"),
            format!("manifest `{shown}` pins no database configuration file, so nothing ties its resolution to this tree; {REGENERATE}"),
        ),
    }
    for f in &manifest.provenance.config_files {
        check_pin(app, &f.path, f.sha256.as_deref(), "configuration file");
    }
    let mut unverified: Vec<String> = Vec::new();
    match &manifest.provenance.gemfile_lock {
        Some(lock) => {
            if let Some(bytes) = check_pin(app, &lock.path, lock.sha256.as_deref(), "lockfile") {
                let text = String::from_utf8_lossy(&bytes);
                let rails = &manifest.provenance.rails_version;
                match locked_version(&text, "activerecord") {
                    Some(locked) if locked == *rails => {}
                    Some(locked) => refuse(
                        app,
                        spans.at(rails),
                        format!("manifest `{shown}` was resolved under Rails {rails}, but `{}` locks activerecord {locked}; {REGENERATE}", lock.path),
                    ),
                    None => unverified.push(format!("rails_version {rails} (`{}` locks no activerecord)", lock.path)),
                }
                let ruby = &manifest.provenance.ruby_version;
                let recorded = ruby.split_whitespace().next().unwrap_or(ruby);
                match locked_ruby(&text) {
                    Some(locked) if locked == recorded => {}
                    Some(locked) => refuse(
                        app,
                        spans.at(ruby),
                        format!("manifest `{shown}` was resolved under Ruby {recorded}, but `{}` locks Ruby {locked}; {REGENERATE}", lock.path),
                    ),
                    None => unverified.push(format!("ruby_version {recorded} (`{}` has no RUBY VERSION)", lock.path)),
                }
            }
        }
        None => refuse(
            app,
            spans.at("gemfile_lock"),
            format!("manifest `{shown}` pins no lockfile, so the bundle it was resolved under is unknown; {REGENERATE}"),
        ),
    }
    for (what, value) in [
        ("source_revision", &manifest.provenance.source_revision),
        ("source_tree", &manifest.provenance.source_tree),
    ] {
        unverified.push(match value {
            Some(v) => format!("{what} {v}"),
            None => format!("{what} (not recorded)"),
        });
    }
    unverified.push("every input ERB in a configuration file reads beyond the recorded env inputs".to_string());
    app.database_refusals.push(DatabaseRefusal {
        span: whole,
        construct: "database_configurations".to_string(),
        detail: format!(
            "manifest `{shown}` provenance recorded but not verified against the tree: {}; the hashes do not guarantee the manifest is fresh",
            unverified.join("; ")
        ),
        warning: true,
    });

    let mut primary: Option<(PathBuf, SchemaFormat)> = None;
    let mut primaries = 0usize;
    for c in &manifest.configurations {
        let span = spans.config(&c.name);
        let Some(adapter) = DatabaseAdapter::parse(&c.adapter) else {
            refuse(app, span, format!("configuration `{}` names adapter `{}`, which Roundhouse does not know", c.name, c.adapter));
            continue;
        };
        if SchemaFormat::parse(&c.schema_format).is_none() {
            refuse(app, span, format!("configuration `{}` has schema format `{}` (ruby or sql)", c.name, c.schema_format));
        }
        let mut pragmas = Vec::new();
        for (key, value) in &c.pragmas {
            if credential_name(key) || value.as_str().is_some_and(carries_userinfo) {
                refuse(
                    app,
                    span,
                    format!("configuration `{}` option `{key}` is credential-shaped; a manifest is credential-free data, so {REGENERATE}", c.name),
                );
                continue;
            }
            match value {
                serde_json::Value::String(s) => pragmas.push((key.clone(), s.clone())),
                serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                    pragmas.push((key.clone(), value.to_string()))
                }
                _ => refuse(app, span, format!("configuration `{}` pragma `{key}` is not a scalar", c.name)),
            }
        }
        if c.schema_search_path.as_deref().is_some_and(carries_userinfo) {
            refuse(app, span, format!("configuration `{}` schema_search_path carries URL userinfo (a credential)", c.name));
        }
        let mut schema_source = None;
        if let Some(src) = &c.schema_source {
            match SchemaFormat::parse(&src.format) {
                None => refuse(
                    app,
                    spans.at(&src.path),
                    format!("configuration `{}` names schema `{}` in format `{}` (ruby or sql)", c.name, src.path, src.format),
                ),
                Some(format) => {
                    let present = check_pin(app, &src.path, src.sha256.as_deref(), "schema file").is_some();
                    schema_source = Some(SchemaSource {
                        path: src.path.clone(),
                        format,
                        sha256: src.sha256.clone().unwrap_or_default(),
                        resolved_by: src.resolved_by.clone(),
                    });
                    if c.primary && !c.replica && present {
                        primary = app_relative(&src.path).map(|rel| (dir.join(rel), format));
                    }
                }
            }
        }
        if c.primary && !c.replica {
            primaries += 1;
            if c.schema_source.is_none() {
                refuse(
                    app,
                    span,
                    format!(
                        "the primary configuration `{}` names no schema source (Rails dumps none for it, and none was declared with --schema-source), so the harness has no schema to load",
                        c.name
                    ),
                );
            }
            if let Some(why) = adapter.runtime_refusal() {
                refuse(
                    app,
                    span,
                    format!("the primary database `{}` uses adapter `{}`: {why}; every model runs on it", c.name, adapter.name()),
                );
            }
        }
        app.databases.push(Database {
            name: c.name.clone(),
            adapter,
            primary: c.primary,
            replica: c.replica,
            database_tasks: c.database_tasks,
            schema_source,
            schema: None,
            schema_search_path: c.schema_search_path.clone(),
            pragmas,
            migrations_paths: c.migrations_paths.clone(),
        });
    }
    if primaries != 1 {
        refuse(
            app,
            spans.at("configurations"),
            format!("manifest `{shown}` has {primaries} primary configuration(s) for `{}`; Rails has exactly one", manifest.env),
        );
        return None;
    }
    primary
}

impl SchemaFormat {
    fn parse(format: &str) -> Option<Self> {
        match format {
            "ruby" => Some(Self::Ruby),
            "sql" => Some(Self::Sql),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{carries_userinfo, credential_name, locked_ruby, locked_version};

    #[test]
    fn credential_shapes() {
        for name in ["DATABASE_URL", "ARCHIVE_DATABASE_URL", "PGPASSWORD", "db_password", "API_TOKEN", "SECRET_KEY_BASE", "AWS_ACCESS_KEY", "KEY", "SENTRY_DSN", "Credentials"] {
            assert!(credential_name(name), "{name}");
        }
        for name in ["RAILS_ENV", "DB_ADAPTER", "SCHEMA", "TEST_ENV_NUMBER", "KEYSPACE", "URLS_ENABLED", "foreign_keys", "journal_mode"] {
            assert!(!credential_name(name), "{name}");
        }
        assert!(carries_userinfo("postgres://u:p@db/x"));
        assert!(carries_userinfo("see trilogy://root@localhost:3306"));
        assert!(!carries_userinfo("postgres://db/x?user=a@b"));
        assert!(!carries_userinfo("core,public"));
        assert!(!carries_userinfo("a@b"));
    }

    #[test]
    fn lockfile_versions() {
        let lock = "GEM\n  specs:\n    activerecord (8.1.4)\n      activemodel (= 8.1.4)\n    activerecord-import (2.0)\n\nRUBY VERSION\n   ruby 3.4.1p0\n";
        assert_eq!(locked_version(lock, "activerecord").as_deref(), Some("8.1.4"));
        assert_eq!(locked_version(lock, "activemodel"), None, "a dependency line is not a spec");
        assert_eq!(locked_ruby(lock).as_deref(), Some("3.4.1"));
        assert_eq!(locked_ruby("GEM\n"), None);
    }

    #[test]
    fn the_shipped_extractor_declares_the_version_ingest_reads() {
        let declared = format!("VERSION = {}", super::EXTRACTOR_VERSION);
        assert!(super::EXTRACTOR_SOURCE.contains(&declared));
    }
}
