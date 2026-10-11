//! The test-case load hooks on the PostgreSQL CRuby tree:
//! `ActiveSupport.on_load(:active_support_test_case)` and the other test
//! bases' hooks, which Core's initializers and engines register at boot
//! (components/platform/essentials/config/initializers/shopify_session.rb,
//! components/platform/search_platform/config/initializers/active_job_test_case.rb,
//! a dozen engines' `on_load(:action_controller_test_case)`), and whose
//! bodies `require "support/helpers/…"` from the test load path and
//! include it.
//!
//! Before, nothing fired them: the ingest refused an initializer's hook
//! ("nothing fires `:active_support_test_case`"), and an engine's,
//! registered at boot, never ran, so no emitted test had the helpers the
//! hooks include. Now the emitted test helper fires them where Rails
//! does (railties 8.2.0.alpha rails/test_help.rb): `:active_support_test_case`
//! as active_support/test_case loads, before rails/test_help's own
//! `include ActiveRecord::TestFixtures`; another base's when the first
//! test class descends from it, as Rails autoloads the base where a test
//! names it (action_view/test_case.rb loading action_controller/test_case
//! first), and a Behavior base's on each class including its Behavior
//! (Core's developer_dashboard check_box_test.rb). A lib file's hook
//! (essentials/lib/patches/action_dispatch_url_for_param_filter.rb:81)
//! runs in registration order with them, not as a reopen at boot. A
//! hook's `require` of a test-support feature loads the carried file
//! on the test `$LOAD_PATH` Rails builds (`TEST_LOAD_PATH_FEATURES`:
//! `test` and each engine's `paths.add("test", load_path: true)`), and
//! Core's `#: self as singleton(ActiveJob::TestCase)` is no error.
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
  database: rh_e2e_test_case_hooks
"#;

/// Boot registers each hook; a base's loading fires them. The
/// `:active_job_test_case` block opens with Core's ascription
/// (components/platform/search_platform/config/initializers/active_job_test_case.rb:5).
const INITIALIZER: &str = r#"ActiveSupport.on_load(:active_support_test_case) do
  require "support/helpers/hook_helper"
  include HookHelper
  HookLog.add("active_support_test_case fixtures=#{include?(ActiveRecord::TestFixtures)}")
end

ActiveSupport.on_load(:action_controller_test_case) do
  require "support/helpers/controller_hook_helper"
  include ControllerHookHelper
  HookLog.add("action_controller_test_case")
end

ActiveSupport.on_load(:action_view_test_case) do
  HookLog.add("action_view_test_case #{name}")
end

ActiveSupport.on_load(:active_job_test_case) do
  #: self as singleton(ActiveJob::TestCase)

  HookLog.add("active_job_test_case")
end
"#;

/// A path-gem engine that puts its `test` on the test `$LOAD_PATH`
/// (Core's `components/*/lib/*/engine.rb`), and loads a lib patch that
/// registers a controller-case hook first (Core's
/// essentials/lib/patches/action_dispatch_url_for_param_filter.rb:81).
/// Core's essentials/config/initializers/shopify_session.rb:42: the
/// block defines a module, a top-level one as Ruby defines it, and
/// extends a controller base with it.
const SESSION_INITIALIZER: &str = r#"ActiveSupport.on_load(:active_support_test_case) do
  module ControllerLikelySessionStore
    def likely_session_store
      :buyer
    end
  end
  ActionController::Base.extend(ControllerLikelySessionStore)
end
"#;

const ENGINE: &str = r#"require "widgets/test_patch"

module Widgets
  class Engine < ::Rails::Engine
    if Rails.env.test?
      paths.add("test", load_path: true)
    end
  end
end
"#;

const TEST_PATCH: &str = r#"module WidgetPatch
  def self.included(base)
    HookLog.add("widget_patch included in #{base.name}")
  end

  def widget_patch_answer
    3
  end
end

ActiveSupport.on_load(:action_controller_test_case) { include WidgetPatch }
"#;

/// The engine's initializer: its hook requires features only the
/// engine's `test` answers, one of them a file with no constant, which
/// nothing but that `require` loads.
const ENGINE_INITIALIZER: &str = r#"ActiveSupport.on_load(:action_controller_test_case) do
  require "support/helpers/widget_setup"
  require "support/helpers/widget_hook_helper"
  include WidgetHookHelper
  HookLog.add("widgets action_controller_test_case")
end
"#;

const HOOK_LOG: &str = r#"class HookLog
  def self.lines
    @lines ||= []
  end

  def self.add(line)
    lines << line
  end
