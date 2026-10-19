//! Rails, ActiveSupport and Mocha helpers count assertions as natively.
//!
//! Each case emits an authored `ActiveSupport::TestCase`, runs it on the
//! emitted CRuby tree, and compares every test's id, outcome and
//! assertion count with the same source run by real Minitest,
//! ActiveSupport, ActiveRecord and Mocha (`native_rows`). Every helper
//! has a passing and a failing case: a helper that raises its own error
//! instead of failing through `assert` reports `error` and loses the
//! count, which a pass alone does not show.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::process::Command;

const SCHEMA: &str = "ActiveRecord::Schema.define do\n  create_table :probes do |t|\n    t.string :name\n  end\nend\n";

const PROBE_MODEL: &str = r#"
class Probe < ApplicationRecord
  def self.ping
    :pong
  end

  def self.tally
    7
  end

  def touch_name
    name
  end
end
"#;

fn app(source: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n")
        .write("app/models/probe.rb", PROBE_MODEL)
        .write("app/jobs/probe_job.rb", PROBE_JOB)
        .write("app/jobs/args_job.rb", ARGS_JOB)
        .write("test/models/assertion_count_test.rb", source)
}

fn ruby() -> Command {
    Command::new(std::env::var("RUBY").unwrap_or_else(|_| "ruby".into()))
}

/// Core's Minitest (its gem home pins 5.27.0).
fn minitest_version() -> String {
    std::env::var("ROUNDHOUSE_ASSERTION_ORACLE_VERSION").unwrap_or_else(|_| "5.27.0".into())
}

/// Extra `-I` directories for both runs: Core's gem home carries mocha
/// 2.5.0 (`ROUNDHOUSE_ASSERTION_ORACLE_MOCHA_LIB=<gems>/mocha-2.5.0/lib`),
/// where the default gem path may resolve mocha 3. Pure Ruby, so a load
/// path is enough; unset, both runs take whatever `require` resolves.
fn shared_includes() -> Vec<String> {
    let mocha = std::env::var("ROUNDHOUSE_ASSERTION_ORACLE_MOCHA_LIB")
        .ok().filter(|lib| !lib.is_empty())
        .map(|lib| vec![format!("-I{lib}")]).unwrap_or_default();
    mocha.into_iter().chain(webmock_includes()).collect()
}

/// The webmock gem's lib directories, colon separated, with those of its
/// dependencies (`ROUNDHOUSE_ASSERTION_ORACLE_WEBMOCK_LIBS`: webmock,
/// addressable, public_suffix, crack, hashdiff, bigdecimal, rexml). The
/// default gem path carries none of them; unset, the webmock case is
/// skipped by name.
fn webmock_includes() -> Vec<String> {
    std::env::var("ROUNDHOUSE_ASSERTION_ORACLE_WEBMOCK_LIBS").ok()
        .map(|libs| libs.split(':').filter(|lib| !lib.is_empty()).map(|lib| format!("-I{lib}")).collect())
        .unwrap_or_default()
}

/// Native-only `-I` directories: a Rails checkout's component libs
/// (`ROUNDHOUSE_ASSERTION_ORACLE_RAILS=<checkout>`, Core's is the
/// bundler git checkout of Shopify/rails 8.2), in place of whatever
/// Rails the gem path resolves.
fn native_includes() -> Vec<String> {
    let Some(rails) = std::env::var("ROUNDHOUSE_ASSERTION_ORACLE_RAILS").ok().filter(|dir| !dir.is_empty()) else {
        return Vec::new();
    };
    ["activesupport", "activemodel", "activerecord", "activejob", "actionpack", "actionview", "railties"]
        .iter().map(|component| format!("-I{rails}/{component}/lib")).collect()
}

const PROBE_JOB: &str = "class ProbeJob < ActiveJob::Base\n  def perform(*args)\n  end\nend\n";

