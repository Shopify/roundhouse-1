//! Database identity and schema source, checkpoint 1.
//!
//! Rails decides which databases an environment has, and which schema
//! file each one loads, from the configuration it resolves at boot. An
//! app states that resolution to Roundhouse as data: a manifest written
//! by `scripts/rails-database-manifest.rb`, named by `roundhouse.yml`'s
//! `database_configurations:` key, and pinned by sha256. These tests pin
//! the five checkpoint-1 claims on a small hand-written app:
//!
//! 1. the manifest's configuration, not which file exists, picks the
//!    primary schema (for a sqlite3 and for a postgresql primary);
//! 2. an app without a manifest keeps today's selection and IR;
//! 3. every stale or mismatched pin is a named error;
//! 4. model ownership follows `connects_to` through abstract parents;
//! 5. each routing shape the runtime does not carry is an error at its
//!    span.
//!
//! Plus the support half of (4): a `connects_to` that resolves to the
//! primary is not an error, so the emitted program must run with it.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::det::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use roundhouse::analyze::{diagnose, Analyzer};
use roundhouse::diagnostic::Severity;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::App;
use serde_json::{json, Value};

/// `sha256sum` of `bytes`: the pins are computed by an independent tool,
/// not by the code under test.
fn sha256(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sha256sum");
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().expect("sha256sum");
    String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_string()
}

fn extractor_sha256() -> String {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/rails-database-manifest.rb");
    sha256(&std::fs::read(&script).expect("the extractor ships in scripts/"))
}

const MANIFEST_PATH: &str = "config/roundhouse/databases.json";
const ROUNDHOUSE_YML: &str = "database_configurations: config/roundhouse/databases.json\n";

const DATABASE_YML: &str = "test:\n  primary:\n    adapter: sqlite3\n    database: tmp/test.sqlite3\n    schema_dump: sqlite/schema.rb\n  archive:\n    adapter: sqlite3\n    database: tmp/archive.sqlite3\n";
const SQLITE_SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"posts\", force: :cascade do |t|\n    t.string \"title\"\n  end\nend\n";
/// What a MySQL dump's prologue looks like to a SQLite-only reader: a
/// statement the structure.sql walker does not model, so selecting this
/// file by existence is an ingest error.
const MYSQL_STRUCTURE: &str = "SET @x = 1;\nPREPARE s FROM 'SELECT 1';\nCREATE TABLE `mysql_only` (`id` bigint NOT NULL);\n";
const PG_STRUCTURE: &str = "CREATE TABLE public.posts (\n    id bigint NOT NULL,\n    title character varying\n);\n";
const WIDGETS_SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";

/// The bundle the fixture's manifest was resolved under: activerecord and
/// Ruby as the manifest records them.
const GEMFILE_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activerecord (8.1.4)\n      activemodel (= 8.1.4)\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  activerecord\n\nRUBY VERSION\n   ruby 4.0.7p0\n";

const APP_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";
const POST: &str = "class Post < ApplicationRecord\nend\n";
const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";

/// One configuration entry, the shape the extractor writes.
fn config(name: &str, adapter: &str, primary: bool, source: Option<(&str, &str, &str)>) -> Value {
    json!({
        "name": name,
        "adapter": adapter,
        "primary": primary,
        "replica": false,
        "database_tasks": true,
        "schema_format": source.map(|s| s.1).unwrap_or("ruby"),
        "schema_dump": null,
        "schema_dump_path": source.map(|s| s.0),
        "schema_source": source.map(|(path, format, text)| json!({
            "path": path, "format": format, "sha256": sha256(text.as_bytes()),
            "resolved_by": "rails: ActiveRecord::Tasks::DatabaseTasks.schema_dump_path",
        })),
        "schema_search_path": null,
        "pragmas": {},
        "migrations_paths": [],
    })
}

