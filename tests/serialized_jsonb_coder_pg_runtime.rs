//! `serialize :col, coder: X` on a `jsonb` column, on the PostgreSQL CRuby tree.
//!
//! Core's Launch::ShopSignupRequest declares `serialize :signup_types,
//! coder: VersionedSerializer.column(Array, ...)` (and tracking_information,
//! plan_spec, statsd_tags, extra) over `jsonb` columns. Rails decorates the
//! column's Json type with Type::Serialized: a write stores the JSON text of
//! `coder.dump(value)`, a read hands `coder.load` the parsed JSON. The
//! compiled model took the schema `jsonb` for a schema-less JSON column and
//! never ran the app coder, so `signup_types` answered the coder's payload
//! String (`undefined method 'signup_types' for nil` / `String#[]` with a
//! Symbol / `delete_if for String` in Core). A `jsonb` column with an app
//! coder now takes the declared-type path like a text or bytea one.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STRUCTURE: &str = r#"CREATE TABLE notes (
    id bigserial NOT NULL,
    title character varying,
    tags jsonb,
    info jsonb,
    plain jsonb,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_serialized_jsonb
"#;

/// Shaped like Core's VersionedSerializer columns: the stored payload is a
/// String (there YAML), the loaded value an Array or Hash.
const LIST_CODER: &str = r#"class ListCoder
  def self.load(raw)
    return [] if raw.nil?
    raise ArgumentError, "payload must be a String, got #{raw.class}" unless raw.is_a?(String)
    raw.split(",")
  end

  def self.dump(value)
    value.join(",")
  end
end
"#;

const INFO_CODER: &str = r#"class InfoCoder
  def self.load(raw)
    return if raw.nil?
    raise ArgumentError, "payload must be a String, got #{raw.class}" unless raw.is_a?(String)
    JSON.parse(raw).merge("loaded" => true)
  end

  def self.dump(value)
    value.except("loaded").to_json
  end
end
"#;

const NOTE: &str = "class Note < ApplicationRecord\n  serialize :tags, coder: ListCoder\n  serialize :info, coder: InfoCoder\nend\n";

const TEST: &str = r##"require "test_helper"

class NoteTest < ActiveSupport::TestCase
  test "write dumps through the coder and read loads through it" do
    note = Note.create!(title: "t", tags: ["a", "b"], info: { "user_agent" => "ua" }, plain: { "k" => 1 })
    assert_equal ["a", "b"], note.tags
    note = Note.find(note.id)
    assert_equal ["a", "b"], note.tags
    assert_equal({ "user_agent" => "ua", "loaded" => true }, note.info)
    assert_equal "ua", note.info["user_agent"]
    assert_equal({ "k" => 1 }, note.plain)
    raw = Note.connection.select_value("SELECT tags::text FROM notes WHERE id = #{note.id}")
    assert_equal '"a,b"', raw
    raw = Note.connection.select_value("SELECT info::text FROM notes WHERE id = #{note.id}")
    assert_equal '"{\\"user_agent\\":\\"ua\\"}"', raw
  end

  test "the coder's default stores nil and a missing value reads as the default" do
    note = Note.create!(title: "t").reload
    assert_equal [], note.tags
    assert_nil note.info
    assert_nil Note.connection.select_value("SELECT tags::text FROM notes WHERE id = #{note.id}")
  end

  test "update re-dumps and dirty tracking sees the change" do
    note = Note.create!(title: "t", tags: ["a"]).reload
    note.tags = ["a", "c"]
    assert_equal ["a", "c"], note.tags
    assert note.changed?
    note.save!
    assert_equal ["a", "c"], Note.find(note.id).tags
  end
end
"##;

const TEST_FILES: [&str; 1] = ["test/models/note_test.rb"];

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\n".into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".into()),
        ("app/models/list_coder.rb", LIST_CODER.into()),
        ("app/models/info_coder.rb", INFO_CODER.into()),
        ("app/models/note.rb", NOTE.into()),
        (TEST_FILES[0], TEST.into()),
    ]
}

/// The native app's own files: Core's pinned Rails booted as an app's
/// config/application.rb boots it, and the test helper Rails generates
/// (`require "rails/test_help"`), which also writes each result the way
/// the emitted runner does (ROUNDHOUSE_TEST_RESULTS). The gem home holds
/// every Core gem's Minitest plugin, where Bundler would offer only the
/// bundle's: the oracle loads none of them.
const NATIVE_FILES: [(&str, &str); 3] = [
    (
        "config/application.rb",
        r#"require ENV.fetch("RH_RAILS_ORACLE_RB")
RailsOracle.activate("activesupport", "activemodel", "activerecord", "activejob", "actionpack", "actionview", "railties")
require "rails"
require "active_record/railtie"
require "action_controller/railtie"
require "action_view/railtie"

module SerializedJsonbApp
  class Application < Rails::Application
    config.root = File.expand_path("..", __dir__)
    config.active_record.maintain_test_schema = false
    config.logger = Logger.new(nil)
    config.secret_key_base = "test" * 16
  end
end
"#,
    ),
    ("config/environment.rb", "require_relative \"application\"\nRails.application.initialize!\n"),
    (
        "test/test_helper.rb",
        r##"ENV["RAILS_ENV"] = "test"
require_relative "../config/environment"
require "rails/test_help"
require "json"

module Minitest
  class RoundhouseResults < AbstractReporter
    def record(result)
      status = result.skipped? ? "skip" : result.error? ? "error" : result.passed? ? "pass" : "fail"
      row = { "id" => "#{result.klass}##{result.name}", "status" => status, "assertions" => result.assertions }
      File.open(ENV.fetch("ROUNDHOUSE_TEST_RESULTS"), "a") { |file| file.puts(JSON.generate(row)) }
    end
  end

  def self.load_plugins; end

  def self.plugin_roundhouse_results_init(_options)
    reporter << RoundhouseResults.new
  end
end
Minitest.register_plugin("roundhouse_results")
"##,
    ),
];