/// A job under an app-defined base, as Core's are (`BackgroundQueue::Job`):
/// it registers a callback, so it enqueues through the shared runtime's `ActiveJob::Enqueuing`, where
/// `ProbeJob`'s `perform_later` is the lowered class-side entry that logs
/// the job's name alone.
const ARGS_JOB: &str = "class ArgsBaseJob < ActiveJob::Base\n  after_perform { }\nend\n\nclass ArgsJob < ArgsBaseJob\n  def perform(*args)\n  end\nend\n";

static ORACLE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn read_rows(text: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = text.lines().filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("JSON result row")).collect();
    rows.sort_by_key(|row| row["id"].as_str().unwrap().to_string());
    rows
}

/// The authored source under real Minitest + ActiveSupport::TestCase +
/// ActiveRecord (SQLite in memory) + ActiveJob's test adapter +
/// mocha/minitest, with what the railties include into the test case.
/// Not minitest/mock: Core never loads it, so `Object#stub` is not
/// there natively either.
fn model_prelude() -> String {
    format!(r#"
require "active_support"
require "active_support/test_case"
require "active_record"
require "active_job"
require "active_job/test_helper"
require "active_record/testing/query_assertions"
require "mocha/minitest"
require "logger"
# What the railties include into ActiveSupport::TestCase.
ActiveSupport::TestCase.include ActiveJob::TestHelper
ActiveSupport::TestCase.include ActiveRecord::Assertions::QueryAssertions
ActiveJob::Base.queue_adapter = :test
ActiveJob::Base.logger = Logger.new(nil)
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Migration.verbose = false
{SCHEMA}
class ApplicationRecord < ActiveRecord::Base
  primary_abstract_class
end
{PROBE_MODEL}
{PROBE_JOB}
{ARGS_JOB}
"#)
}

/// A real Rails application (ActionController only) serving `routes` with
/// `controller`, for `ActionDispatch::IntegrationTest`.
fn integration_prelude(routes: &str, controller: &str) -> String {
    format!(r#"
require "logger"
require "rails"
require "action_controller/railtie"
require "mocha/minitest"
class OracleApp < Rails::Application
  config.eager_load = false
  config.secret_key_base = "x" * 64
  config.hosts.clear
  config.logger = Logger.new(nil)
end
OracleApp.initialize!
OracleApp.routes.draw do
{routes}
end
class ApplicationController < ActionController::Base
end
{controller}
"#)
}

fn native_rows(prelude: &str, source: &str) -> Vec<Value> {
    let version = minitest_version();
    let oracle = format!(r#"
gem "minitest", "={version}"
require "minitest/test"
require "json"
Minitest.seed = 4242
{prelude}
{source}
AssertionCountTest.runnable_methods.sort.each do |name|
  Probe.delete_all if defined?(Probe)
  result = AssertionCountTest.new(name).run
  status = if result.error?
    "error"
  elsif result.failures.any? {{ |failure| !failure.is_a?(Minitest::Skip) }}
    "fail"
  elsif result.skipped?
    "skip"
  else
    "pass"
  end
  puts JSON.generate(id: "AssertionCountTest##{{name}}", status: status, assertions: result.assertions,
    messages: result.failures.map {{ |f| f.respond_to?(:error) ? f.error.message : f.message }})
end
"#);
    // A file, not `-e`: ActiveSupport reads a lambda's source back from its
    // file for a failure message (`_callable_to_source_string`).
    let dir = std::env::temp_dir().join(format!("roundhouse-assertion-oracle-{}-{}", std::process::id(),
        ORACLE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("assertion_count_test.rb");
    std::fs::write(&path, &oracle).unwrap();
    let result = ruby().args(shared_includes()).args(native_includes()).arg(&path).output().expect("native Minitest");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    read_rows(&String::from_utf8_lossy(&result.stdout))
}

fn emitted_rows(overlay: emit_and_run::Overlay, test_path: &str) -> (Vec<Value>, String) {
    let (emitted, errors) = overlay.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let results = emitted.join("results.jsonl");
    let output = ruby()
        .args(shared_includes())
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), test_path])
        .current_dir(&emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .output().expect("emitted CRuby");
    let test_file = std::fs::read_to_string(emitted.join(test_path)).unwrap_or_default();
    let tests: Vec<String> = ["test", "test/models", "test/controllers"].iter().flat_map(|sub| {
        std::fs::read_dir(emitted.join(sub)).map(|dir| dir.flatten()
            .map(|entry| format!("{sub}/{}", entry.file_name().to_string_lossy())).collect::<Vec<_>>()).unwrap_or_default()
    }).filter(|path| path.contains("probe") || path.contains("assertion")).collect();
    let log = format!("emitted test ({test_path}; test/ holds {tests:?}):\n{test_file}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let text = std::fs::read_to_string(&results).unwrap_or_else(|error| panic!("no results: {error}\n{log}"));
    (read_rows(&text), log)
}

/// A native message as the emitted tree can spell it. Three renderings
/// are not the helpers' to reproduce: an ActiveRecord class's `inspect`
/// (`Probe(id: integer, name: string)`, the runtime's model class
/// answers `Probe`), an object's address (`#<Probe:0x430>`, which mocha
/// derives from `__id__`), and the SQL a query listing quotes (the
/// tree's SQLite statements leave `probes` unquoted).
fn unaddressed(message: &str) -> String {
    let mut out = message.replace("Probe(id: integer, name: string)", "Probe").replace("FROM \"probes\"", "FROM probes");
    while let Some(at) = out.find("#<Probe:0x") {
        let rest = &out[at + "#<Probe:0x".len()..];
        let Some(end) = rest.find('>') else { break };
        out.replace_range(at..at + "#<Probe:0x".len() + end + 1, "#<Probe>");
    }
    out
}

/// Same ids, outcomes and assertion counts as native; failure messages
/// too where `messages` names the tests whose message must match.
fn compare(source: &str, messages: &[&str]) -> Vec<Value> {
    let native = native_rows(&model_prelude(), source);
    let (rows, log) = emitted_rows(app(source), "test/models/assertion_count_test.rb");
    check(native, rows, log, messages)
}

/// `compare` for an `ActionDispatch::IntegrationTest` against a real
/// Rails application serving the same routes and controller.
fn compare_integration(routes: &str, controller: &str, source: &str, messages: &[&str]) -> Vec<Value> {
    let native = native_rows(&integration_prelude(routes, controller), source);
    let overlay = emit_and_run::empty_app()
        .write("db/schema.rb", SCHEMA)
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/controllers/probes_controller.rb", controller)
        .write("config/routes.rb", &format!("Rails.application.routes.draw do\n{routes}\nend\n"))
        .write("test/controllers/probes_controller_test.rb", source);
    // The emitted tree files every test class by its name under test/models.
    let (rows, log) = emitted_rows(overlay, "test/models/assertion_count_test.rb");
    check(native, rows, log, messages)
}

fn check(native: Vec<Value>, rows: Vec<Value>, log: String, messages: &[&str]) -> Vec<Value> {
    let project = |rows: &[Value]| -> Vec<Value> {
        rows.iter().map(|row| serde_json::json!({
            "id": row["id"], "status": row["status"], "assertions": row["assertions"],
        })).collect()
    };
    let messages_of = |row: &Value| -> Vec<String> {
        match row.get("exceptions") {
            Some(list) => list.as_array().unwrap().iter()
                .map(|e| format!("{} {}", e["class"].as_str().unwrap_or(""), e["message"].as_str().unwrap_or(""))).collect(),
            None => row["messages"].as_array().unwrap().iter().map(|m| m.as_str().unwrap().to_string()).collect(),
        }
    };
    let mut table = String::new();
    for (index, native_row) in native.iter().enumerate() {
        let emitted_row = rows.iter().find(|row| row["id"] == native_row["id"]);
        let same = emitted_row.is_some_and(|row| row["status"] == native_row["status"] && row["assertions"] == native_row["assertions"]);
        table.push_str(&format!("{} {} native {}/{} {:?}\n", if same { " " } else { "!" }, native_row["id"],
            native_row["status"], native_row["assertions"], messages_of(native_row)));
        match emitted_row {
            Some(row) => table.push_str(&format!("      emitted {}/{} {:?}\n", row["status"], row["assertions"], messages_of(row))),
            None => table.push_str(&format!("      emitted (missing) {index}\n")),
        }
    }
    assert_eq!(project(&rows), project(&native), "\n{table}\n{log}");
    for name in messages {
        let id = format!("AssertionCountTest#{name}");
        let native_row = native.iter().find(|row| row["id"] == id.as_str()).unwrap_or_else(|| panic!("no native {id}"));
        let emitted_row = rows.iter().find(|row| row["id"] == id.as_str()).unwrap();
        let emitted_messages: Vec<String> = emitted_row["exceptions"].as_array().unwrap().iter()
            .map(|e| e["message"].as_str().unwrap().to_string()).collect();
        let native_messages: Vec<String> = native_row["messages"].as_array().unwrap().iter()
            .map(|m| unaddressed(m.as_str().unwrap())).collect();
        assert_eq!(emitted_messages, native_messages, "{id}\n{table}\n{log}");
    }
    rows
}

#[test]
fn mocha_expectations_count_when_verified() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_expects
    Probe.expects(:ping).returns(:pong)
    assert_equal :pong, Probe.ping
  end

  def test_stubs_count_nothing
    Probe.stubs(:ping).returns(:pang)
    assert_equal :pang, Probe.ping
  end

  def test_never
    Probe.expects(:tally).never
  end

  def test_never_invoked_fails
    Probe.expects(:tally).never
    Probe.tally
  end

  def test_once_and_times
    Probe.expects(:ping).once.returns(:a)
    Probe.expects(:tally).times(2).returns(1)
    Probe.ping
    Probe.tally
    Probe.tally
  end

  def test_at_least_once
    Probe.expects(:ping).at_least_once.returns(:b)
    Probe.ping
    Probe.ping
  end

  def test_unmet_expectation_fails
    Probe.expects(:ping).returns(:a)
    Probe.expects(:tally).returns(1)
    Probe.ping
  end

  def test_failed_test_is_not_verified
    Probe.expects(:ping).returns(:a)
    assert_equal 1, 2
  end

  def test_mock_expects
    thing = mock("thing")
    thing.expects(:size).returns(3)
    assert_equal 3, thing.size
  end

  def test_mock_stubs_count_nothing
    thing = mock("thing", size: 3)
    assert_equal 3, thing.size
  end
end
"#, &["test_unmet_expectation_fails", "test_never_invoked_fails", "test_failed_test_is_not_verified"]);
}

/// The app-method slot (`MochaStub`): a bare `expects` on an instance or
/// `any_instance` of an app model.
#[test]
fn mocha_app_method_slots_count_when_verified() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_any_instance_expects
    Probe.any_instance.expects(:touch_name)
    assert_nil Probe.new(name: "x").touch_name
  end

  def test_any_instance_unmet_fails
    Probe.any_instance.expects(:touch_name)
  end

  def test_any_instance_times_unmet_fails
    Probe.any_instance.expects(:touch_name).times(2)
    Probe.new(name: "x").touch_name
  end

  def test_any_instance_over_called_fails
    Probe.any_instance.expects(:touch_name).once
    probe = Probe.new(name: "x")
    probe.touch_name
    probe.touch_name
  end

  def test_instance_never
    probe = Probe.new(name: "n")
    probe.expects(:touch_name).never
    assert_equal "n", probe.name
  end

  def test_instance_never_invoked_fails
    probe = Probe.new(name: "n")
    probe.expects(:touch_name).never
    probe.touch_name
  end

  def test_instance_expects_twice
    probe = Probe.new(name: "n")
    probe.expects(:touch_name).twice
    probe.touch_name
    probe.touch_name
  end
end
"#, &["test_any_instance_unmet_fails", "test_any_instance_times_unmet_fails", "test_any_instance_over_called_fails",
      "test_instance_never_invoked_fails"]);
}

