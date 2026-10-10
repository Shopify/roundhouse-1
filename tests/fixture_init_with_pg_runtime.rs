//! A record built from database values without a query, on the
//! PostgreSQL CRuby tree: Shopify Core's DisconnectedFixtureSet.new_record
//! (components/platform/essentials/test/support/helpers/
//! disconnected_fixture_set.rb) calls
//! `klass.allocate.init_with("attributes" => values).tap(&:clear_changes_information)`,
//! and Core's essentials/lib/patches/active_record/attributes_coercion.rb
//! turns the Hash into an attribute set first, from a load hook its
//! engine's `platform.essentials.patches` initializer registers:
//! `ActiveSupport.on_load(:active_record) { prepend ActiveRecordAttributesCoercion }`.
//! ApplicationRecord's ActiveRecordLegacyYamlDecoder supers into
//! Base#init_with (app/models/concerns/active_record_legacy_yaml_decoder.rb).
//!
//! Before, the ingest dropped that hook in silence (only an `include` in a
//! hooked class's block was carried), and the runtime had no
//! Base#init_with, attributes_builder, clear_changes_information or
//! strict_loading!: every `build_fixture` raised NoMethodError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STRUCTURE: &str = r#"CREATE TABLE notes (
    id bigserial NOT NULL,
    body character varying,
    pinned boolean DEFAULT false NOT NULL,
    created_at timestamp(6) without time zone,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_fixture_init_with
"#;

/// Core's essentials engine loads every lib patch from an initializer
/// (components/platform/essentials/lib/platform/essentials/engine.rb:357).
const ENGINE: &str = r#"module Widgets
  class Engine < ::Rails::Engine
    initializer "widgets.patches" do
      Dir[config.root.join("lib/patches/**/*.rb")].each { |file| require file }
    end
  end
end
"#;

/// Core's attributes_coercion.rb, as written.
const COERCION: &str = r#"module ActiveRecordAttributesCoercion
  def init_with(coder)
    if coder["attributes"].is_a?(Hash)
      coder["attributes"] = self.class
        .attributes_builder.build_from_database(coder["attributes"])
    end
    super
  end
end

ActiveSupport.on_load(:active_record) { prepend ActiveRecordAttributesCoercion }
"#;

/// Core's ActiveRecordLegacyYamlDecoder, less its Psych-coder branches.
const LEGACY_DECODER: &str = r#"module ActiveRecordLegacyYamlDecoder
  def init_with(coder)
    super(ActiveRecordLegacyYamlDecoder.convert(self.class, coder))
  end

  def self.convert(klass, coder)
    return coder unless coder.is_a?(Psych::Coder)

    raise ArgumentError, "legacy YAML for #{klass}"
  end
end
"#;

const TEST: &str = r##"require "test_helper"

class FixtureInitWithTest < ActiveSupport::TestCase
  # DisconnectedFixtureSet.new_record's call.
  def new_record(values)
    Note.allocate.init_with("attributes" => values).tap(&:clear_changes_information)
  end

  def values
    { "id" => 7, "body" => "hi", "pinned" => true, "created_at" => "2020-01-02 03:04:05" }
  end

  test "the patch's load hook prepends it on the base" do
    assert_includes ActiveRecord::Base.ancestors, ActiveRecordAttributesCoercion
    assert_operator Note.ancestors.index(ActiveRecordLegacyYamlDecoder), :<, Note.ancestors.index(ActiveRecordAttributesCoercion)
  end

  test "init_with builds a persisted record from database values" do
    note = new_record(values)
    assert_instance_of Note, note
    assert_equal 7, note.id
    assert_equal "hi", note.body
    assert_equal true, note.pinned
    assert_equal Time.utc(2020, 1, 2, 3, 4, 5), note.created_at
    assert note.persisted?
    assert_not note.new_record?
    assert_not note.changed?
    assert_equal({}, note.saved_changes)
  end

  test "a write after clear_changes_information is a change from the database value" do
    note = new_record(values)
    note.body = "there"
    assert note.changed?
    assert_equal "hi", note.attribute_was("body")
    assert_equal({ "body" => ["hi", "there"] }, note.changes_to_save)
  end

  test "the attribute set the builder makes" do
    set = Note.attributes_builder.build_from_database(values)
    assert set.key?("body")
    assert_equal "hi", set.fetch_value("body")
    assert_equal true, set.fetch_value("pinned")
  end

  test "new_record in the coder" do
    note = Note.allocate.init_with("attributes" => Note.attributes_builder.build_from_database(values), "new_record" => true)
    assert note.new_record?
    assert_equal "hi", note.body
  end

  test "strict_loading! records the flag and mode" do
    note = new_record(values)
    assert_not note.strict_loading?
    note.strict_loading!
    assert note.strict_loading?
    assert_equal :all, note.strict_loading_mode
    note.strict_loading!(false, mode: :n_plus_one_only)
    assert_not note.strict_loading?
    assert note.strict_loading_n_plus_one_only?
    error = assert_raises(ArgumentError) { note.strict_loading!(mode: :bogus) }
    assert_equal "The :mode option must be one of [:all, :n_plus_one_only] but :bogus was provided.", error.message
  end
