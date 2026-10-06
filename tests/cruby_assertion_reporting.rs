//! Count the actual authored assertions, not the number of lowered raise nodes.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::process::Command;

const SOURCE: &str = r#"
class AssertionContractTest < Minitest::Test
  def test_query_shape
    error = assert_raises(RuntimeError) { raise "SELECT missing Backtrace:" }
    assert_equal 1, 1
    assert_match(/SELECT/, error.message)
    assert_match "Backtrace:", error.message
  end

  def test_nested
    error = assert_raises(RuntimeError) do
      assert true
      raise "boom"
    end
    assert_equal "boom", error.message
  end

  def test_wrong_exception
    assert_raises(ArgumentError) { raise "boom" }
  end

  def test_nested_failure
    assert_raises(RuntimeError) { assert false }
  end

  def fail_value
    raise "argument failed"
  end

  def test_argument_error
    assert_equal 1, fail_value
  end

  def mark(value)
    @seen = @seen + value
    value
  end

  def test_argument_order
    @seen = ""
    assert_equal mark("a"), mark("a"), mark("m")
    assert_equal "aam", @seen
  end

  def test_return_value
    assert_equal true, assert(7)
  end

  def test_skip
    assert true
    skip "later"
  end

  def test_regex_failure
    assert_match(/absent/, "here")
  end
end
"#;

const LIFECYCLE_HELPER: &str = r#"
module AssertionLifecycleHelper
  def setup
    super
    @trace = @trace + " module"
  end
  def teardown
    @trace = @trace + " teardown"
    super
  end
end
"#;

fn app(source: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :probes do |t|\n    t.string :name\n  end\nend\n")
        .write("lib/assertion_lifecycle_helper.rb", LIFECYCLE_HELPER)
        .write("test/models/assertion_contract_test.rb", source)
}

fn ruby() -> Command {
    Command::new(std::env::var("RUBY").unwrap_or_else(|_| "ruby".into()))
}