/// Mocha's own stand-ins (`stub`, `stub_everything`, `mock` with stubs):
/// nothing to verify, nothing counted.
#[test]
fn mocha_stand_ins_count_nothing() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_stub_object
    thing = stub(size: 3)
    assert_equal 3, thing.size
  end

  def test_stub_everything
    thing = stub_everything("thing")
    assert_nil thing.whatever
  end

  def test_stubs_on_a_stub
    thing = stub("thing")
    thing.stubs(:size).returns(4)
    assert_equal 4, thing.size
  end
end
"#, &[]);
}

#[test]
fn assert_difference_counts_like_active_support() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_lambda
    assert_difference -> { Probe.count }, 1 do
      Probe.create!(name: "a")
    end
  end

  def test_string
    assert_difference "Probe.count" do
      Probe.create!(name: "a")
    end
  end

  def test_array
    assert_difference ["Probe.count", -> { Probe.where(name: "b").count }], 1 do
      Probe.create!(name: "b")
    end
  end

  def test_hash
    assert_difference "Probe.count" => 2, "Probe.where(name: 'c').count" => 1 do
      Probe.create!(name: "c")
      Probe.create!(name: "d")
    end
  end

  def test_value
    probe = assert_difference("Probe.count") { Probe.create!(name: "e") }
    assert_equal "e", probe.name
  end

  def test_ivar_string
    @before = Probe.create!(name: "i")
    assert_difference "Probe.where(name: @before.name).count", 1 do
      Probe.create!(name: "i")
    end
  end

  def test_fails
    assert_difference "Probe.count", 2 do
      Probe.create!(name: "f")
    end
  end

  def test_message_fails
    assert_difference "Probe.count", 1, "made one" do
      Probe.count
    end
  end

  def test_lambda_fails
    assert_difference -> { Probe.count } do
      Probe.count
    end
  end

  def test_block_raises
    assert_difference "Probe.count" do
      raise ArgumentError, "boom"
    end
  end

  def test_no_difference
    assert_no_difference "Probe.count" do
      Probe.where(name: "zz").count
    end
  end

  def test_no_difference_fails
    assert_no_difference -> { Probe.count } do
      Probe.create!(name: "g")
    end
  end