/// The resolution's required pins: the configuration file Rails loaded
/// (`config/database.yml`) and the lockfile, as the tree has them.
fn manifest(configurations: Vec<Value>) -> Value {
    json!({
        "roundhouse_database_manifest": 1,
        "env": "test",
        "inputs": { "RAILS_ENV": "test", "DB_ADAPTER": null },
        "configurations": configurations,
        "provenance": {
            "source_revision": "fixture",
            "source_tree": null,
            "gemfile_lock": { "path": "Gemfile.lock", "sha256": sha256(GEMFILE_LOCK.as_bytes()) },
            "database_config": { "path": "config/database.yml", "sha256": sha256(DATABASE_YML.as_bytes()) },
            "extractor": {
                "path": "scripts/rails-database-manifest.rb",
                "sha256": extractor_sha256(),
                "version": 1,
            },
            "ruby_version": "4.0.7",
            "rails_version": "8.1.4",
            "command": "hand-written for tests/multi_database.rs",
            "config_files": [],
            "withheld_inputs": [],
            "unhashed_inputs": [],
        },
    })
}

/// The sqlite3 single-database manifest: primary loads db/sqlite/schema.rb.
fn sqlite_manifest() -> Value {
    manifest(
        vec![config("primary", "sqlite3", true, Some(("db/sqlite/schema.rb", "ruby", SQLITE_SCHEMA)))]
    )
}

/// The base tree: models, routes, the config file and both schema files.
fn base_files() -> Vec<(String, String)> {
    [
        ("app/models/application_record.rb", APP_RECORD),
        ("app/models/post.rb", POST),
        ("app/controllers/application_controller.rb", APP_CONTROLLER),
        ("config/routes.rb", ROUTES),
        ("config/database.yml", DATABASE_YML),
        ("Gemfile.lock", GEMFILE_LOCK),
        ("db/sqlite/schema.rb", SQLITE_SCHEMA),
        ("db/structure.sql", MYSQL_STRUCTURE),
    ]
    .into_iter()
    .map(|(p, c)| (p.to_string(), c.to_string()))
    .collect()
}

fn with_manifest(mut files: Vec<(String, String)>, manifest: &Value) -> Vec<(String, String)> {
    files.push(("roundhouse.yml".into(), ROUNDHOUSE_YML.into()));
    files.push((MANIFEST_PATH.into(), serde_json::to_string_pretty(manifest).unwrap()));
    files
}

fn put(files: &mut Vec<(String, String)>, path: &str, content: &str) {
    files.retain(|(p, _)| p != path);
    files.push((path.to_string(), content.to_string()));
}

struct Checked {
    app: App,
    /// Every error diagnostic, rendered `path:line:col: error[code]: message`.
    errors: Vec<String>,
    warnings: Vec<String>,
}

fn check(files: &[(String, String)]) -> Checked {
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).unwrap_or_else(|e| panic!("ingest failed: {e}"));
    Analyzer::new(&app).analyze(&mut app);
    let diagnostics = diagnose(&app);
    let rendered = |severity: Severity| {
        diagnostics.iter().filter(|d| d.severity == severity).map(|d| d.render(&app.sources)).collect::<Vec<_>>()
    };
    let (errors, warnings) = (rendered(Severity::Error), rendered(Severity::Warning));
    Checked { app, errors, warnings }
}

fn table_names(app: &App) -> Vec<String> {
    app.schema.tables.keys().map(|k| k.as_str().to_string()).collect()
}

/// The error at `location` (a `path:line:` prefix) whose text has every
/// one of `phrases`; panics with all errors when there is none.
fn assert_error_at(errors: &[String], location: &str, phrases: &[&str]) {
    let hit = errors
        .iter()
        .any(|e| e.starts_with(location) && phrases.iter().all(|p| e.contains(p)));
    assert!(hit, "expected an error at {location} naming {phrases:?}; errors:\n{}", errors.join("\n"));
}

// 1. ---------------------------------------------------------------------

