//! The CRuby runner's rows and lifecycle (`runtime/spinel/test/minitest_runner.rb`)
//! and the test prelude's `Rack::Test::UploadedFile` (`runtime/spinel/test/test_helper.rb`).
//!
//! * Mocha follows mocha/minitest's adapter: an expectation verified is an
//!   assertion, a test that already failed is not verified, and a test
//!   whose fixture reset raised before `mocha_setup` is not torn down
//!   (on unit-1000 every errored row carried a second NoMethodError,
//!   "undefined method 'pop' for nil", from Mocha::Mockery.teardown).
//! * Each exception in a row names its `site` (the first frame in the
//!   emitted tree) and a short `backtrace` that ends at the runner.
//! * An app whose Bundler.require loads rack-test keeps the gem's
//!   `Rack::Test::UploadedFile` (the prelude's subclass of
//!   `ActionDispatch::Http::UploadedFile` was a superclass-mismatch
//!   TypeError at boot on Core), and the in-process dispatch still hands
//!   a controller an `ActionDispatch::Http::UploadedFile`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ruby() -> Command {
    Command::new(std::env::var("RUBY").unwrap_or_else(|_| "ruby".into()))
}

fn minitest_version() -> String {
    std::env::var("ROUNDHOUSE_ASSERTION_ORACLE_VERSION").unwrap_or_else(|_| {
        include_str!("../runtime/spinel/scaffold/Gemfile.lock").lines()
            .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
            .expect("scaffold Minitest lock").to_string()
    })
}

fn read_rows(text: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = text.lines().filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("JSON result row")).collect();
    rows.sort_by_key(|row| row["id"].as_str().unwrap().to_string());
    rows
}

const PROBE_MODEL: &str = "class Probe < ApplicationRecord\n  def self.ping\n    0\n  end\n\n  def self.pong\n    0\n  end\nend\n";

fn app(test_source: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n")
        .write("app/models/probe.rb", PROBE_MODEL)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :probes do |t|\n    t.string :name\n  end\nend\n")
        .write("test/models/runner_contract_test.rb", test_source)
}