end
"#, &["test_fails", "test_message_fails", "test_lambda_fails", "test_block_raises", "test_no_difference_fails"]);
}

#[test]
fn assert_changes_counts_like_active_support() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_changes_from_to
    probe = Probe.create!(name: "h")
    assert_changes -> { probe.reload.name }, from: "h", to: "i" do
      probe.update!(name: "i")
    end
  end

  def test_changes_string
    @probe = Probe.create!(name: "h")
    assert_changes "@probe.reload.name" do
      @probe.update!(name: "j")
    end
  end

  def test_changes_fails
    probe = Probe.create!(name: "h")
    assert_changes -> { probe.reload.name } do
      probe.touch_name
    end
  end

  def test_changes_to_fails
    probe = Probe.create!(name: "h")
    assert_changes -> { probe.reload.name }, to: "z" do
      probe.update!(name: "y")
    end
  end

  def test_no_changes
    probe = Probe.create!(name: "k")
    assert_no_changes -> { probe.reload.name } do
      probe.touch_name
    end
  end

  def test_no_changes_from
    probe = Probe.create!(name: "k")
    assert_no_changes -> { probe.reload.name }, from: "k" do
      probe.touch_name
    end
  end

  def test_no_changes_fails
    probe = Probe.create!(name: "k")
    assert_no_changes -> { probe.reload.name } do
      probe.update!(name: "l")
    end
  end
