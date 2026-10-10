//! Each engine's test/fixtures on fixture_paths, and Core's fixture
//! files read where Core globs them, on the PostgreSQL CRuby tree.
//!
//! railties 8.2.0.alpha's `add_fixture_paths` puts every engine's
//! existing test/fixtures on ActiveRecord::TestFixtures' fixture_paths
//! (an `on_load(:active_record_fixtures)` hook), and rails/test_help adds
//! the app's test/fixtures last. Core's DisconnectedFixtureSet.load_yaml
//! (components/platform/essentials/test/support/helpers/
//! disconnected_fixture_set.rb) globs
//! `{fixture_paths}<shard_0|primary>/<collection>.{yml,yml.erb}` and reads
//! each file with ActiveSupport::ConfigurationFile.parse. Before, the
//! engine initializer was refused (error[engine_initializer_not_applied]),
//! fixture_paths was the tree's test/fixtures alone, and
//! ConfigurationFile was "constant not supported": every
//! `DisconnectedFixtureSet` read raised MissingFixtureError ('snowdevil'
//! on Shop). The engine's fixture files are carried at their paths as
//! data; an engine with no test/fixtures adds nothing.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STRUCTURE: &str = r#"CREATE TABLE notes (
    id bigserial NOT NULL,
    body character varying,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_engine_fixture_paths
"#;

const ENGINE: &str = "module Widgets\n  class Engine < ::Rails::Engine\n  end\nend\n";
const PLAIN_ENGINE: &str = "module Plain\n  class Engine < ::Rails::Engine\n  end\nend\n";

/// Core's DisconnectedFixtureSet.load_yaml, with the collection and
/// database given, beside a reopen of the case, as Core's
/// essentials test/support/initializers/active_support_test_case.rb has.
const FIXTURE_YAML: &str = r##"module ActiveSupport
  class TestCase
    def fixture_yaml(collection, database)
      FixtureYaml.load_yaml(collection, database)
    end
  end
end

module FixtureYaml
  FIXTURE_EXTENSIONS = "{yml,yml.erb}"

  def self.load_yaml(collection, database)
    paths = ActiveSupport::TestCase.fixture_paths.map { |path| File.join(path, "/") }
    fixture_files = Dir["{#{paths.join(",")}}#{database}/#{collection}.#{FIXTURE_EXTENSIONS}"]
    fixture_files.each_with_object({}) do |fixture_file, fixtures|
      fixtures.merge!(ActiveSupport::ConfigurationFile.parse(fixture_file))
    end
  end
end
"##;

/// Core's essentials shops.yml shape: an ignored DEFAULTS anchor merged
/// into a row, a date, a literal block.
const SHOPS: &str = "_fixture:\n  ignore: DEFAULTS\nDEFAULTS: &DEFAULTS\n  plan: basic\nsnowdevil:\n  <<: *DEFAULTS\n  name: Snowdevil\n  created_at: 2005-05-05\n";
const SHOPS_ERB: &str = "<% 2.times do |i| %>\nshop_<%= i %>:\n  name: Shop <%= i * 3 %>\n  id: <%= 100 + i %>\n<% end %>\n";

const TEST: &str = r##"require "test_helper"
require "support/helpers/fixture_yaml"