end
"##;

const TEST_FILES: [&str; 1] = ["test/models/fixture_init_with_test.rb"];

const PATH_GEMS: &str = "PATH\n  remote: components/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  widgets!\n";

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\n".into()),
        ("Gemfile.lock", PATH_GEMS.into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  include ActiveRecordLegacyYamlDecoder\n\n  self.abstract_class = true\nend\n".into(),
        ),
        ("app/models/concerns/active_record_legacy_yaml_decoder.rb", LEGACY_DECODER.into()),
        ("app/models/note.rb", "class Note < ApplicationRecord\nend\n".into()),
        ("components/widgets/lib/widgets/engine.rb", ENGINE.into()),
        ("components/widgets/lib/patches/active_record/attributes_coercion.rb", COERCION.into()),
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
# The path gems' libs, as Bundler puts them on the load path; the
# engines load as Bundler.require would load them.
%w[widgets].each do |name|
  $LOAD_PATH.unshift(File.expand_path("../components/#{name}/lib", __dir__))
  require File.join(name, "engine")
end

module FixtureInitWithApp
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
const EXPECTED: [(&str, &str, u64); 6] = [
    ("FixtureInitWithTest#test_a_write_after_clear_changes_information_is_a_change_from_the_database_value", "pass", 3),
    ("FixtureInitWithTest#test_init_with_builds_a_persisted_record_from_database_values", "pass", 9),
    ("FixtureInitWithTest#test_new_record_in_the_coder", "pass", 2),
    ("FixtureInitWithTest#test_strict_loading!_records_the_flag_and_mode", "pass", 7),
    ("FixtureInitWithTest#test_the_attribute_set_the_builder_makes", "pass", 3),
    ("FixtureInitWithTest#test_the_patch's_load_hook_prepends_it_on_the_base", "pass", 3),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The patch file registers its hook as written; no other statement of
/// it is dropped.
#[test]
fn a_prepend_only_load_hook_is_registered_as_written() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let hits: Vec<String> = walk(&emitted.join("app"))
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter(|text| text.contains("ActiveSupport.on_load(:active_record)"))
        .collect();
    assert_eq!(hits.len(), 1, "tree={}", emitted.display());
    assert!(hits[0].contains("prepend(ActiveRecordAttributesCoercion)") || hits[0].contains("prepend ActiveRecordAttributesCoercion"), "{}", hits[0]);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "rb") {
            out.push(path);
        }
    }
    out
}

/// The result rows the driver printed for `side`, sorted, and its stderr.
fn run_side(side: &str, root: &std::path::Path, database: &str) -> (Vec<(String, String, u64)>, String) {
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
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

/// The test runs on PostgreSQL, natively (activerecord 8.2.0.alpha,
/// Shopify/rails 5813559f) and emitted, and both answer [`EXPECTED`].
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn init_with_from_database_values_answers_as_natively_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fixture-init-with-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_fixture_init_with_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}
