//! The configuration surfaces an application's initializers read, on the
//! PostgreSQL CRuby tree: `config.autoload_paths` / `eager_load_paths` /
//! `autoload_once_paths`, `config.paths[...]` (`expanded`, `to_ary`),
//! `ActiveSupport::Dependencies.autoload_paths`, a framework namespace
//! (`config.action_controller.asset_host = …`) and
//! `app.routes.append { … }`, and the class the app's namespace names
//! (`Sample::Application.config`).
//!
//! railties 8.2.0.alpha `Rails::Application::Configuration` (over
//! `Rails::Engine::Configuration`, `Rails::Paths`) answers them; the
//! emitted boot's `Rails.application.config` answered only the keys the
//! body lifted, so an initializer reading one was a NoMethodError, and
//! `Sample::Application` was an uninitialized constant. The oracle is
//! native Rails booting the same body.
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
  database: rh_e2e_application_config
"#;

/// Each initializer reads a surface a plain `config.k = v` line does not
/// reach and records what it saw on `config`.
const APPLICATION_BODY: &str = r#"module Sample
  class Application < Rails::Application
    config.root = File.expand_path("..", __dir__)
    config.active_record.maintain_test_schema = false
    config.logger = Logger.new(nil)
    config.secret_key_base = "test" * 16

    initializer :before_eager_load do
      config.eager_seen = (config.autoload_paths - config.eager_load_paths).size
      Rails.autoloaders.main.do_not_eager_load(File.expand_path("../lib/skipped", __dir__))
    end

    initializer :prune_autoload_paths, after: :set_autoload_paths do
      unwanted = config.paths["app/views"].expanded + config.paths["app/views"].to_ary
      config.eager_load_paths -= unwanted
      config.autoload_paths -= unwanted
      config.autoload_once_paths -= unwanted
      config.eager_load_paths.freeze
      config.autoload_paths.freeze
      config.autoload_once_paths.freeze
      ActiveSupport::Dependencies.autoload_paths -= unwanted
      ActiveSupport::Dependencies.autoload_once_paths -= unwanted
      config.pruned = [config.paths["app/views"].expanded.map { |p| p.end_with?("/app/views") }, config.paths["app/views"].to_ary]
      config.pruned_frozen = config.autoload_paths.frozen?
    end

    initializer(:configure_asset_host) do
      config.default_asset_host = "cdn.example.com"
      config.asset_host = "https://#{config.default_asset_host}"
      config.action_controller.asset_host = config.asset_host
    end

    initializer(:append_main_identity, after: :add_internal_routes) do |app|
      app.routes.append do
        get "/probe", to: "probe#show"
      end
    end
  end
end
"#;

const TEST: &str = r##"require "test_helper"

class ApplicationConfigTest < ActiveSupport::TestCase
  test "an initializer reads the autoload and eager-load path lists" do
    assert_equal 0, Rails.application.config.eager_seen
  end

  test "an initializer prunes the path lists and the dependencies' lists" do
    assert_equal [[true], ["app/views"]], Rails.application.config.pruned
    assert Rails.application.config.pruned_frozen
    assert_equal [], Rails.application.config.autoload_paths
    assert_kind_of Array, ActiveSupport::Dependencies.autoload_paths
  end

  test "the application's paths answer its log file" do
    assert Rails.application.config.paths["log"].first.end_with?("log/test.log")
  end

  test "a framework namespace an initializer wrote is read back" do
    assert_equal "https://cdn.example.com", Rails.application.config.action_controller.asset_host
    assert_nil Rails.application.config.action_controller.never_set
  end

  test "the app's namespace names the application and its configuration" do
    assert_equal "cdn.example.com", Sample::Application.config.default_asset_host
    assert_equal Rails.application.config.asset_host, Sample::Application.config.asset_host
  end
end
"##;

const TEST_FILES: [&str; 1] = ["test/models/application_config_test.rb"];

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\n".into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/application.rb", APPLICATION_BODY.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".into()),
        ("app/models/note.rb", "class Note < ApplicationRecord\nend\n".into()),
        (TEST_FILES[0], TEST.into()),
    ]
}

/// The native app's own files: Core's pinned Rails booted as an app's
/// config/application.rb boots it (the header, then the body above), and
/// the test helper Rails generates (`require "rails/test_help"`), which
/// also writes each result the way the emitted runner does
/// (ROUNDHOUSE_TEST_RESULTS).
const NATIVE_HEADER: &str = r#"require ENV.fetch("RH_RAILS_ORACLE_RB")
RailsOracle.activate("activesupport", "activemodel", "activerecord", "activejob", "actionpack", "actionview", "railties")
require "rails"
require "active_record/railtie"
require "action_controller/railtie"
"#;

const NATIVE_FILES: [(&str, &str); 2] = [
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
const EXPECTED: [(&str, &str, u64); 5] = [
    ("ApplicationConfigTest#test_a_framework_namespace_an_initializer_wrote_is_read_back", "pass", 2),
    ("ApplicationConfigTest#test_an_initializer_prunes_the_path_lists_and_the_dependencies'_lists", "pass", 4),
    ("ApplicationConfigTest#test_an_initializer_reads_the_autoload_and_eager-load_path_lists", "pass", 1),
    ("ApplicationConfigTest#test_the_app's_namespace_names_the_application_and_its_configuration", "pass", 2),
    ("ApplicationConfigTest#test_the_application's_paths_answer_its_log_file", "pass", 1),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The reopen carries each `initializer` call as written, and the
/// namespace the body sits in names the application.
#[test]
fn the_application_reopen_registers_the_body_initializers_and_the_namespace_constant() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let application = std::fs::read_to_string(emitted.join("config/application.rb")).expect("config/application.rb");
    for name in ["before_eager_load", "prune_autoload_paths", "configure_asset_host", "append_main_identity"] {
        assert!(application.contains(&format!("initializer(:{name}")) || application.contains(&format!("initializer :{name}")), "{name}: {application}");
    }
    assert!(application.contains("module Sample\n  Application = ::Rails::Application\nend"), "{application}");
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


/// The tests run on PostgreSQL, natively (railties 8.2.0.alpha,
/// Shopify/rails 5813559f, with rails/test_help) and emitted, and both
/// answer [`EXPECTED`]: each initializer read the path lists, paths,
/// framework namespace and routes it names, and `Sample::Application`
/// answers the application's configuration.
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn application_config_run_as_natively_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("application-config-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> = files()
        .into_iter()
        .filter(|(path, _)| *path != "config/application.rb")
        .chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string())))
        .chain([("config/application.rb", format!("{NATIVE_HEADER}{APPLICATION_BODY}"))])
        .collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_application_config_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    // Two things are named, not dropped: the appended routes block (this
    // tree's routes are compiled, and drawing a block at run time is not
    // ported) and the eager-load exclusion (its eager load is compiled).
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert_eq!(named.len(), 2, "emitted tree={}\n{}", emitted.display(), named.join("\n"));
    assert!(named[0].contains("error[autoload_exclusion_not_applied]"), "{}", named[0]);
    assert!(named[1].contains("error[gem_railtie_not_run]") && named[1].contains("error[route_set_not_drawn]"), "{}", named[1]);
}