/// The manifest names `db/sqlite/schema.rb` for the primary. The tree also
/// has a `db/structure.sql` that today's existence rule would pick (there
/// is no `db/schema.rb`) and that does not ingest.
#[test]
fn manifest_selects_the_primary_schema_by_configuration_not_file_existence() {
    let checked = check(&with_manifest(base_files(), &sqlite_manifest()));
    assert_eq!(table_names(&checked.app), vec!["posts"]);
    assert!(checked.errors.is_empty(), "{}", checked.errors.join("\n"));
    let dbs = serde_json::to_value(&checked.app).unwrap()["databases"].clone();
    assert_eq!(dbs[0]["name"], "primary", "{dbs}");
    assert_eq!(dbs[0]["adapter"], "sqlite3", "{dbs}");
    assert_eq!(dbs[0]["schema_source"]["path"], "db/sqlite/schema.rb", "{dbs}");
    assert_eq!(dbs[0]["schema_source"]["format"], "ruby", "{dbs}");
}

/// A postgresql primary whose configuration says `schema_format: sql`
/// loads `db/structure.sql` (pg_dump DDL) although a `db/schema.rb` with
/// other tables exists. Its models are refused until an emitted runtime
/// adapter for postgresql exists; the refusal is named at the manifest.
#[test]
fn a_postgresql_primary_selects_its_structure_sql_and_is_refused_by_name() {
    let mut files = base_files();
    put(&mut files, "db/structure.sql", PG_STRUCTURE);
    put(&mut files, "db/schema.rb", WIDGETS_SCHEMA);
    let manifest = manifest(
        vec![config("primary", "postgresql", true, Some(("db/structure.sql", "sql", PG_STRUCTURE)))]
    );
    let checked = check(&with_manifest(files, &manifest));
    assert_eq!(table_names(&checked.app), vec!["posts"]);
    assert_error_at(&checked.errors, MANIFEST_PATH, &["`primary`", "no emitted runtime adapter for postgresql yet"]);
    let dbs = serde_json::to_value(&checked.app).unwrap()["databases"].clone();
    assert_eq!(dbs[0]["adapter"], "postgresql", "{dbs}");
    assert_eq!(dbs[0]["schema_source"]["format"], "sql", "{dbs}");
}

// 2. ---------------------------------------------------------------------

/// Without a manifest the existence order is unchanged: `db/schema.rb`
/// first, then `db/structure.sql`. The IR gains no database fields.
#[test]
fn without_a_manifest_the_single_database_selection_is_unchanged() {
    let mut files = base_files();
    put(&mut files, "db/schema.rb", WIDGETS_SCHEMA);
    let checked = check(&files);
    assert_eq!(table_names(&checked.app), vec!["widgets"]);
    let ir = serde_json::to_value(&checked.app).unwrap();
    for key in ["databases", "model_databases", "database_refusals"] {
        assert!(ir.get(key).is_none(), "an app without a manifest serializes `{key}`: {}", ir[key]);
    }

    let mut files = base_files();
    put(&mut files, "db/structure.sql", PG_STRUCTURE);
    let checked = check(&files);
    assert_eq!(table_names(&checked.app), vec!["posts"]);
}

// 3. ---------------------------------------------------------------------

