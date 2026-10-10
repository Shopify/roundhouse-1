//! `serialize :col, coder: X` with a fixture that writes the column's
//! stored text through ERB, on the PostgreSQL CRuby tree.
//!
//! Core's merchant_subscriptions prices.yml.erb writes a serialized
//! varchar column as `capabilities: <%= {...}.to_json(escape: false).inspect %>`.
//! Rails renders the file through ERB and Psych reads the text, so the
//! row holds the JSON text (the quoted scalar's contents), and the fixture
//! insert casts it by the column's database type, not through the coder.
//! The emitted loader used the tag's value as is: the stored text kept
//! its quotes, and CapabilityPayloadCoder.load's JSON.parse answered a
//! String ("capability payload must be a JSON object"). Each whole-tag
//! value now goes through `TestFixtures.erb_scalar`: on the jsonb column a
//! JSON-text tag is the Hash YAML reads, cast to JSON. Create round-trips
//! the varchar column through the coder's `dump`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STRUCTURE: &str = r#"CREATE TABLE notes (
    id bigserial NOT NULL,
    title character varying,
    count integer,
    payload character varying,
    data jsonb,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_serialized_coder
"#;

/// A coder shaped like Core's CapabilityPayloadCoder, which also says
/// what class of value `load` received.
const CODER: &str = r#"class PayloadCoder
  def self.load(raw)
    return if raw.nil?
    parsed = raw.is_a?(String) ? JSON.parse(raw) : raw
    raise ArgumentError, "payload must be a JSON object" unless parsed.is_a?(Hash)
    parsed.merge("_raw" => raw.class.name)
  end

  def self.dump(value)
    return if value.nil?
    value.except("_raw").to_json
  end
end
"#;

const NOTE: &str = "class Note < ApplicationRecord\n  serialize :payload, coder: PayloadCoder\nend\n";

const NOTES: &str = r#"one:
  title: <%= "plain".inspect %>
  count: <%= 2 + 3 %>
  payload: <%= {
    "landed_cost_calculation": false,
    "staff_accounts": { "enforced": true, "limit": 5 },
  }.to_json(escape: false).inspect %>
  data: <%= { "a" => 1 }.to_json %>

two:
  title: Note <%= 1 + 1 %>
  count: <%= "" %>
  payload: '{"b":2}'
  data:
    c: 3
"#;

const TEST: &str = r##"require "test_helper"

class NoteTest < ActiveSupport::TestCase
  fixtures :all

  test "fixture tags are the text yaml reads" do
    one = notes(:one)
    assert_equal "plain", one.title
    assert_equal 5, one.count
    assert_equal({ "landed_cost_calculation" => false, "staff_accounts" => { "enforced" => true, "limit" => 5 }, "_raw" => "String" }, one.payload)
    assert_equal({ "a" => 1 }, one.data)
    raw = Note.connection.select_value("SELECT payload FROM notes WHERE id = #{one.id}")
    assert_equal '{"landed_cost_calculation":false,"staff_accounts":{"enforced":true,"limit":5}}', raw
  end

  test "fixture literals and embedded tags" do
    two = notes(:two)
    assert_equal "Note 2", two.title
    assert_nil two.count
    assert_equal({ "b" => 2, "_raw" => "String" }, two.payload)
    assert_equal({ "c" => 3 }, two.data)
  end

  test "create dumps through the coder" do
    note = Note.create!(payload: { "x" => 1 }, data: { "y" => 2 }).reload
    assert_equal({ "x" => 1, "_raw" => "String" }, note.payload)
    assert_equal({ "y" => 2 }, note.data)
    assert_equal '{"x":1}', Note.connection.select_value("SELECT payload FROM notes WHERE id = #{note.id}")
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
        ("app/models/payload_coder.rb", CODER.into()),
        ("app/models/note.rb", NOTE.into()),
        ("test/fixtures/notes.yml", NOTES.into()),
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

module SerializedCoderApp
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
    ("NoteTest#test_create_dumps_through_the_coder", "pass", 3),
    ("NoteTest#test_fixture_literals_and_embedded_tags", "pass", 4),
    ("NoteTest#test_fixture_tags_are_the_text_yaml_reads", "pass", 5),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// Each whole-tag fixture value is read back as YAML by the runtime; a
/// tag inside other text is the interpolated string.
#[test]
fn whole_erb_fixture_values_go_through_erb_scalar() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let loader = std::fs::read_to_string(emitted.join("test/fixtures/notes.rb")).expect("notes.rb");
    assert!(loader.contains("ActiveRecord::TestFixtures.erb_scalar({ landed_cost_calculation: false"), "{loader}");
    assert!(loader.contains("ActiveRecord::TestFixtures.erb_scalar(2 + 3)"), "{loader}");
    assert!(loader.contains("\"Note #{1 + 1}\""), "{loader}");
    assert!(!loader.contains("erb_scalar(\"Note"), "{loader}");
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

/// The test runs on PostgreSQL, natively (Shopify/rails 5813559f, with
/// rails/test_help) and emitted, and both answer [`EXPECTED`].
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn serialized_coder_fixtures_answer_as_natively_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("serialized-coder-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_serialized_coder_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}
