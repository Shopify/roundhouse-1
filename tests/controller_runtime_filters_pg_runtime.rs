//! A Concern's class macro registers filters when the controller class
//! loads (`Cors::ClassMethods#allow_insecure_cross_origin_access` ends in
//! `prepend_before_action(opts) { … }`), so lowering cannot fold them
//! into the synthesized `process_action` as it does a `before_action
//! :name` written in the body. The ruby tree carries a run-time registry
//! (`runtime/spinel/scaffold/ruby_overlay/runtime/action_controller_filters.rb`)
//! and the dispatcher of a controller with such a macro consults it:
//! run-time prepends lead the chain, run-time before filters follow the
//! lowered ones, a halting lowered filter stops them, after and around
//! filters wrap the action.
//!
//! The oracle is native Rails (Shopify/rails 5813559f): the same app and
//! controller test run natively on the same PostgreSQL and the emitted
//! tree must answer what it answers, assertion for assertion.
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
  database: rh_e2e_controller_runtime_filters
"#;

const ROUTES: &str = r#"Rails.application.routes.draw do
  resources :notes, only: [:index, :show] do
    collection do
      get :secret
    end
  end
end
"#;

/// The shape of Core's `Cors` concern: a ClassMethods macro that
/// registers a block filter, prepended by default.
const CONCERN: &str = r#"module CrossOrigin
  extend ActiveSupport::Concern

  module ClassMethods
    def allow_cross_origin(origin, options = {})
      if options.fetch(:prepend, true)
        prepend_before_action(options.slice(:only, :except)) do
          handle_cross_origin(origin)
        end
      else
        before_action(options.slice(:only, :except)) do
          handle_cross_origin(origin)
        end
      end
    end

    def audit_requests
      before_action :audit_before, unless: :quiet?
      after_action { trace("after") }
      around_action :measure
    end
  end

  private

  def handle_cross_origin(origin)
    trace("cors:#{origin}")
    response.headers["Access-Control-Allow-Origin"] = origin
  end

  def audit_before
    trace("audit")
  end

  def quiet?
    params[:quiet] == "1"
  end

  def measure
    trace("around:in")
    yield
    trace("around:out")
  end

  def trace(step)
    @trail = (@trail || []) + [step]
    response.headers["X-Trail"] = @trail.join(",")
  end
end
"#;

/// Lowered filters (`first_static`, `deny`, `last_static`) around the
/// macro-registered ones; `secret` is denied by a lowered filter.
const CONTROLLER: &str = r#"class NotesController < ActionController::Base
  include CrossOrigin

  before_action :first_static
  before_action :deny, only: :secret
  before_action :last_static, only: :show
  allow_cross_origin "https://a.example", only: [:index, :secret]
  allow_cross_origin "https://b.example", prepend: false, only: :show
  audit_requests

  def index
    render json: { trail: @trail }
  end

  def show
    render json: { trail: @trail }
  end

  def secret
    render json: { trail: @trail }
  end

  private

  def first_static
    trace("first")
  end

  def last_static
    trace("last")
  end

  def deny
    trace("deny")
    head :forbidden
  end
end
"#;

const CONTROLLER_TEST: &str = r#"require "test_helper"

class NotesControllerTest < ActionController::TestCase
  test "a prepended macro filter leads the chain, the lowered one follows, the others wrap the action" do
    get :index
    assert_response :success
    assert_equal "https://a.example", @response.headers["Access-Control-Allow-Origin"]
    assert_equal %w[cors:https://a.example first audit around:in around:out after], @response.headers["X-Trail"].split(",")
    assert_equal %w[cors:https://a.example first audit around:in], JSON.parse(@response.body)["trail"]
  end

  test "an appended macro filter follows the lowered filters, in declaration order" do
    get :show, params: { id: 1 }
    assert_response :success
    assert_nil @response.headers["Access-Control-Allow-Origin"].to_s[/a\.example/]
    assert_equal "https://b.example", @response.headers["Access-Control-Allow-Origin"]
    assert_equal %w[first last cors:https://b.example audit around:in around:out after], @response.headers["X-Trail"].split(",")
  end

  test "a lowered filter that halts stops the macro filters declared after it, not the one prepended" do
    get :secret
    assert_response :forbidden
    assert_equal "https://a.example", @response.headers["Access-Control-Allow-Origin"]
    assert_equal %w[cors:https://a.example first deny], @response.headers["X-Trail"].split(",")
  end

  test "an if/unless guard on a macro filter reads the request" do
    get :index, params: { quiet: "1" }
    assert_response :success
    assert_equal %w[cors:https://a.example first around:in around:out after], @response.headers["X-Trail"].split(",")
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
        ("app/controllers/concerns/cross_origin.rb", CONCERN.into()),
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

const EXPECTED: [(&str, &str, u64); 4] = [
    ("NotesControllerTest#test_a_lowered_filter_that_halts_stops_the_macro_filters_declared_after_it,_not_the_one_prepended", "pass", 3),
    ("NotesControllerTest#test_a_prepended_macro_filter_leads_the_chain,_the_lowered_one_follows,_the_others_wrap_the_action", "pass", 4),
    ("NotesControllerTest#test_an_appended_macro_filter_follows_the_lowered_filters,_in_declaration_order", "pass", 4),
    ("NotesControllerTest#test_an_if/unless_guard_on_a_macro_filter_reads_the_request", "pass", 2),
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
fn macro_registered_filters_run_in_the_dispatcher_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("controller-runtime-filters-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_controller_runtime_filters_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}

/// The lowering side, without PostgreSQL: the dispatcher of a controller
/// with a class-body macro calls the registry's hooks around its lowered
/// filters, and a controller without one is unchanged.
#[test]
fn the_dispatcher_consults_the_registry_only_for_a_controller_with_a_macro() {
    let app = overlay().write(
        "app/controllers/plain_controller.rb",
        "class PlainController < ActionController::Base\n  before_action :gate\n\n  def index\n    render json: {}\n  end\n\n  private\n\n  def gate\n    head :forbidden\n  end\nend\n",
    )
    .write("config/routes.rb", "Rails.application.routes.draw do\n  resources :notes, only: [:index, :show]\n  resources :plain, only: [:index]\n  get \"notes/secret\", to: \"notes#secret\"\nend\n");
    let (emitted, errors) = app.emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let notes = read("app/controllers/notes_controller.rb");
    let at = |needle: &str| notes.find(needle).unwrap_or_else(|| panic!("`{needle}` missing:\n{notes}"));
    assert!(at("run_prepended_filters(action_name)") < at("first_static"), "{notes}");
    assert!(at("deny") < at("run_appended_filters(action_name)"), "{notes}");
    assert!(at("last_static") < at("run_appended_filters(action_name)"), "{notes}");
    assert!(at("run_appended_filters(action_name)") < at("run_around_filters(action_name)"), "{notes}");
    assert!(at("run_around_filters(action_name)") < at("run_after_filters(action_name)"), "{notes}");
    let plain = read("app/controllers/plain_controller.rb");
    assert!(plain.contains("def process_action"), "{plain}");
    assert!(!plain.contains("run_prepended_filters") && !plain.contains("run_around_filters"), "{plain}");
}