/// Each pin that no longer matches the tree is an error naming the file
/// and telling the reader to regenerate; nothing falls back to the
/// existence order.
#[test]
fn a_stale_or_mismatched_manifest_is_a_named_error() {
    // A configuration file edited after the manifest was written.
    let mut files = with_manifest(base_files(), &sqlite_manifest());
    put(&mut files, "config/database.yml", &DATABASE_YML.replace("tmp/test", "tmp/other"));
    let checked = check(&files);
    assert_error_at(&checked.errors, MANIFEST_PATH, &["config/database.yml", "sha256", "regenerate"]);
    assert_eq!(table_names(&checked.app), vec!["posts"], "a stale pin must not change the selection");

    // The selected schema edited after the manifest was written.
    let mut files = with_manifest(base_files(), &sqlite_manifest());
    put(&mut files, "db/sqlite/schema.rb", WIDGETS_SCHEMA);
    let checked = check(&files);
    assert_error_at(&checked.errors, MANIFEST_PATH, &["db/sqlite/schema.rb", "sha256", "regenerate"]);

    // The selected schema absent.
    let files: Vec<_> = with_manifest(base_files(), &sqlite_manifest())
        .into_iter()
        .filter(|(p, _)| p != "db/sqlite/schema.rb")
        .collect();
    let checked = check(&files);
    assert_error_at(&checked.errors, MANIFEST_PATH, &["db/sqlite/schema.rb", "not in the tree"]);
    assert!(checked.app.schema.tables.is_empty(), "no fallback to db/structure.sql");

    // The required resolution pins: the configuration file Rails loaded
    // and the lockfile. A manifest without one, or whose file the tree
    // lacks or has changed, is an error; an empty `config_files` (the
    // extra files) does not stand in for them.
    let mut m = sqlite_manifest();
    m["provenance"]["database_config"] = Value::Null;
    m["provenance"]["config_files"] = json!([]);
    let checked = check(&with_manifest(base_files(), &m));
    assert_error_at(&checked.errors, MANIFEST_PATH, &["pins no database configuration file"]);
    let mut m = sqlite_manifest();
    m["provenance"].as_object_mut().unwrap().remove("database_config");
    let checked = check(&with_manifest(base_files(), &m));
    assert_error_at(&checked.errors, MANIFEST_PATH, &["pins no database configuration file"]);
    let files: Vec<_> = with_manifest(base_files(), &sqlite_manifest())
        .into_iter()
        .filter(|(p, _)| p != "config/database.yml")
        .collect();
    let checked = check(&files);
    assert_error_at(&checked.errors, MANIFEST_PATH, &["config/database.yml", "not in the tree"]);
    let mut m = sqlite_manifest();
    m["provenance"]["gemfile_lock"] = Value::Null;
    let checked = check(&with_manifest(base_files(), &m));
    assert_error_at(&checked.errors, MANIFEST_PATH, &["pins no lockfile"]);
    let mut files = with_manifest(base_files(), &sqlite_manifest());
    put(&mut files, "Gemfile.lock", &GEMFILE_LOCK.replace("activerecord (8.1.4)", "activerecord (8.1.5)"));
    let checked = check(&files);
    assert_error_at(&checked.errors, MANIFEST_PATH, &["Gemfile.lock", "is stale", "regenerate"]);
    // A lockfile that matches its pin but not the recorded versions.
    for (pointer, value, phrases) in [
        ("/provenance/rails_version", json!("8.0.2"), ["Rails 8.0.2", "activerecord 8.1.4"]),
        ("/provenance/ruby_version", json!("3.3.6 (x86_64-linux)"), ["Ruby 3.3.6", "Ruby 4.0.7"]),
    ] {
        let mut m = sqlite_manifest();
        *m.pointer_mut(pointer).unwrap() = value;
        let checked = check(&with_manifest(base_files(), &m));
        assert_error_at(&checked.errors, MANIFEST_PATH, &phrases);
    }

    // What the tree cannot verify is a warning saying so, never silence.
    let checked = check(&with_manifest(base_files(), &sqlite_manifest()));
    assert!(checked.errors.is_empty(), "{}", checked.errors.join("\n"));
    let unverified: Vec<_> = checked.warnings.iter().filter(|w| w.contains("not verified")).collect();
    assert_eq!(unverified.len(), 1, "{:?}", checked.warnings);
    for phrase in ["source_revision fixture", "source_tree (not recorded)", "ERB", "do not guarantee"] {
        assert!(unverified[0].contains(phrase), "{phrase}: {}", unverified[0]);
    }
    assert!(!unverified[0].contains("ruby_version"), "the lockfile locks Ruby: {}", unverified[0]);

    // Another manifest version, another environment, another extractor.
    for (pointer, value, phrase) in [
        ("/roundhouse_database_manifest", json!(2), "manifest version 2"),
        ("/env", json!("development"), "environment `development`"),
        ("/inputs/RAILS_ENV", json!("production"), "RAILS_ENV"),
        ("/provenance/extractor/version", json!(0), "extractor version 0"),
        ("/provenance/extractor/sha256", json!("00"), "extractor"),
        ("/configurations/0/adapter", json!("oracle"), "adapter `oracle`"),
        ("/configurations/0/adapter", json!("postgis"), "postgis is not postgresql"),
        ("/configurations/0/schema_source/format", json!("yaml"), "format `yaml`"),
    ] {
        let mut m = sqlite_manifest();
        *m.pointer_mut(pointer).unwrap() = value;
        let checked = check(&with_manifest(base_files(), &m));
        assert_error_at(&checked.errors, MANIFEST_PATH, &[phrase]);
    }

    // A credential is not manifest data: not as a configuration key, not
    // as a recorded input (by name or by URL userinfo), not as an option.
    let mut m = sqlite_manifest();
    m["configurations"][0]["password"] = json!("hunter2");
    let checked = check(&with_manifest(base_files(), &m));
    assert_error_at(&checked.errors, MANIFEST_PATH, &["password"]);
    for (input, value) in [
        ("DATABASE_URL", "postgres://db.internal/core"),
        ("PGPASSWORD", "hunter2"),
        ("API_TOKEN", "t0k3n"),
        ("DB_HOST_SPEC", "postgres://core:hunter2@db.internal/core"),
    ] {
        let mut m = sqlite_manifest();
        m["inputs"][input] = json!(value);
        let checked = check(&with_manifest(base_files(), &m));
        assert_error_at(&checked.errors, MANIFEST_PATH, &[&format!("input `{input}`"), "credential"]);
    }
    let mut m = sqlite_manifest();
    m["inputs"]["DATABASE_URL"] = Value::Null;
    let checked = check(&with_manifest(base_files(), &m));
    assert!(checked.errors.is_empty(), "an unset input carries nothing: {}", checked.errors.join("\n"));
    for (key, value) in [("password", json!("hunter2")), ("replica_url", json!("x")), ("note", json!("mysql://root:pw@h/db"))] {
        let mut m = sqlite_manifest();
        m["configurations"][0]["pragmas"][key] = value;
        let checked = check(&with_manifest(base_files(), &m));
        assert_error_at(&checked.errors, MANIFEST_PATH, &[&format!("option `{key}`"), "credential"]);
    }

    // A manifest path the tree does not have.
    let files: Vec<_> = with_manifest(base_files(), &sqlite_manifest())
        .into_iter()
        .filter(|(p, _)| p != MANIFEST_PATH)
        .collect();
    let checked = check(&files);
    assert_error_at(&checked.errors, "roundhouse.yml:1:", &[MANIFEST_PATH]);
}