/// `SIDE ROOT DATABASE FILE...`: a fresh DATABASE with ROOT's
/// db/structure.sql, each test FILE run from ROOT in its own process (as
/// the unit-1000 runner runs one; emitted, the files the tree's test
/// index names for it), the database dropped; prints the result rows.
const DRIVER: &str = r##"require "pg"
require "json"
require "rbconfig"
admin = PG::Connection.conninfo_parse(ENV.fetch("ROUNDHOUSE_PG_TEST_URL")).each_with_object({}) { |o, h| h[o[:keyword].to_sym] = o[:val] if o[:val] }
side, root, db = ARGV.fetch(0), ARGV.fetch(1), ARGV.fetch(2)
results = File.join(root, "results.jsonl")
File.delete(results) if File.exist?(results)
PG.connect(**admin) { |c| c.exec("DROP DATABASE IF EXISTS #{c.quote_ident(db)} WITH (FORCE)"); c.exec("CREATE DATABASE #{c.quote_ident(db)}") }
PG.connect(**admin.merge(dbname: db)) { |c| c.exec(File.read(File.join(root, "db/structure.sql"))) }
env = { "RAILS_ENV" => "test", "ROUNDHOUSE_TEST_RESULTS" => results, "DATABASE_URL" => nil, "ROUNDHOUSE_DATABASE_URL" => nil }
if side == "native"
  env["DATABASE_URL"] = "postgresql:///#{db}?" + admin.slice(:host, :port, :user).map { |k, v| "#{k}=#{v}" }.join("&")
else
  env["ROUNDHOUSE_DATABASE_URL"] = admin.merge(dbname: db).slice(:host, :port, :dbname, :user).map { |k, v| "#{k}=#{v}" }.join(" ")
end
files = ARGV.drop(3)
if side == "emitted"
  # Each source's emitted files, as bin/run-target maps them.
  index = JSON.parse(File.read(File.join(root, "test/roundhouse_tests.json"))).fetch("sources").to_h { |s| [s.fetch("source"), s.fetch("classes").map { |c| c.fetch("file") }.uniq] }
  files = files.flat_map { |file| index.fetch(file) }
end
ok = files.map { |file| system(env, RbConfig.ruby, "-Itest", file, chdir: root, out: :err) }.all?
PG.connect(**admin) { |c| c.exec("DROP DATABASE IF EXISTS #{c.quote_ident(db)} WITH (FORCE)") }
print File.exist?(results) ? File.read(results) : ""
exit(ok ? 0 : 1)
"##;

/// What native Rails answers (Shopify/rails 5813559f, checked on the task
/// PostgreSQL), so two identical wrong answers still fail.
const EXPECTED: [(&str, &str, u64); 3] = [
    ("NoteTest#test_the_coder's_default_stores_nil_and_a_missing_value_reads_as_the_default", "pass", 3),
    ("NoteTest#test_update_re-dumps_and_dirty_tracking_sees_the_change", "pass", 3),
    ("NoteTest#test_write_dumps_through_the_coder_and_read_loads_through_it", "pass", 7),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The result rows the driver printed for `side`, sorted, and its stderr.
fn run_side(side: &str, root: &std::path::Path, database: &str) -> (Vec<(String, String, u64)>, String) {
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    let oracle = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/rails_oracle.rb");
    let output = std::process::Command::new("ruby")
        .args(["-e", DRIVER, side])
        .arg(root)
        .arg(database)
        .args(TEST_FILES)
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        // The gem home's native extensions (pg) are built against a
        // jemalloc Ruby.
        .env("LD_PRELOAD", &jemalloc)
        .env("RH_RAILS_ORACLE_RB", &oracle)
        .env("RH_RAILS_ORACLE_EXEC", "1")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let log = format!("{side} root={}\nstdout={stdout}\nstderr={}", root.display(), String::from_utf8_lossy(&output.stderr));
    let mut rows: Vec<(String, String, u64)> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let row: serde_json::Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}\n{log}"));
            (row["id"].as_str().unwrap().into(), row["status"].as_str().unwrap().into(), row["assertions"].as_u64().unwrap())
        })
        .collect();
    rows.sort();
    assert!(output.status.success() || !rows.is_empty(), "{log}");
    if rows.iter().any(|(_, status, _)| status != "pass") {
        eprintln!("{log}");
    }
    (rows, String::from_utf8_lossy(&output.stderr).into_owned())
}

/// Removes the native app's scratch tree however the test ends.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The test runs on PostgreSQL, natively (Shopify/rails 5813559f, with
/// rails/test_help) and emitted, and both answer [`EXPECTED`].
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn serialized_jsonb_coder_answers_as_natively_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("serialized-jsonb-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_serialized_jsonb_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}

/// A `jsonb` column with an app coder goes through the declared type; the
/// uncoded `jsonb` column keeps the schema-less JSON accessors.
#[test]
fn a_jsonb_column_with_an_app_coder_takes_the_declared_type_path() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let model = std::fs::read_to_string(emitted.join("app/models/note.rb")).expect("note.rb");
    assert!(model.contains("self.class._attribute_deserialize(\"tags\", value)"), "{model}");
    assert!(model.contains("self.class._attribute_deserialize(\"info\", value)"), "{model}");
    assert!(!model.contains("def tags\n        RoundhouseJsonColumn"), "{model}");
    assert!(model.contains("RoundhouseJsonColumn.load(@plain)"), "{model}");
}
