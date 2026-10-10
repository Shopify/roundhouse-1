//! Rails test-support surface the CRuby runner ports, against native
//! Minitest + ActiveSupport::TestCase: `ActiveSupport::Testing::
//! ConstantStubbing#stub_const` with its `exists:` branches (Core's tests
//! `include` the module by name), and Minitest's `name`, `passed?`,
//! `error?` and `skipped?` on the running test (Core's ActiveSupport::
//! TestCase#after_teardown reads `error? || skipped?` and `method(name)`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;

const TEST: &str = r##"# frozen_string_literal: true

require "test_helper"

module Limits
  MAX = 3
end

class StubConstTest < ActiveSupport::TestCase
  include ActiveSupport::Testing::ConstantStubbing

  test "stub_const swaps a constant for the block" do
    seen = stub_const(Limits, :MAX, 10) { Limits::MAX }
    assert_equal 10, seen
    assert_equal 3, Limits::MAX
  end

  test "stub_const refuses a missing constant unless exists is false" do
    error = assert_raises(NameError) { stub_const(Limits, :MIN, 1) { } }
    assert_match "Limits::MIN", error.message
    seen = stub_const(Limits, :MIN, 1, exists: false) { Limits.const_get(:MIN) }
    assert_equal 1, seen
    refute Limits.const_defined?(:MIN, false)
  end

  test "stub_const exists false refuses a defined constant" do
    error = assert_raises(NameError) { stub_const(Limits, :MAX, 1, exists: false) { } }
    assert_match "already defined constant MAX in Limits", error.message
  end

  test "the running test knows its name and result" do
    assert_equal "test_the_running_test_knows_its_name_and_result", name
    assert_equal __method__.to_s, name
    assert passed?
    refute error?
    refute skipped?
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
fn rails_test_support_ports_match_native() {
    let path = "test/models/stub_const_test.rb";
    let native_root = std::env::temp_dir().join(format!("rh-test-support-ports-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    let full = native_root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, TEST).unwrap();
    let native = native_ids(&native_root, path, &["StubConstTest"]);
    let _ = std::fs::remove_dir_all(&native_root);
    assert_eq!(native.len(), 4, "native oracle: {native:#?}");
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(path, TEST)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (ran, stderr) = emitted_ids(&emitted);
    assert_eq!(ran, native, "emitted results vs native\n{stderr}");
}
