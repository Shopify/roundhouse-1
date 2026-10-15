//! A Concern's class macro whose body is not filter DSL the lowering can
//! fold (`stamp_*` below: a statement that is no filter, a
//! filter with an `if:` Array, a String `only:`, an option the
//! static binder does not know, an `around_action` continuation called
//! with an argument) is refused by `expand_class_body_macros`, by name,
//! with one gap, and its call stays in the class body. Since
//! `replay_subclass_macros` that call is a class-body statement
//! (`ClassIvarInit`) the emitted class runs when it loads, and the macro
//! registers its filters in the run-time registry
//! (`runtime/spinel/scaffold/ruby_overlay/runtime/action_controller_filters.rb`).
//! Rails does exactly that, so the refused shape runs faithfully, once.
//!
//! The oracle is native Rails (Shopify/rails 5813559f): the same app and
//! controller test run natively on the same PostgreSQL and the emitted
//! tree must answer what it answers, assertion for assertion. The trail
//! each filter writes also shows a filter is applied once, not both
//! lowered into the preamble and registered at class load.
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
  database: rh_e2e_refused_filter_macro_replay
"#;

const ROUTES: &str = r#"Rails.application.routes.draw do
  resources :notes, only: [:index, :show]
end
"#;

const CONCERN: &str = r#"module Stamps
  extend ActiveSupport::Concern

  module ClassMethods
    # A statement that is no filter DSL beside the filter.
    def stamp_tracked(name)
      (@tracked ||= []) << name
      before_action { trace("tracked:#{name}") }
    end

    # Options forwarded verbatim: a String `only:`, an `if:` Array, and
    # `prepend:`, which Rails reads itself.
    def stamp_kw(name, **kwargs)
      before_action(**kwargs) do |controller|
        trace("kw:#{name}")
      end
    end

    # A macro calling another macro: the parent controller shadows
    # `stamp_base`, so Ruby (not the concern's own text) decides which.
    def stamp_nested(name)
      stamp_base(name)
      before_action(only: :show) { trace("nested:#{name}") }
    end

    def stamp_base(name)
      before_action { trace("concern-base:#{name}") }
    end

    # The continuation called with an argument.
    def stamp_around(name)
      around_action do |controller, block|
        trace("around:in:#{name}")
        block.call(1)
        trace("around:out:#{name}")
      end
    end
  end

  private

  def yes?
    true
  end

  def also?
    params[:also] != "0"
  end

  def trace(step)
    @trail = (@trail || []) + [step]
    response.headers["X-Trail"] = @trail.join(",")
  end
end
"#;

const APPLICATION_CONTROLLER: &str = r#"class ApplicationController < ActionController::Base
  include Stamps

  def self.stamp_base(name)
    before_action { trace("app-base:#{name}") }
  end
end
"#;

const CONTROLLER: &str = r#"class NotesController < ApplicationController
  before_action :first_static
  stamp_tracked "a"
  stamp_kw "b", only: "index"
  stamp_kw "c", if: [:yes?, :also?], only: [:index, :show]
  stamp_kw "d", prepend: true
  stamp_nested "g"
  stamp_around "f"

  def index
    render json: { trail: @trail }
  end

  def show
    render json: { trail: @trail }
  end

  private

  def first_static
    trace("first")
  end
end
"#;

const CONTROLLER_TEST: &str = r#"require "test_helper"

class NotesControllerTest < ActionController::TestCase
  test "every refused shape registers its filter once, in declaration order, prepend first" do
    get :index
    assert_response :success
    assert_equal %w[kw:d first tracked:a kw:b kw:c app-base:g around:in:f around:out:f], @response.headers["X-Trail"].split(",")
    assert_equal %w[kw:d first tracked:a kw:b kw:c app-base:g around:in:f], JSON.parse(@response.body)["trail"]
  end

  test "a parent controller's class method shadows the macro a refused macro calls" do
    get :show, params: { id: 1 }
    assert_response :success
    trail = @response.headers["X-Trail"].split(",")
    assert_includes trail, "app-base:g"
    assert_not_includes trail, "concern-base:g"
    assert_equal 1, trail.count("nested:g")
  end

  test "a String only: scopes the filter and an if: Array guards it" do
    get :show, params: { id: 1, also: "0" }
    assert_response :success
    assert_equal %w[kw:d first tracked:a app-base:g nested:g around:in:f around:out:f], @response.headers["X-Trail"].split(",")
  end
end
"#;

const TEST_FILES: [&str; 1] = ["test/controllers/notes_controller_test.rb"];

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\n".into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/routes.rb", ROUTES.into()),
        ("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = true\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".into()),
        ("app/models/note.rb", "class Note < ApplicationRecord\nend\n".into()),
        ("app/controllers/concerns/stamps.rb", CONCERN.into()),
        ("app/controllers/application_controller.rb", APPLICATION_CONTROLLER.into()),
        ("app/controllers/notes_controller.rb", CONTROLLER.into()),
        (TEST_FILES[0], CONTROLLER_TEST.into()),
    ]
}

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

const EXPECTED: [(&str, &str, u64); 3] = [
    ("NotesControllerTest#test_a_String_only:_scopes_the_filter_and_an_if:_Array_guards_it", "pass", 2),
    ("NotesControllerTest#test_a_parent_controller's_class_method_shadows_the_macro_a_refused_macro_calls", "pass", 6),
    ("NotesControllerTest#test_every_refused_shape_registers_its_filter_once,_in_declaration_order,_prepend_first", "pass", 3),
];

const NATIVE_FILES: [(&str, &str); 3] = [
    (
        "config/application.rb",
        r#"require ENV.fetch("RH_RAILS_ORACLE_RB")
RailsOracle.activate("activesupport", "activemodel", "activerecord", "activejob", "actionpack", "actionview", "railties")
require "rails"
require "active_record/railtie"
require "active_job/railtie"
require "action_controller/railtie"
require "action_view/railtie"
# Core bundles rails-controller-testing (assigns, assert_template).
require "rails-controller-testing"

module ProcessApp
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


/// Each test file runs on PostgreSQL, natively and emitted, and both
/// answer [`EXPECTED`].
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn refused_filter_macros_replay_like_rails_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("refused-filter-macro-replay-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_refused_filter_macro_replay_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}