end
"#, &["test_changes_fails", "test_changes_to_fails", "test_no_changes_fails"]);
}

#[test]
fn active_support_assertions_count_like_active_support() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_nothing_raised
    assert_nothing_raised { Probe.count }
  end

  def test_nothing_raised_errors
    assert_nothing_raised { raise ArgumentError, "boom" }
  end

  def test_raises_match
    assert_raises(ArgumentError, match: /bo+m/) { raise ArgumentError, "boom" }
  end

  def test_raises_match_fails
    assert_raises(ArgumentError, match: "bang") { raise ArgumentError, "boom" }
  end

  def test_not_predicate
    assert_not_predicate Probe.new(name: "m"), :persisted?
  end

  def test_not_predicate_fails
    assert_not_predicate "", :empty?
  end

  def test_assert_not
    assert_not Probe.exists?(name: "nope")
  end

  def test_assert_not_fails
    assert_not 1, "custom"
  end

  def test_assert_not_default_message_fails
    assert_not :present
  end
end
"#, &["test_nothing_raised_errors", "test_raises_match_fails", "test_not_predicate_fails", "test_assert_not_fails",
      "test_assert_not_default_message_fails"]);
}

/// No SQL inside these blocks: Rails 8.2 also reports each query as an
/// `active_record.sql` event, which the tree's ActiveRecord does not.
#[test]
fn event_reporter_assertions_count_like_active_support() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  include ActiveSupport::Testing::EventReporterAssertions

  def test_event_reported
    assert_event_reported("probe.pinged", payload: { id: 1 }) do
      ActiveSupport.event_reporter.notify("probe.pinged", id: 1)
    end
  end

  def test_none_reported_fails
    assert_event_reported("probe.pinged") { Probe.ping }
  end

  def test_mismatch_fails
    assert_event_reported("probe.pinged") do
      ActiveSupport.event_reporter.notify("probe.other", id: 2)
    end
  end

  def test_no_event_reported
    assert_no_event_reported("probe.pinged") { ActiveSupport.event_reporter.notify("probe.other") }
  end

  def test_no_event_at_all
    assert_no_event_reported { Probe.ping }
  end

  def test_no_event_reported_fails
    assert_no_event_reported("probe.pinged") { ActiveSupport.event_reporter.notify("probe.pinged", id: 3) }
  end

  def test_debug_event_reported
    with_debug_event_reporting do
      assert_event_reported("probe.debugged", payload: { id: 4 }) do
        ActiveSupport.event_reporter.debug("probe.debugged", id: 4)
      end
    end
  end