end
"#;

/// The `:active_support_test_case` hook ran before rails/test_help's own
/// `include ActiveRecord::TestFixtures`, and no other base loaded (the lib
/// patch's hook among them).
const TEST: &str = r#"require "test_helper"

class HookOrderTest < ActiveSupport::TestCase
  test "hook helper reaches the test" do
    assert_equal 42, hook_helper_answer
  end

  test "only the active support hooks ran" do
    assert_kind_of HookHelper, self
    assert_not_kind_of WidgetPatch, self
    assert_equal ["active_support_test_case fixtures=false"], HookLog.lines
  end

  test "a module the hook block defines is a top level module" do
    assert_equal :buyer, ActionController::Base.likely_session_store
    assert_equal "ControllerLikelySessionStore", ControllerLikelySessionStore.name
    assert Object.const_defined?(:ControllerLikelySessionStore, false)
  end
end
"#;

const JOB_TEST: &str = r#"require "test_helper"

class JobHookTest < ActiveJob::TestCase
  test "the job case hooks ran as it loaded" do
    assert_kind_of HookHelper, self
    assert_equal ["active_support_test_case fixtures=false", "active_job_test_case"], HookLog.lines
  end
end
"#;

/// The controller case's hooks in registration order: the lib patch's
/// (loaded with the engine), the engine initializer's, the app's.
const CONTROLLER_LINES: &str = r#"[
      "active_support_test_case fixtures=false",
      "widget_patch included in ActionController::TestCase",
      "widget_setup required",
      "widgets action_controller_test_case",
      "action_controller_test_case",
    ]"#;

const CONTROLLER_TEST: &str = r#"require "test_helper"

class ControllerHookTest < ActionController::TestCase
  test "controller hook helpers reach the test in registration order" do
    assert_equal 7, controller_hook_answer
    assert_equal 5, widget_hook_answer
    assert_equal 3, widget_patch_answer
    assert_equal CONTROLLER_LINES, HookLog.lines
  end

  test "session reads the request session" do
    session[:user_id] = 7
    assert_equal 7, session["user_id"]
    assert_same request.session, session
  end
end
"#;

const VIEW_TEST: &str = r#"require "test_helper"

class ViewHookTest < ActionView::TestCase
  test "the view case loads the controller case first" do
    assert_kind_of HookHelper, self
    assert_equal CONTROLLER_LINES + ["action_view_test_case ActionView::TestCase"], HookLog.lines
  end
end
"#;

/// Core's developer_dashboard check_box_test.rb shape: a test that
/// includes the view case's Behavior gets its hooks run on itself.
const BEHAVIOR_TEST: &str = r#"require "test_helper"

class BehaviorHookTest < ActiveSupport::TestCase
  include ActionView::TestCase::Behavior

  test "including the view case behavior runs its hooks on the includer" do
    assert_equal CONTROLLER_LINES + ["action_view_test_case ActionView::TestCase", "action_view_test_case BehaviorHookTest"], HookLog.lines
  end
end
"#;

/// ActionController::TestCase::Behavior's first setup callback: the
/// controller its name names, and the request a `setup do` block reads
/// (Core's login helpers reach `request.buyer_session`).
const CONTROLLER_REQUEST_TEST: &str = r#"require "test_helper"

class NotesControllerTest < ActionController::TestCase
  setup do
    @seen = [request.class.name, request.controller_class.name, @controller.class.name, @controller.request.equal?(request), @controller.params.empty?]
  end

  test "the controller and its request exist before setup blocks run" do
    assert_equal ["ActionController::TestRequest", "NotesController", "NotesController", true, true], @seen
  end
end
"#;

const TEST_FILES: [&str; 6] = [
    "test/models/hook_order_test.rb",
    "test/models/job_hook_test.rb",
    "test/controllers/controller_hook_test.rb",
    "test/helpers/view_hook_test.rb",
    "test/helpers/behavior_hook_test.rb",
    "test/controllers/notes_controller_test.rb",
];

const PATH_GEMS: &str = "PATH\n  remote: components/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  widgets!\n";

