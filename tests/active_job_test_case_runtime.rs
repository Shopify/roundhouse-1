//! `ActiveJob::TestCase` (runtime/spinel/test/test_helper.rb), the
//! parent Core's job tests write against: the emitter keeps the name
//! (only ActiveSupport::TestCase and Minitest::Test become TestBase), so
//! a tree without it failed every such file at load with
//! `NameError: uninitialized constant ActiveJob::TestCase` (38 rows of
//! unit-1000). It is Rails' class: TestBase with ActiveJob::TestHelper,
//! running the `:active_job_test_case` load hooks where the tree carries
//! lazy_load_hooks.
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
        include_str!("../runtime/spinel/scaffold/Gemfile.lock")
            .lines()
            .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
            .expect("scaffold Minitest lock")
            .to_string()
    })
}

const TEST_SOURCE: &str = r#"require "test_helper"

class NotifyJobTest < ActiveJob::TestCase
  test "enqueues through the helper" do
    assert_enqueued_jobs 1, only: NotifyJob do
      NotifyJob.perform_later("x")
    end
  end

  test "is a job test case" do
    assert_kind_of ActiveJob::TestHelper, self
    assert_equal ActiveJob::TestCase, self.class.superclass
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n")
        .write("app/models/thing.rb", "class Thing < ApplicationRecord\nend\n")
        .write("app/jobs/application_job.rb", "class ApplicationJob < ActiveJob::Base\nend\n")
        .write("app/jobs/notify_job.rb", "class NotifyJob < ApplicationJob\n  def perform(name)\n    nil\n  end\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :things do |t|\n    t.string :name\n  end\nend\n")
        .write("test/models/notify_job_test.rb", TEST_SOURCE)
}

fn emitted_rows(emitted: &Path, test: &str) -> (Vec<Value>, String) {
    let results = emitted.join("results.jsonl");
    let output = ruby()
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), test])
        .current_dir(emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("emitted CRuby");
    let log = format!(
        "tree={}\nstdout={}\nstderr={}",
        emitted.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(&results).unwrap_or_else(|error| panic!("no results: {error}\n{log}"));
    let mut rows: Vec<Value> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("JSON result row"))
        .collect();
    rows.sort_by_key(|row| row["id"].as_str().unwrap().to_string());
    (rows, log)
}

#[test]
fn an_active_job_test_case_file_loads_and_runs_its_tests() {
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    // The emitter writes the tests it runs under test/models.
    let test = "test/models/notify_job_test.rb";
    let source = std::fs::read_to_string(emitted.join(test))
        .unwrap_or_else(|e| panic!("{test} under {}: {e}", emitted.display()));
    assert!(source.contains("< ActiveJob::TestCase"), "{source}");
    let (rows, log) = emitted_rows(&emitted, test);
    let statuses: Vec<(String, String)> = rows
        .iter()
        .map(|row| (row["id"].as_str().unwrap().to_string(), row["status"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("NotifyJobTest#test_enqueues_through_the_helper".to_string(), "pass".to_string()),
            ("NotifyJobTest#test_is_a_job_test_case".to_string(), "pass".to_string()),
        ],
        "{rows:#?}\n{log}"
    );
    assert!(rows.iter().all(|row| row["assertions"].as_u64().unwrap() >= 1), "{rows:#?}");
}