end
"#, &["test_none_reported_fails", "test_mismatch_fails", "test_no_event_reported_fails"]);
}

#[test]
fn error_reporter_assertions_count_like_active_support() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_error_reported
    assert_error_reported(ArgumentError) do
      ActiveSupport.error_reporter.report(ArgumentError.new("bad"), handled: true)
    end
  end

  def test_none_reported_fails
    assert_error_reported(ArgumentError) { Probe.count }
  end

  def test_other_reported_fails
    assert_error_reported(ArgumentError) do
      ActiveSupport.error_reporter.report(KeyError.new("bad"), handled: true)
    end
  end

  def test_no_error_reported
    assert_no_error_reported { Probe.count }
  end

  def test_no_error_reported_fails
    assert_no_error_reported { ActiveSupport.error_reporter.report(ArgumentError.new("bad")) }
  end
end
"#, &["test_none_reported_fails", "test_other_reported_fails"]);
}

#[test]
fn job_assertions_count_like_active_job() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_enqueued_jobs_block
    assert_enqueued_jobs 1 do
      ProbeJob.perform_later(1)
    end
  end

  def test_enqueued_jobs_plain
    ProbeJob.perform_later(1)
    assert_enqueued_jobs 1
  end

  def test_enqueued_jobs_fails
    assert_enqueued_jobs 2 do
      ProbeJob.perform_later(1)
    end
  end

  def test_no_enqueued_jobs
    assert_no_enqueued_jobs { Probe.count }
  end

  def test_no_enqueued_jobs_fails
    assert_no_enqueued_jobs { ProbeJob.perform_later(2) }
  end

  def test_enqueued_with
    assert_enqueued_with(job: ProbeJob) { ProbeJob.perform_later(3) }
  end

  def test_enqueued_with_plain
    ProbeJob.perform_later(3)
    assert_enqueued_with(job: ProbeJob)
  end

  def test_enqueued_with_fails
    assert_enqueued_with(job: ProbeJob) { Probe.count }
  end

  def test_perform_enqueued_jobs_block
    perform_enqueued_jobs do
      ProbeJob.perform_later(4)
    end
  end

  # A callable `args:` is called with the enqueued job's arguments, and
  # what it asserts counts on the test.
  def test_enqueued_with_callable_args
    expected = ->(args) do
      assert_equal(1, args.first)
      assert_equal({ name: "a" }, args.last)
    end
    assert_enqueued_with(job: ArgsJob, args: expected) { ArgsJob.perform_later(1, { name: "a" }) }
  end

  def test_enqueued_with_callable_args_plain
    ArgsJob.perform_later(5)
    assert_enqueued_with(job: ArgsJob, args: ->(args) { assert_equal([5], args) })
  end

  def test_enqueued_with_callable_args_fails
    assert_enqueued_with(job: ArgsJob, args: ->(args) { assert_equal(9, args.first) }) { ArgsJob.perform_later(6) }
  end
