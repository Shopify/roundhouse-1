//! A helper module a test file declares beside its test class (Core's
//! `order_risks_controller_test.rb`: `module Admin; module
//! OrderAssessmentLockTestHelper; private; def expect_…` before `class
//! OrderRisksControllerTest`, which includes it) is defined as the file
//! loads, as natively. The walk over the file's classes went through the
//! module and kept nothing of it, so the include was a load-time
//! NameError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;

const TEST: &str = r##"# frozen_string_literal: true

require "test_helper"

module Admin
  module LockTestHelper
    LOCK_KEY = "order-lock"

    private

    def expect_lock
      "#{LOCK_KEY}:#{lock_suffix}"
    end
  end

  module Plain
    module Nested
      def self.answer
        42
      end
    end
  end

  class OrderLocksTest < ActiveSupport::TestCase
    include LockTestHelper

    def lock_suffix
      "a"
    end

    test "uses the helper module" do
      assert_equal "order-lock:a", expect_lock
      assert_equal 42, Plain::Nested.answer
    end

    test "the helper method stays private" do
      assert_includes LockTestHelper.private_instance_methods, :expect_lock
    end
  end

  class OrderLocksApiTest < ActiveSupport::TestCase
    include LockTestHelper

    def lock_suffix
      "api"
    end

    test "a second class of the file includes it too" do
      assert_equal "order-lock:api", expect_lock
    end
  end
end
"##;

fn minitest_version() -> String {
    include_str!("../runtime/spinel/scaffold/Gemfile.lock")
        .lines()
        .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
        .expect("scaffold Minitest lock")
        .to_string()
}

fn rows(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        assert!(map.insert(id.clone(), row["status"].as_str().unwrap().to_string()).is_none(), "{id} reported twice");
    }
    map
}

/// `id => status` for every test of `classes` Minitest runs natively.
fn native_ids(root: &std::path::Path, test: &str, classes: &[&str]) -> BTreeMap<String, String> {
    let runs: String = classes
        .iter()
        .map(|class| {
            format!(
                r##"{class}.runnable_methods.sort.each do |name|
  result = {class}.new(name).run
  status = result.passed? ? "pass" : (result.skipped? ? "skip" : (result.error? ? "error" : "fail"))
  puts JSON.generate(id: "{class}##{{name}}", status: status)
end
"##
            )
        })
        .collect();
    let oracle = format!(
        r##"gem "minitest", "={}"
require "minitest/test"
Minitest.seed = 4242
require "json"
require "active_support"
require "active_support/test_case"
$LOAD_PATH.unshift(File.join(Dir.pwd, "test"))
$LOADED_FEATURES << File.join(Dir.pwd, "test/test_helper.rb")
load {test:?}
{runs}"##,
        minitest_version()
    );
    let out = emit_and_run::ruby().args(["-e", &oracle]).current_dir(root).output().expect("native Minitest");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    rows(&String::from_utf8_lossy(&out.stdout))
}

/// Every emitted test file of `test/roundhouse_tests.json`, one process
/// each as the target runner does; `id => status`, and the stderr.
fn emitted_ids(emitted: &std::path::Path) -> (BTreeMap<String, String>, String) {
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index"))
            .unwrap();
    let mut files: Vec<String> = json["sources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["classes"].as_array().unwrap().iter().map(|c| c["file"].as_str().unwrap().to_string()))
        .collect();
    files.sort();
    files.dedup();
    let mut ran = BTreeMap::new();
    let mut stderr = String::new();
    for file in files {
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), &file])
            .current_dir(emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            .env("RAILS_ENV", "test")
            .output()
            .expect("emitted CRuby");
        let log = format!(
            "{file}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{log}");
        stderr.push_str(&String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice");
        }
    }
    (ran, stderr)
}

#[test]
fn a_test_files_helper_module_is_defined_for_its_test_classes() {
    let path = "test/controllers/admin/order_locks_test.rb";
    let native_root = std::env::temp_dir().join(format!("rh-helper-modules-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let full = native_root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, TEST).unwrap();
    let native = native_ids(&native_root, path, &["Admin::OrderLocksTest", "Admin::OrderLocksApiTest"]);
    let _ = std::fs::remove_dir_all(&native_root);
    assert_eq!(native.len(), 3, "native oracle: {native:#?}");
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(path, TEST)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (ran, stderr) = emitted_ids(&emitted);
    assert_eq!(ran, native, "emitted results vs native\n{stderr}");
}