// 4. ---------------------------------------------------------------------

const ARCHIVE_RECORD: &str = "class ArchiveRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: { writing: :archive }\nend\n";
const COLD_RECORD: &str = "class ColdRecord < ArchiveRecord\n  self.abstract_class = true\nend\n";

fn two_database_files(archive_adapter: &str) -> Vec<(String, String)> {
    let mut files = base_files();
    put(&mut files, "app/models/archive_record.rb", ARCHIVE_RECORD);
    put(&mut files, "app/models/archived_post.rb", "class ArchivedPost < ArchiveRecord\nend\n");
    put(&mut files, "app/models/cold_record.rb", COLD_RECORD);
    put(&mut files, "app/models/cold_post.rb", "class ColdPost < ColdRecord\nend\n");
    let archive_schema = "ActiveRecord::Schema.define do\n  create_table \"archived_posts\", force: :cascade do |t|\n    t.string \"title\"\n  end\nend\n";
    put(&mut files, "db/sqlite/archive_schema.rb", archive_schema);
    let m = manifest(
        vec![
            config("primary", "sqlite3", true, Some(("db/sqlite/schema.rb", "ruby", SQLITE_SCHEMA))),
            config("archive", archive_adapter, false, Some(("db/sqlite/archive_schema.rb", "ruby", archive_schema))),
        ]
    );
    with_manifest(files, &m)
}