class FixturePathsTest < ActiveSupport::TestCase
  test "each engine's test fixtures then the app's" do
    root = File.expand_path(Rails.root.to_s)
    paths = ActiveSupport::TestCase.fixture_paths.map { |path| path.to_s.delete_prefix("#{root}/") }
    assert_equal ["components/widgets/test/fixtures/", "test/fixtures/"], paths
  end

  test "an engine's fixture files are read where core globs them" do
    shops = fixture_yaml("shops", "shard_0")
    assert_equal ["DEFAULTS", "_fixture", "shop_0", "shop_1", "snowdevil"], shops.keys.sort
    assert_equal({ "plan" => "basic", "name" => "Snowdevil", "created_at" => Date.new(2005, 5, 5) }, shops["snowdevil"])
    assert_equal({ "name" => "Shop 3", "id" => 101 }, shops["shop_1"])
    assert_equal ["online_store"], FixtureYaml.load_yaml("api_clients", "primary").keys
    assert_equal({}, FixtureYaml.load_yaml("shops", "primary"))
  end

  test "a fixture's Hash or Array value is written as YAML" do
    connection = ActiveRecord::Base.connection
    assert_equal "---\nplan: basic\n", connection.with_yaml_fallback({ "plan" => "basic" })
    assert_equal "---\n- 1\n- 2\n", connection.with_yaml_fallback([1, 2])
    assert_equal "basic", connection.with_yaml_fallback("basic")
  end
end
"##;

const TEST_FILES: [&str; 1] = ["test/models/fixture_paths_test.rb"];

const PATH_GEMS: &str = "PATH\n  remote: components/plain\n  specs:\n    plain (0.1.0)\n\nPATH\n  remote: components/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  plain!\n  widgets!\n";

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\ntest_paths:\n  - test/support\n".into()),
        ("Gemfile.lock", PATH_GEMS.into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".into()),
        ("app/models/note.rb", "class Note < ApplicationRecord\nend\n".into()),
        ("components/widgets/lib/widgets/engine.rb", ENGINE.into()),
        ("components/widgets/test/fixtures/shard_0/shops.yml", SHOPS.into()),
        ("components/widgets/test/fixtures/shard_0/shops.yml.erb", SHOPS_ERB.into()),
        ("components/widgets/test/fixtures/primary/api_clients.yml", "online_store:\n  title: Online Store\n".into()),
        ("components/plain/lib/plain/engine.rb", PLAIN_ENGINE.into()),
        ("test/support/helpers/fixture_yaml.rb", FIXTURE_YAML.into()),
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
%w[plain widgets].each do |name|
  $LOAD_PATH.unshift(File.expand_path("../components/#{name}/lib", __dir__))
  require File.join(name, "engine")
end

module FixturePathsApp
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
    ("FixturePathsTest#test_a_fixture's_Hash_or_Array_value_is_written_as_YAML", "pass", 3),
    ("FixturePathsTest#test_an_engine's_fixture_files_are_read_where_core_globs_them", "pass", 5),
    ("FixturePathsTest#test_each_engine's_test_fixtures_then_the_app's", "pass", 1),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The engine's fixture files are carried at their paths, and the tree
/// carries the ConfigurationFile port the helper names.
#[test]
fn engine_fixture_files_and_the_configuration_file_port_are_carried() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    for (path, text) in files() {
        if path.starts_with("components/widgets/test/fixtures/") {
            assert_eq!(std::fs::read_to_string(emitted.join(path)).ok().as_deref(), Some(text.as_str()), "{path}");
        }
    }
    assert!(!emitted.join("components/plain/test/fixtures").exists());
    assert!(emitted.join("runtime/active_support_configuration_file.rb").exists(), "port not carried");
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(facades.contains("require_relative \"active_support_configuration_file\"\n"), "{facades}");
    let engine = std::fs::read_to_string(emitted.join("runtime/rails_engine.rb")).expect("rails_engine.rb");
    assert!(engine.contains("ActiveSupport.on_load(:active_record_fixtures) { self.fixture_paths |= [\"#{fixtures}/\"] }"), "{engine}");
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

/// The test runs on PostgreSQL, natively (railties 8.2.0.alpha,
/// Shopify/rails 5813559f, with rails/test_help) and emitted, and both
/// answer [`EXPECTED`]: the widgets engine's test/fixtures precedes the
/// app's on fixture_paths, the plain engine adds nothing, and the
/// engine's YAML and ERB fixture files read through
/// ActiveSupport::ConfigurationFile.
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn engine_fixture_paths_and_fixture_files_answer_as_natively_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("engine-fixture-paths-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_engine_fixture_paths_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}
