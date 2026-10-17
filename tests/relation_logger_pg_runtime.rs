//! Three Rails surfaces a unit test reaches through a model, on the
//! PostgreSQL CRuby tree, native and emitted:
//!
//! - `logger` on a record and on its class (`ActiveRecord::Base.logger`,
//!   core.rb's `cattr_accessor :logger`, which railties' "active_record.logger"
//!   fills from `Rails.logger`): Core's `Option#move_to_end` calls
//!   `logger.debug(...)` and raised NoMethodError;
//! - `relation.take(n)`, which answers an Array of up to n rows
//!   (Core's `shop.products.where(...).take(max)`; the runtime's `take`
//!   took no argument, ArgumentError);
//! - a model macro that declares `class_attribute` and writes it
//!   (Core's `has_tags`), replayed on the model so `Model.tag_options`
//!   answers.
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
  database: rh_e2e_relation_logger
"#;

const APPLICATION_BODY: &str = r#"module Sample
  class Application < Rails::Application
    config.root = File.expand_path("..", __dir__)
    config.active_record.maintain_test_schema = false
    config.logger = Logger.new(nil)
    config.secret_key_base = "test" * 16
  end
end
"#;

const TAGGED: &str = r##"module Tagged
  extend ActiveSupport::Concern

  module ClassMethods
    def has_tags(tag_associate_name = nil, options = {})
      class_attribute :tag_associate_name, :tag_options
      self.tag_associate_name = tag_associate_name
      self.tag_options = options
    end
  end
end
"##;

const NOTE: &str = "class Note < ApplicationRecord\n  include Tagged\n\n  has_tags :note_tags, limit: 5\n\n  def self.few(count)\n    where(\"id > 0\").take(count)\n  end\n\n  def self.one\n    where(\"id > 0\").take\n  end\n\n  def log_it\n    logger.debug(\"note #{id}\")\n    logger\n  end\nend\n";

const TEST: &str = r##"require "test_helper"

class RelationLoggerTest < ActiveSupport::TestCase
  test "a record and its class answer the logger" do
    note = Note.create!(body: "a")
    assert_same Rails.logger, Note.logger
    assert_same Note.logger, note.log_it
    assert_same ActiveRecord::Base.logger, note.logger
  end

  test "take with a limit answers an Array of up to n rows" do
    %w[a b c].each { |body| Note.create!(body: body) }
    taken = Note.few(2)
    assert_kind_of Array, taken
    assert_equal 2, taken.length
    assert_equal 3, Note.few(10).length
    assert_kind_of Note, Note.one
  end

  test "a macro's class_attribute is declared on the model" do
    assert_equal({ limit: 5 }, Note.tag_options)
    assert_equal :note_tags, Note.tag_associate_name
  end
end
"##;

const TEST_FILES: [&str; 1] = ["test/models/relation_logger_test.rb"];

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\n".into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/application.rb", APPLICATION_BODY.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".into()),
        ("app/models/concerns/tagged.rb", TAGGED.into()),
        ("app/models/note.rb", NOTE.into()),
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

/// What native Rails answers (Shopify/rails 5813559f, checked on the task PostgreSQL).
const EXPECTED: [(&str, &str, u64); 3] = [
    ("RelationLoggerTest#test_a_macro's_class_attribute_is_declared_on_the_model", "pass", 2),
    ("RelationLoggerTest#test_a_record_and_its_class_answer_the_logger", "pass", 3),
    ("RelationLoggerTest#test_take_with_a_limit_answers_an_Array_of_up_to_n_rows", "pass", 4),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported") && !e.contains("no known method")).collect()
}

/// Removes the native app's scratch tree however the test ends.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The model macro is replayed, not refused, and `take(count)` is the
/// counted terminal while bare `take` stays one record.
#[test]
fn the_macro_is_replayed_and_take_with_a_limit_is_the_counted_terminal() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let note = std::fs::read_to_string(emitted.join("app/models/note.rb")).expect("note.rb");
    assert!(note.contains("has_tags :note_tags") || note.contains("has_tags(:note_tags"), "{note}");
    assert!(note.contains(".take_n(count)") && note.contains(".take\n"), "{note}");
}

/// The tests run on PostgreSQL natively and emitted, and both answer
/// [`EXPECTED`].
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn relation_and_logger_surfaces_run_as_natively_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("relation-logger-native-{}", std::process::id()));
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
    let database = format!("rh_e2e_relation_logger_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, _) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
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