fn minitest_version() -> String {
    // Compare the exact dependency resolved by the emitted scaffold. An
    // explicit override also exercises Core's older Minitest contract.
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

fn native_rows(source: &str) -> Vec<Value> {
    let version = minitest_version();
    let rails = if source.contains("ActiveSupport::TestCase") {
        "require 'active_support'; require 'active_support/test_case'"
    } else { "" };
    let oracle = format!(r#"
gem "minitest", "={version}"
require "minitest/test"
require "json"
Minitest.seed = 4242
{rails}
{LIFECYCLE_HELPER}
{source}
AssertionContractTest.runnable_methods.each do |name|
  result = AssertionContractTest.new(name).run
  status = if result.error?
    "error"
  elsif result.failures.any? {{ |failure| !failure.is_a?(Minitest::Skip) }}
    "fail"
  elsif result.skipped?
    "skip"
  else
    "pass"
  end
  puts JSON.generate(id: "AssertionContractTest##{{name}}", status: status, assertions: result.assertions, minitest_version: Gem.loaded_specs.fetch("minitest").version.to_s)
end
"#);
    let result = ruby().args(["-e", &oracle]).output().expect("native Minitest");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    read_rows(&String::from_utf8_lossy(&result.stdout))
}

#[test]
fn setup_callbacks_precede_included_setup_and_keep_super_chains() {
    compare(r#"
class AssertionContractTest < ActiveSupport::TestCase
  include AssertionLifecycleHelper
  setup do
    @trace = "callback"
    assert true
  end
  def setup
    super
    assert_equal "callback module", @trace
  end
  def test_lifecycle
    assert_equal "callback module", @trace
  end
  def teardown
    super
    assert_equal "callback module teardown", @trace
  end
end
"#);
}

#[test]
fn authored_assertion_override_keeps_precedence() {
    compare(r#"
class AssertionContractTest < Minitest::Test
  def assert_equal(expected, actual)
    assert expected == actual
    "owned"
  end
  def test_override
    result = assert_equal(1, 1)
    assert result == "owned"
  end
end
"#);
}

fn compare(source: &str) -> Vec<Value> {
    let native = native_rows(source);
    let (emitted, errors) = app(source).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let results = emitted.join("results.jsonl");
    let output = ruby()
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), "test/models/assertion_contract_test.rb"])
        .current_dir(&emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .output().expect("emitted CRuby");
    let text = std::fs::read_to_string(&results).unwrap_or_else(|error| panic!(
        "no results: {error}\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)));
    let rows = read_rows(&text);
    let projected: Vec<Value> = rows.iter().map(|row| serde_json::json!({
        "id": row["id"], "status": row["status"], "assertions": row["assertions"], "minitest_version": row["minitest_version"]
    })).collect();
    assert_eq!(projected, native, "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.status.success(), native.iter().all(|row| row["status"] == "pass" || row["status"] == "skip"));
    rows
}

#[test]
fn emitted_assertions_match_native_counts_outcomes_and_evaluation() {
    let rows = compare(SOURCE);
    assert_eq!(rows.len(), 9);
    let target = rows.iter().find(|row| row["id"] == "AssertionContractTest#test_query_shape").unwrap();
    assert_eq!(target["assertions"], 6);
}

#[test]
fn setup_and_teardown_failures_keep_counts_and_all_exceptions() {
    let setup = r#"
class AssertionContractTest < Minitest::Test
  def setup
    super
    assert true
    raise "setup failed"
  end
  def test_not_reached
    assert false
  end
  def teardown
    assert true
    super
  end
end
"#;
    let rows = compare(setup);
    assert_eq!(rows[0]["assertions"], 2);
    assert_eq!(rows[0]["exceptions"][0]["phase"], "setup");

    let teardown = r#"
class AssertionContractTest < Minitest::Test
  def test_skip_then_error
    assert true
    skip "later"
  end
  def test_fail_then_error
    assert false
  end
  def teardown
    assert true
    raise "teardown failed"
  end
end
"#;
    for row in compare(teardown) {
        assert_eq!(row["assertions"], 2);
        assert_eq!(row["exceptions"].as_array().unwrap().len(), 2);
        assert_eq!(row["exceptions"][1]["phase"], "teardown");
    }
}

#[test]
fn only_cruby_projects_use_minitest_assertions() {
    let source = "class AssertionContractTest < Minitest::Test\n  def test_one\n    assert_equal 1, 1\n  end\nend\n";
    for target in [BuildTarget::Ruby, BuildTarget::Jruby, BuildTarget::Spinel] {
        let (emitted, errors) = app(source).emit(target);
        assert!(errors.is_empty(), "{target:?}: {errors:#?}");
        let uses_minitest = matches!(target, BuildTarget::Ruby);
        let path = if matches!(target, BuildTarget::Spinel) {
            "test/assertion_contract_test.rb"
        } else {
            "test/models/assertion_contract_test.rb"
        };
        let code = std::fs::read_to_string(emitted.join(path)).unwrap();
        let helper = std::fs::read_to_string(emitted.join("test/test_helper.rb")).unwrap();
        assert_eq!(code.contains("RoundhouseTestRunner.run("), uses_minitest, "{target:?}");
        assert_eq!(helper.contains("require_relative \"minitest_runner\""), uses_minitest, "{target:?}");
        assert_eq!(emitted.join("test/minitest_runner.rb").exists(), uses_minitest, "{target:?}");
        assert_eq!(code.contains("assert_equal failed"), !uses_minitest, "{target:?}");
    }
}

#[test]
fn inline_backends_refuse_setup_method_control_flow() {
    let source = "class AssertionContractTest < Minitest::Test\n  def setup\n    super\n  end\n  def test_one\n    assert_equal 1, 1\n  end\nend\n";
    for target in [BuildTarget::Jruby, BuildTarget::Spinel] {
        let (_, errors) = app(source).emit(target);
        assert!(errors.iter().any(|error| error.contains("test_setup_control_flow")), "{target:?}: {errors:#?}");
    }
}