end
"#, &["test_enqueued_jobs_fails", "test_no_enqueued_jobs_fails", "test_enqueued_with_fails", "test_enqueued_with_callable_args_fails"]);
}

/// webmock/minitest wraps `assert_request_requested` and
/// `assert_request_not_requested` (behind `assert_requested` and
/// `assert_not_requested`) to count one assertion each on
/// `Minitest::Test`; the emitted TestBase is not one and counts them
/// itself.
#[test]
fn webmock_request_assertions_count_like_webmock() {
    if webmock_includes().is_empty() {
        eprintln!("skipped: ROUNDHOUSE_ASSERTION_ORACLE_WEBMOCK_LIBS names no webmock gem");
        return;
    }
    let source = r#"
class AssertionCountTest < ActiveSupport::TestCase
  # Names WebMock, which is what wires the gem into the emitted tree.
  include WebMock::API

  def test_requested
    stub = stub_request(:get, "http://probe.test/a").to_return(body: "ok")
    Net::HTTP.get(URI("http://probe.test/a"))
    assert_requested(stub)
  end

  def test_requested_times
    stub = stub_request(:get, "http://probe.test/b").to_return(body: "ok")
    2.times { Net::HTTP.get(URI("http://probe.test/b")) }
    assert_requested(stub, times: 2)
    assert_equal(1, 1)
  end

  def test_not_requested
    stub_request(:get, "http://probe.test/c").to_return(body: "ok")
    assert_not_requested(:get, "http://probe.test/c")
  end

  def test_requested_fails
    stub = stub_request(:get, "http://probe.test/d").to_return(body: "ok")
    assert_requested(stub)
  end

  def test_not_requested_fails
    stub_request(:get, "http://probe.test/e").to_return(body: "ok")
    Net::HTTP.get(URI("http://probe.test/e"))
    assert_not_requested(:get, "http://probe.test/e")
  end
end
"#;
    let prelude = format!("{}\nrequire \"net/http\"\nrequire \"webmock/minitest\"\n", model_prelude());
    let native = native_rows(&prelude, source);
    let (rows, log) = emitted_rows(app(source), "test/models/assertion_count_test.rb");
    check(native, rows, log, &[]);
}