fn files() -> Vec<(&'static str, String)> {
    let lines = |test: &str| test.replace("CONTROLLER_LINES", CONTROLLER_LINES);
    vec![
        (
            "roundhouse.yml",
            "database_adapter: postgresql\ntest_paths:\n  - test/support\n  - components/widgets/test/support\n".into(),
        ),
        ("Gemfile.lock", PATH_GEMS.into()),
        ("config/database.yml", DATABASE_YML.into()),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n".into()),
        // Eager loading, as Core's test environment does when EAGER_LOAD
        // is set (BootLib.test_eager_load?): it loads no test-support file.
        ("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = true\nend\n".into()),
        ("db/structure.sql", STRUCTURE.into()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".into()),
        ("app/models/note.rb", "class Note < ApplicationRecord\nend\n".into()),
        ("app/controllers/notes_controller.rb", "class NotesController < ActionController::Base\nend\n".into()),
        ("app/models/hook_log.rb", HOOK_LOG.into()),
        ("config/initializers/test_hooks.rb", INITIALIZER.into()),
        ("config/initializers/session_store.rb", SESSION_INITIALIZER.into()),
        ("components/widgets/lib/widgets/engine.rb", ENGINE.into()),
        ("components/widgets/lib/widgets/test_patch.rb", TEST_PATCH.into()),
        ("components/widgets/config/initializers/widget_hooks.rb", ENGINE_INITIALIZER.into()),
        ("components/widgets/test/support/helpers/widget_setup.rb", "HookLog.add(\"widget_setup required\")\n".into()),
        (
            "components/widgets/test/support/helpers/widget_hook_helper.rb",
            "module WidgetHookHelper\n  def widget_hook_answer\n    5\n  end\nend\n".into(),
        ),
        ("test/support/helpers/hook_helper.rb", "module HookHelper\n  def hook_helper_answer\n    42\n  end\nend\n".into()),
        (
            "test/support/helpers/controller_hook_helper.rb",
            "module ControllerHookHelper\n  def controller_hook_answer\n    7\n  end\nend\n".into(),
        ),
        (TEST_FILES[0], TEST.into()),
        (TEST_FILES[1], JOB_TEST.into()),
        (TEST_FILES[2], lines(CONTROLLER_TEST)),
        (TEST_FILES[3], lines(VIEW_TEST)),
        (TEST_FILES[4], lines(BEHAVIOR_TEST)),
        (TEST_FILES[5], CONTROLLER_REQUEST_TEST.into()),
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
require "active_job/railtie"
require "action_controller/railtie"
require "action_view/railtie"
# The path gem's lib, as Bundler puts it on the load path; the engine
# loads as Bundler.require would load it.
$LOAD_PATH.unshift(File.expand_path("../components/widgets/lib", __dir__))
require "widgets/engine"

module HooksApp
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
const EXPECTED: [(&str, &str, u64); 9] = [
    ("BehaviorHookTest#test_including_the_view_case_behavior_runs_its_hooks_on_the_includer", "pass", 1),
    ("ControllerHookTest#test_controller_hook_helpers_reach_the_test_in_registration_order", "pass", 4),
    ("ControllerHookTest#test_session_reads_the_request_session", "pass", 2),
    ("HookOrderTest#test_a_module_the_hook_block_defines_is_a_top_level_module", "pass", 3),
    ("HookOrderTest#test_hook_helper_reaches_the_test", "pass", 1),
    ("HookOrderTest#test_only_the_active_support_hooks_ran", "pass", 3),
    ("JobHookTest#test_the_job_case_hooks_ran_as_it_loaded", "pass", 2),
    ("NotesControllerTest#test_the_controller_and_its_request_exist_before_setup_blocks_run", "pass", 1),
    ("ViewHookTest#test_the_view_case_loads_the_controller_case_first", "pass", 2),
];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The initializers' hooks are carried; the helper fires the
/// `:active_support_test_case` ones around rails/test_help's own include
/// and defers each other base's to its first subclass (and a Behavior
/// base's to each includer as well); a hook's test-support feature
/// resolves to the carried file on the test `$LOAD_PATH` only; the lib
/// patch's controller-case hook is registered as written, not carried
/// as a reopen of ActionController::TestCase; Core's `#: self as
/// singleton(ActiveJob::TestCase)` checks clean.
#[test]
fn test_case_hooks_are_carried_and_fired_where_rails_test_help_fires_them() {
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("nothing fires")), "{}", errors.join("\n"));
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let helper = read("test/test_helper.rb");
    let at = |needle: &str| helper.find(needle).unwrap_or_else(|| panic!("no {needle:?} in\n{helper}"));
    let order = [
        at("naming_failures(\"activesupport/lib/active_support/test_case.rb:408\") { ActiveSupport.run_load_hooks(:active_support_test_case, TestBase) }"),
        at("TestBase.include(ActiveRecord::TestFixtures)"),
        at("run_when_loaded(:action_controller_test_case, ActionController::TestCase, \"actionpack/lib/action_controller/test_case.rb:611\", behavior: true) if defined?(ActionController::TestCase)"),
        at("run_when_loaded(:action_dispatch_integration_test, ActionDispatch::IntegrationTest, \"actionpack/lib/action_dispatch/testing/integration.rb:693\", behavior: true) if defined?(ActionDispatch::IntegrationTest)"),
        at("run_when_loaded(:action_view_test_case, ActionView::TestCase, \"actionview/lib/action_view/test_case.rb:256\", after: [:action_controller_test_case], behavior: true) if defined?(ActionView::TestCase)"),
        at("run_when_loaded(:active_job_test_case, ActiveJob::TestCase, \"activejob/lib/active_job/test_case.rb:9\") if defined?(ActiveJob::TestCase)"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}\n{helper}");
    assert!(at("require_relative \"minitest_runner\"") < order[0], "{helper}");
    // Deferred, not also fired where the runtime defines the class.
    assert_eq!(helper.matches("run_load_hooks(:active_job_test_case").count(), 0, "{helper}");
    let models = read("app/models.rb");
    let table = &models[models.find("TEST_LOAD_PATH_FEATURES = {").unwrap_or_else(|| panic!("no table in\n{models}"))..];
    let table = &table[..table.find("}.freeze").expect("table end")];
    // Rails' `test` and the engine's (its `paths.add("test", load_path:
    // true)`); the engine's lib is not the test load path's.
    for feature in [
        "support/helpers/controller_hook_helper",
        "support/helpers/hook_helper",
        "support/helpers/widget_hook_helper",
        "support/helpers/widget_setup",
    ] {
        assert!(table.contains(&format!("\"{feature}\" =>")), "{feature}: {table}");
    }
    assert_eq!(table.matches(" => ").count(), 4, "{table}");
    let initializer = read("app/models/roundhouse_initializer_config_initializers_test_hooks.rb");
    assert!(initializer.contains("require \"support/helpers/hook_helper\""), "{initializer}");
    // The ascription is a comment for Ruby: nothing raises in its place.
    assert!(!initializer.contains("declared type not supported"), "{initializer}");
    assert!(!initializer.contains("initializer statement did not run"), "{initializer}");
    assert!(!initializer.contains("ActiveJob::TestCase)"), "{initializer}");
    // The lib patch registers its hook where it loads, for the test
    // helper to fire; no boot-time reopen of the controller case.
    let patch = std::fs::read_dir(emitted.join("app/models"))
        .expect("app/models")
        .flatten()
        .map(|e| e.path())
        .chain(walk(&emitted.join("app/models/roundhouse_files")))
        .filter(|p| p.extension().is_some_and(|x| x == "rb"))
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|text| (p, text)))
        .filter(|(_, text)| text.contains("include WidgetPatch") || text.contains("include(WidgetPatch)"))
        .collect::<Vec<_>>();
    assert_eq!(patch.len(), 1, "{patch:#?}");
    assert!(patch[0].1.contains("ActiveSupport.on_load(:action_controller_test_case)"), "{patch:#?}");
    // The module the session hook's block defines is carried as the
    // top-level module it is, read where the block defines it, before
    // the block's `extend` of it.
    let session = read("app/models/roundhouse_initializer_config_initializers_session_store.rb");
    let module_read = session.find("ControllerLikelySessionStore\n").unwrap_or_else(|| panic!("{session}"));
    let extend = session.find("ActionController::Base.extend(ControllerLikelySessionStore)").unwrap_or_else(|| panic!("{session}"));
    assert!(module_read < extend, "{session}");
    assert!(!session.contains("defined inside a method or block"), "{session}");
    let module = read("app/models/controller_likely_session_store.rb");
    assert!(module.contains("module ControllerLikelySessionStore"), "{module}");
    assert!(module.contains("def likely_session_store"), "{module}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// Precisely the test `$LOAD_PATH`: an engine without `paths.add("test",
/// load_path: true)` (Core's access_and_auth/login_with_shop) adds no
/// root, so a feature only its `test` holds stays Kernel's `require`;
/// the application's `config.paths.add("test/support", load_path: true)`
/// under `if Rails.env.test?` (Core's config/application.rb:208-211)
/// adds one, and a file below both `test` and `test/support` answers
/// both features. A `case Rails.env` `when "test"` branch counts (Core's
/// markets/platform/lib/markets/platform/engine.rb:9); its `when
/// "development"` branch does not.
#[test]
fn the_test_load_path_is_rails_test_and_each_load_path_entry() {
    let application = r#"require "rails"

module HooksApp
  class Application < Rails::Application
    if Rails.env.test?
      config.paths.add("test", load_path: true)
      config.paths.add("test/support", load_path: true)
    end
    config.paths.add("db/migrate_shard")
  end
end
"#;
    let plain_engine = "module Plain\n  class Engine < ::Rails::Engine\n  end\nend\n";
    let cased_engine = r#"module Cased
  class Engine < ::Rails::Engine
    case Rails.env
    when "development"
      paths.add("test/dev_support", load_path: true)
    when "test"
      paths.add("test/support", load_path: true)
    end
  end
end
"#;
    let lock = "PATH\n  remote: components/cased\n  specs:\n    cased (0.1.0)\n\nPATH\n  remote: components/plain\n  specs:\n    plain (0.1.0)\n\nPATH\n  remote: components/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  cased!\n  plain!\n  widgets!\n";
    let (emitted, errors) = overlay()
        .write("config/application.rb", application)
        .write("Gemfile.lock", lock)
        .write(
            "roundhouse.yml",
            "database_adapter: postgresql\ntest_paths:\n  - test/support\n  - components/widgets/test/support\n  - components/plain/test/support\n  - components/cased/test/support\n  - components/cased/test/dev_support\n",
        )
        .write("components/cased/lib/cased/engine.rb", cased_engine)
        .write("components/cased/test/support/helpers/cased_helper.rb", "module CasedHelper\nend\n")
        .write("components/cased/test/dev_support/dev_helper.rb", "module DevHelper\nend\n")
        .write("components/plain/lib/plain/engine.rb", plain_engine)
        .write("components/plain/test/support/helpers/plain_helper.rb", "module PlainHelper\nend\n")
        .emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let models = std::fs::read_to_string(emitted.join("app/models.rb")).expect("models");
    let table = &models[models.find("TEST_LOAD_PATH_FEATURES = {").expect("table")..];
    let table = &table[..table.find("}.freeze").expect("table end")];
    // Both are carried, and neither is on the test load path.
    let carried = walk(&emitted.join("app/models"))
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect::<Vec<_>>()
        .join("\n");
    for module in ["module PlainHelper", "module DevHelper", "module CasedHelper"] {
        assert!(carried.contains(module), "{module} not carried");
    }
    assert!(!table.contains("plain_helper"), "{table}");
    assert!(!table.contains("dev_helper"), "{table}");
    for feature in ["support/helpers/hook_helper", "helpers/hook_helper", "support/helpers/widget_setup", "helpers/cased_helper"] {
        assert!(table.contains(&format!("\"{feature}\" =>")), "{feature}: {table}");
    }
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

/// Each test file runs on PostgreSQL, natively (railties 8.2.0.alpha,
/// Shopify/rails 5813559f, with rails/test_help) and emitted, and both
/// answer [`EXPECTED`]: the hooks' helpers reach the tests, the
/// `:active_support_test_case` hook ran before rails/test_help's
/// `include ActiveRecord::TestFixtures`, and a base's hooks ran only in
/// the process whose test descends from it.
#[test]
#[ignore = "requires pg gem, Core's gem home and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn emitted_tests_get_the_hooks_helpers_on_postgresql() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let (emitted, errors) = overlay().emit(BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    std::fs::write(emitted.join("db/structure.sql"), STRUCTURE).expect("structure");
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("test-case-load-hooks-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let _scratch = Scratch(native_root.clone());
    let native_files: Vec<(&str, String)> =
        files().into_iter().chain(NATIVE_FILES.iter().map(|(path, text)| (*path, text.to_string()))).collect();
    for (path, text) in &native_files {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let database = format!("rh_e2e_test_case_hooks_{}", std::process::id());
    let expected: Vec<(String, String, u64)> =
        EXPECTED.iter().map(|(id, status, n)| (id.to_string(), status.to_string(), *n)).collect();
    let (native, _) = run_side("native", &native_root, &format!("{database}_n"));
    assert_eq!(native, expected, "native");
    let (emitted_rows, stderr) = run_side("emitted", &emitted, &format!("{database}_e"));
    assert_eq!(emitted_rows, native, "emitted tree={}", emitted.display());
    // Every hook statement ran: a `require` the test load path does not
    // answer, or a hook that raises, would be named here.
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("roundhouse: error[")).collect();
    assert!(named.is_empty(), "emitted tree={}\n{}", emitted.display(), named.join("\n"));
}