#[test]
fn ownership_follows_connects_to_through_abstract_parents() {
    let checked = check(&two_database_files("sqlite3"));
    let owners = serde_json::to_value(&checked.app).unwrap()["model_databases"].clone();
    for model in ["ArchiveRecord", "ArchivedPost", "ColdRecord", "ColdPost"] {
        assert_eq!(owners[model]["named"]["writing"], "archive", "{model}: {owners}");
        assert_eq!(owners[model]["named"]["declared_by"], "ArchiveRecord", "{model}: {owners}");
    }
    assert_eq!(owners["Post"], "primary", "{owners}");
    assert_eq!(owners["ApplicationRecord"], "primary", "{owners}");
    // One refusal, at the declaration, counting what it owns; an
    // abstract class asks for no table of its own.
    let archive: Vec<_> = checked.errors.iter().filter(|e| e.contains("`archive`")).collect();
    assert_eq!(archive.len(), 1, "{}", checked.errors.join("\n"));
    assert_error_at(&checked.errors, "app/models/archive_record.rb:3:", &["`archive`", "sqlite3", "only the primary", "4 model"]);
    assert!(!checked.errors.iter().any(|e| e.contains("archive_records") || e.contains("cold_records")), "{}", checked.errors.join("\n"));
}

#[test]
fn a_non_primary_database_is_refused_by_adapter() {
    let checked = check(&two_database_files("postgresql"));
    assert_error_at(&checked.errors, "app/models/archive_record.rb:3:", &["`archive`", "no emitted runtime adapter for postgresql yet"]);
    for adapter in ["trilogy", "mysql2"] {
        let checked = check(&two_database_files(adapter));
        assert_error_at(
            &checked.errors,
            "app/models/archive_record.rb:3:",
            &["`archive`", &format!("no runtime adapter for {adapter}; not substituted")],
        );
    }
}

// 5. ---------------------------------------------------------------------

#[test]
fn each_routing_shape_the_runtime_does_not_carry_is_refused_at_its_span() {
    let cases: Vec<(&str, &str, &str, &[&str])> = vec![
        (
            "app/models/archive_record.rb",
            "class ArchiveRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: DATABASES\nend\n",
            "app/models/archive_record.rb:3:",
            &["connects_to", "literal"],
        ),
        (
            "app/models/archive_record.rb",
            "class ArchiveRecord < ApplicationRecord\n  self.abstract_class = true\n  if ENV[\"ARCHIVE\"]\n    connects_to database: { writing: :archive }\n  end\nend\n",
            "app/models/archive_record.rb:4:",
            &["connects_to", "condition"],
        ),
        (
            "app/models/archive_record.rb",
            "class ArchiveRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to shards: { one: { writing: :archive } }\nend\n",
            "app/models/archive_record.rb:3:",
            &["connects_to", "shards"],
        ),
        (
            "app/models/archive_record.rb",
            "class ArchiveRecord < ApplicationRecord\n  connects_to database: { writing: :archive }\nend\n",
            "app/models/archive_record.rb:2:",
            &["connects_to", "abstract"],
        ),
        (
            "app/models/archive_record.rb",
            "class ArchiveRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: { writing: :nowhere }\nend\n",
            "app/models/archive_record.rb:3:",
            &["`nowhere`", "manifest"],
        ),
        (
            "app/models/post.rb",
            "class Post < ApplicationRecord\n  def self.archived\n    ApplicationRecord.connected_to(role: :reading) { 1 }\n  end\nend\n",
            "app/models/post.rb:3:",
            &["connected_to", "role and shard switching is not modeled"],
        ),
        (
            "app/models/post.rb",
            "class Post < ApplicationRecord\n  def self.connection_pool\n    super\n  end\nend\n",
            "app/models/post.rb:2:",
            &["connection_pool", "app-defined"],
        ),
        (
            "app/models/post.rb",
            "class Post < ApplicationRecord\n  establish_connection :archive\nend\n",
            "app/models/post.rb:2:",
            &["establish_connection"],
        ),
    ];
    let mut missed = Vec::new();
    for (path, source, location, phrases) in cases {
        let mut files = with_manifest(base_files(), &sqlite_manifest());
        put(&mut files, path, source);
        let checked = check(&files);
        let hit = checked
            .errors
            .iter()
            .any(|e| e.starts_with(location) && phrases.iter().all(|p| e.contains(p)));
        if !hit {
            missed.push(format!("{source:?}: expected {location} {phrases:?}, got {:?}", checked.errors));
        }
    }
    assert!(missed.is_empty(), "{} not refused at their span:\n{}", missed.len(), missed.join("\n"));
}