/// Run the emitted test file under the scaffold's Minitest; its rows.
fn emitted_rows(emitted: &Path) -> (Vec<Value>, String) {
    let results = emitted.join("results.jsonl");
    let output = ruby()
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), "test/models/runner_contract_test.rb"])
        .current_dir(emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .output().expect("emitted CRuby");
    let log = format!("tree={}\nstdout={}\nstderr={}", emitted.display(), String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let text = std::fs::read_to_string(&results).unwrap_or_else(|error| panic!("no results: {error}\n{log}"));
    (read_rows(&text), log)
}

/// The same class under native Minitest with mocha/minitest.
fn native_mocha_rows(source: &str) -> Vec<Value> {
    let oracle = format!(r#"
gem "minitest", "={version}"
require "minitest/test"
require "mocha/minitest"
Minitest.seed = 4242
require "json"
require "active_support"
require "active_support/test_case"
class Probe
  def self.ping = 0
  def self.pong = 0
end
{source}
RunnerContractTest.runnable_methods.sort.each do |name|
  result = RunnerContractTest.new(name).run
  status = if result.error?
    "error"
  elsif result.failures.any? {{ |failure| !failure.is_a?(Minitest::Skip) }}
    "fail"
  elsif result.skipped?
    "skip"
  else
    "pass"
  end
  puts JSON.generate(id: "RunnerContractTest##{{name}}", status: status, assertions: result.assertions,
                     classes: result.failures.map {{ |f| f.respond_to?(:error) ? f.error.class.name : f.class.name }})
end
"#, version = minitest_version());
    let result = ruby().args(["-e", &oracle]).output().expect("native Minitest");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    read_rows(&String::from_utf8_lossy(&result.stdout))
}

const MOCHA_SOURCE: &str = r#"
class RunnerContractTest < ActiveSupport::TestCase
  test "a met expectation counts as an assertion" do
    Probe.expects(:ping).returns(1)
    assert_equal 1, Probe.ping
  end

  test "two met expectations count twice" do
    Probe.expects(:ping).returns(1)
    Probe.expects(:pong).returns(2)
    assert_equal 3, Probe.ping + Probe.pong
  end

  test "an unmet expectation fails" do
    Probe.expects(:ping)
    assert true
  end

  test "a failed test is not verified" do
    Probe.expects(:ping)
    assert false
  end

  test "a stub is not an assertion" do
    Probe.stubs(:ping).returns(5)
    assert_equal 5, Probe.ping
  end
end
"#;

#[test]
fn mocha_expectations_count_and_verify_as_under_mocha_minitest() {
    let native = native_mocha_rows(MOCHA_SOURCE);
    let (emitted, errors) = app(MOCHA_SOURCE).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (rows, log) = emitted_rows(&emitted);
    let projected: Vec<Value> = rows.iter().map(|row| serde_json::json!({
        "id": row["id"], "status": row["status"], "assertions": row["assertions"],
        "classes": row["exceptions"].as_array().unwrap().iter().map(|x| x["class"].clone()).collect::<Vec<_>>(),
    })).collect();
    assert_eq!(projected, native, "{log}");
}

/// A fixture load that raises in the reset (two rows under one unique
/// index: Rails' fixture insert raises ActiveRecord::RecordNotUnique in
/// setup_fixtures) errors every test in setup, before TestBase#setup's
/// mocha_setup. The row is that one error, with its site and backtrace;
/// teardown adds nothing.
#[test]
fn a_reset_error_is_the_only_exception_and_names_its_site() {
    let source = r#"
class RunnerContractTest < ActiveSupport::TestCase
  test "stubs, but its reset fails first" do
    Probe.expects(:ping).returns(1)
    assert_equal 1, Probe.ping
  end
end
"#;
    let (emitted, errors) = app(source)
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :probes do |t|\n    t.string :name\n    t.index :name, unique: true\n  end\nend\n")
        .write("test/fixtures/probes.yml", "first:\n  name: same\n\nsecond:\n  name: same\n")
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (rows, log) = emitted_rows(&emitted);
    assert_eq!(rows.len(), 1, "{log}");
    let row = &rows[0];
    assert_eq!(row["status"], "error", "{log}");
    let exceptions = row["exceptions"].as_array().unwrap();
    assert_eq!(exceptions.len(), 1, "teardown added an exception: {exceptions:#?}\n{log}");
    let x = &exceptions[0];
    assert_eq!(x["phase"], "setup", "{x:#}");
    let site = x["site"].as_str().unwrap();
    assert!(!site.is_empty() && !site.starts_with('/'), "site is a tree-relative frame: {x:#}");
    let backtrace: Vec<&str> = x["backtrace"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    assert!(!backtrace.is_empty() && backtrace.len() <= 12, "{x:#}");
    assert!(backtrace.contains(&site), "the site is one of the frames: {x:#}");
    assert!(backtrace.iter().all(|f| !f.contains("RoundhouseTestRunner.capture'")), "{x:#}");
}

/// A test body's exception: the site is the authored line, and the
/// backtrace runs from the raise to the runner's block, not through the
/// runner's own callers.
#[test]
fn a_test_error_row_has_the_authored_site_and_a_short_backtrace() {
    let source = r#"
class RunnerContractTest < ActiveSupport::TestCase
  def explode
    raise ArgumentError, "boom"
  end

  test "raises in a helper" do
    explode
  end
end
"#;
    let (emitted, errors) = app(source).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (rows, log) = emitted_rows(&emitted);
    let x = &rows[0]["exceptions"][0];
    assert_eq!(x["class"], "ArgumentError", "{log}");
    assert_eq!(x["phase"], "test", "{x:#}");
    let site = x["site"].as_str().unwrap();
    assert!(site.starts_with("test/models/runner_contract_test.rb:") && site.contains("explode"), "{x:#}");
    let backtrace: Vec<&str> = x["backtrace"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    assert_eq!(backtrace.first(), Some(&site), "{x:#}");
    assert!(backtrace.len() <= 4, "the runner's callers are not part of it: {x:#}");
    assert!(backtrace.last().unwrap().contains("block in RoundhouseTestRunner.run_one"), "{x:#}");
}

/// Minitest::Test#capture_exceptions, which Core's teardowns wrap a reset
/// in (PaymentMethod::DirectTest's Magellan reset, ElasticSearchTestHelper's
/// teardown callback): each failure, error or skip in the block is
/// recorded on the result in the phase it ran in, and the caller goes on,
/// as Minitest's `failures <<` does.
#[test]
fn capture_exceptions_records_and_continues_as_minitest_does() {
    let source = r#"
class RunnerContractTest < ActiveSupport::TestCase
  def teardown
    capture_exceptions { raise ArgumentError, "teardown boom" }
    super
  end

  test "captures and goes on" do
    capture_exceptions { assert_equal 1, 2 }
    capture_exceptions { raise KeyError, "body boom" }
    assert true
  end
end
"#;
    let (emitted, errors) = app(source).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (rows, log) = emitted_rows(&emitted);
    let row = &rows[0];
    assert_eq!(row["status"], "error", "{log}");
    assert_eq!(row["assertions"], 2, "{log}");
    let found: Vec<(String, String, String)> = row["exceptions"].as_array().unwrap().iter()
        .map(|x| (x["phase"].as_str().unwrap().into(), x["status"].as_str().unwrap().into(), x["class"].as_str().unwrap().into()))
        .collect();
    let expected: Vec<(String, String, String)> = [
        ("test", "fail", "Minitest::Assertion"),
        ("test", "error", "KeyError"),
        ("teardown", "error", "ArgumentError"),
    ].iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect();
    assert_eq!(found, expected, "{log}");
}

/// Core's Gemfile names rack-test in :test, and the app's Bundler.require
/// loads it before test_helper.rb. The gem's class stands, and a
/// controller reached through the in-process dispatch gets the
/// `ActionDispatch::Http::UploadedFile` Rails' multipart parse would.
fn rack_test_blog() -> emit_and_run::Overlay {
    emit_and_run::real_blog()
        .edit("Gemfile", "group :test do\n", "group :test do\n  gem \"rack-test\"\n")
        .edit(
            "config/routes.rb",
            "Rails.application.routes.draw do\n",
            "Rails.application.routes.draw do\n  post \"/uploads\", to: \"uploads#create\"\n",
        )
        .write(
            "app/controllers/uploads_controller.rb",
            r##"class UploadsController < ApplicationController
  skip_forgery_protection

  def create
    file = params[:file]
    render plain: "#{file.class.name}|#{file.original_filename}|#{file.content_type}|#{file.read}"
  end
end
"##,
        )
        .write(
            "test/controllers/uploads_controller_test.rb",
            r#"require "test_helper"

class UploadsControllerTest < ActionDispatch::IntegrationTest
  test "the gem's upload is the gem's class" do
    upload = Rack::Test::UploadedFile.new(StringIO.new("RIFF half"), "image/webp", original_filename: "broken.webp")
    assert_equal "broken.webp", upload.original_filename
    assert_equal "image/webp", upload.content_type
    assert_equal "Rack::Test::UploadedFile", upload.class.name
    assert_not upload.is_a?(ActionDispatch::Http::UploadedFile)
  end

  test "a controller gets the uploaded file Rails' multipart parse gives" do
    upload = Rack::Test::UploadedFile.new(StringIO.new("RIFF half"), "image/webp", original_filename: "broken.webp")
    post "/uploads", params: { file: upload }
    assert_response :success
    assert_equal "ActionDispatch::Http::UploadedFile|broken.webp|image/webp|RIFF half", response.body
  end
end
"#,
        )
}

/// Run as bin/run-target runs Core, under `RAILS_ENV=test`: the
/// Bundler.require facade loads a `group :test` gem only in that
/// environment, and the probe's boot reads it before test_helper.rb
/// would default it.
#[test]
fn a_loaded_rack_test_keeps_its_uploaded_file_and_dispatch_converts_it() {
    let (emitted, errors) = rack_test_blog().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let gem_facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap_or_default();
    assert!(gem_facades.contains("\"rack/test\"], [\"test\"]]"), "the probe's Bundler.require loads rack-test:\n{gem_facades}");
    let output = ruby()
        .args(["-Itest", "-I.", "test/controllers/uploads_controller_test.rb"])
        .current_dir(&emitted)
        .env("RAILS_ENV", "test")
        .output()
        .expect("emitted CRuby");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success() && stdout.contains("2 tests passed"), "stdout={stdout}\nstderr={stderr}");
}