#[test]
fn query_assertions_count_like_active_record() {
    compare(r#"
class AssertionCountTest < ActiveSupport::TestCase
  def test_no_queries
    probe = Probe.new(name: "q")
    assert_no_queries { probe.name }
  end

  def test_no_queries_fails
    assert_no_queries { Probe.count }
  end

  def test_queries_count
    assert_queries_count(1) { Probe.count }
  end

  def test_queries_count_fails
    assert_queries_count(2) { Probe.count }
  end

  def test_queries_match
    assert_queries_match(/COUNT/) { Probe.count }
  end

  def test_queries_match_fails
    assert_queries_match(/DELETE/) { Probe.count }
  end

  def test_no_queries_match
    assert_no_queries_match(/DELETE/) { Probe.count }
  end
end
"#, if native_includes().is_empty() {
        // Rails 8.1 words these messages differently from Core's 8.2.
        &[]
    } else {
        &["test_no_queries_fails", "test_queries_count_fails", "test_queries_match_fails"]
    });
}

const PROBES_ROUTES: &str = r#"  get "/probe" => "probes#show"
  get "/gone" => "probes#gone"
  get "/missing" => "probes#missing"
  get "/created" => "probes#created"
  get "/away" => "probes#away""#;

const PROBES_CONTROLLER: &str = r#"class ProbesController < ApplicationController
  def show
    render plain: "ok"
  end

  def gone
    redirect_to "/probe"
  end

  def missing
    head :not_found
  end

  def created
    head :created
  end

  def away
    redirect_to "http://other.example.org/x", allow_other_host: true
  end
end
"#;

#[test]
fn response_assertions_count_like_action_dispatch() {
    compare_integration(PROBES_ROUTES, PROBES_CONTROLLER, r#"
class AssertionCountTest < ActionDispatch::IntegrationTest
  def test_success
    get "/probe"
    assert_response :success
  end

  def test_symbol_code
    get "/created"
    assert_response :created
  end

  def test_numeric_code
    get "/created"
    assert_response 201
  end

  def test_missing
    get "/missing"
    assert_response :missing
  end

  def test_success_fails
    get "/missing"
    assert_response :success
  end

  def test_symbol_code_fails
    get "/probe"
    assert_response :created
  end

  def test_redirect_on_redirect_fails
    get "/gone"
    assert_response :success
  end

  def test_redirected_to_path
    get "/gone"
    assert_redirected_to "/probe"
  end

  def test_redirected_to_url
    get "/gone"
    assert_redirected_to "http://www.example.com/probe"
  end

  def test_redirected_to_regexp
    get "/gone"
    assert_redirected_to %r{/pro}
  end

  def test_redirected_to_fails
    get "/gone"
    assert_redirected_to "/elsewhere"
  end

  def test_redirected_to_not_a_redirect_fails
    get "/probe"
    assert_redirected_to "/probe"
  end
end
"#, &["test_success_fails", "test_symbol_code_fails", "test_redirect_on_redirect_fails", "test_redirected_to_fails",
      "test_redirected_to_not_a_redirect_fails"]);
}

/// An authored `capture_queries(*args, **kwargs)` on the test case (a
/// helper's own capture API) is not the runtime assertions' capture: Rails'
/// `assert_queries_count` subscribes its own `SQLCounter` and never calls it.
/// Emitted side only: every test passes and counts its assertion.
#[test]
fn query_assertions_do_not_route_through_an_authored_capture_queries() {
    let source = r#"
module CaptureQueriesHelper
  def capture_queries(*args, **kwargs)
    yield
    []
  end
end

class AssertionCountTest < ActiveSupport::TestCase
  include CaptureQueriesHelper

  def test_no_queries
    probe = Probe.new(name: "q")
    assert_no_queries { probe.name }
  end

  def test_queries_count
    assert_queries_count(1) { Probe.count }
  end

  def test_queries_match
    assert_queries_match(/COUNT/) { Probe.count }
  end
end
"#;
    let (rows, log) = emitted_rows(app(source), "test/models/assertion_count_test.rb");
    assert_eq!(rows.len(), 3, "{log}");
    for row in &rows {
        assert_eq!(row["status"], "pass", "{row}\n{log}");
        assert!(row["assertions"].as_i64() >= Some(1), "{row}\n{log}");
    }
}