/// A concern's `class_methods` override reaches every includer; the error
/// is at the concern's `def`, and the includer's ownership says so.
#[test]
fn a_connection_override_from_a_concern_is_refused_at_its_def() {
    let mut files = with_manifest(base_files(), &sqlite_manifest());
    put(
        &mut files,
        "app/models/concerns/routed.rb",
        "module Routed\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def connection_pool\n      super\n    end\n  end\nend\n",
    );
    put(&mut files, "app/models/post.rb", "class Post < ApplicationRecord\n  include Routed\nend\n");
    let checked = check(&files);
    assert_error_at(&checked.errors, "app/models/concerns/routed.rb:5:", &["connection_pool", "app-defined", "Post"]);
    let owners = serde_json::to_value(&checked.app).unwrap()["model_databases"].clone();
    assert_eq!(owners["Post"]["app_defined"]["method"], "connection_pool", "{owners}");
    assert_eq!(owners["Post"]["app_defined"]["defined_by"], "Routed", "{owners}");
}

/// `connects_to` without a manifest has nothing to resolve its names
/// against, so it is refused rather than run on the primary.
#[test]
fn connects_to_without_a_manifest_is_refused() {
    let mut files = base_files();
    put(&mut files, "db/schema.rb", SQLITE_SCHEMA);
    put(&mut files, "app/models/archive_record.rb", ARCHIVE_RECORD);
    let checked = check(&files);
    assert_error_at(&checked.errors, "app/models/archive_record.rb:3:", &["connects_to", "database_configurations"]);
}

// Support half --------------------------------------------------------

/// A `connects_to` whose writing role is the manifest's primary is not
/// refused, which is a claim that the emitted program runs with it: the
/// call is the primary's own connection, the one the harness opens.
#[test]
fn connects_to_the_primary_checks_clean_and_the_emitted_program_runs() {
    let m = manifest(
        vec![config("primary", "sqlite3", true, Some(("db/sqlite/schema.rb", "ruby", SQLITE_SCHEMA)))]
    );
    let mut overlay = emit_and_run::empty_app();
    for (path, content) in with_manifest(base_files(), &m) {
        overlay = overlay.write(&path, &content);
    }
    overlay
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\n  connects_to database: { writing: :primary }\nend\n",
        )
        .run_ruby(
            r#"
Post.create!(title: "kept")
raise "count #{Post.count}" unless Post.count == 1
raise "title" unless Post.first.title == "kept"
puts "primary-owned connects_to ran"
"#,
        )
        .assert_passes();
}
