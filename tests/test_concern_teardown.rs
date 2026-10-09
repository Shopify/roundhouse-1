//! A `teardown do … end` in a concern's `included do` block.
//!
//! Core's `BillingApi::TestHelper` restores a configuration value and
//! clears the mocked request engine's stubs in one:
//!
//! ```ruby
//! included do
//!   setup do … end
//!   teardown do
//!     Rails.configuration.billing.billing_contract_service_provider = @original_contract_service_provider
//!     BillingApi::Connection.mock_request_engine.stubs = {}
//!   end
//! end
//! ```
//!
//! The test-ingest splice (`ingest::test_mixin`) refused every class that
//! included it ("a `teardown` in an `included do` block is not
//! modeled"). Rails registers the block as a teardown callback on the
//! class where the `include` stands; ActiveSupport's `after_teardown`
//! runs the callbacks after the `def teardown` chain, the latest
//! registered first. Each class here checks that order from the callback
//! that runs last, so a callback lost, run early, or run twice fails the
//! test, and its assertion counts.
//!
//! The second class's `def teardown` is a shared module's: the class's
//! own callbacks must not replace it (Rails still calls it before them).
//!
//! The native oracle is Core's pinned activesupport; the emitted CRuby
//! tree must list and run the same ids with the same outcomes and
//! assertion counts.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
  end
end
"#;

/// `BillingApi::TestHelper`'s shape: a setup and a teardown declared for
/// the includer.
const TEARDOWN_LOG: &str = r##"module TeardownLog
  extend ActiveSupport::Concern

  included do
    setup do
      @log = [:setup]
    end

    teardown do
      @log << :concern_teardown
    end
  end
end
"##;

/// A module that brings `TeardownLog` and defines `def teardown` itself.
const CLOSING_HELPER: &str = r##"module ClosingHelper
  extend ActiveSupport::Concern
  include TeardownLog

  #: -> void
  def teardown
    @log << :module_def_teardown
  end

  #: -> Array[Symbol]
  def logged
    @log
  end
end
"##;

const TESTS: &str = r##"class OrderedTeardownTest < ActiveSupport::TestCase
  teardown do
    @log << :first_registered
    assert_equal [:setup, :test, :def_teardown, :last_registered, :concern_teardown, :first_registered], @log
  end

  include TeardownLog

  teardown do
    @log << :last_registered
  end

  def teardown
    @log << :def_teardown
  end

  test "the callbacks run after the def, the latest registered first" do
    @log << :test
    assert_equal [:setup, :test], @log
  end
end

class ModuleDefTeardownTest < ActiveSupport::TestCase
  teardown do
    assert_equal [:setup, :test, :module_def_teardown, :class_teardown, :concern_teardown], @log
  end

  include ClosingHelper

  teardown do
    @log << :class_teardown
  end

  test "a shared module's def teardown runs before the callbacks" do
    @log << :test
    assert_equal [:setup, :test], logged
  end
end
"##;

const CLASSES: [&str; 2] = ["OrderedTeardownTest", "ModuleDefTeardownTest"];

fn minitest_version() -> String {
    include_str!("../runtime/spinel/scaffold/Gemfile.lock")
        .lines()
        .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
        .expect("scaffold Minitest lock")
        .to_string()
}

/// `id => (status, assertions)`, one row per line.
fn rows(text: &str) -> BTreeMap<String, (String, u64)> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| l.starts_with('{')) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        let outcome = (row["status"].as_str().unwrap().to_string(), row["assertions"].as_u64().unwrap());
        assert!(map.insert(id.clone(), outcome).is_none(), "{id} reported twice");
    }
    map
}

/// The classes run by native Minitest on Core's pinned ActiveSupport.
fn native() -> BTreeMap<String, (String, u64)> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_test_concern_teardown_native_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (path, text) in [("teardown_log.rb", TEARDOWN_LOG), ("closing_helper.rb", CLOSING_HELPER), ("tests.rb", TESTS)] {
        std::fs::write(dir.join(path), text).unwrap();
    }
    let program = format!(
        r##"require {oracle:?}
RailsOracle.activate("activesupport", minitest: "5.27.0")
require "minitest"
require "json"
Minitest.seed = 4242
require "active_support/all"
require "active_support/test_case"
abort "native: activesupport is not Core's" unless Gem.loaded_specs["activesupport"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)
load "teardown_log.rb"
load "closing_helper.rb"
load "tests.rb"
{classes:?}.each do |class_name|
  klass = Object.const_get(class_name)
  klass.runnable_methods.each do |name|
    result = klass.new(name).run
    status = result.passed? ? "pass" : (result.skipped? ? "skip" : (result.error? ? "error" : "fail"))
    puts JSON.generate(id: "#{{class_name}}##{{name}}", status: status, assertions: result.assertions)
  end
end
"##,
        classes = CLASSES,
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    rows(&stdout)
}

#[test]
fn concern_teardown_callbacks_run_where_rails_runs_them() {
    let native = native();
    let expected: BTreeMap<String, (String, u64)> = [
        "ModuleDefTeardownTest#test_a_shared_module's_def_teardown_runs_before_the_callbacks",
        "OrderedTeardownTest#test_the_callbacks_run_after_the_def,_the_latest_registered_first",
    ]
    .iter()
    .map(|id| (id.to_string(), ("pass".to_string(), 2)))
    .collect();
    assert_eq!(native, expected, "native oracle");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/post.rb", "class Post < ApplicationRecord\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/support/teardown_log.rb", TEARDOWN_LOG)
        .write("test/support/closing_helper.rb", CLOSING_HELPER)
        .write("test/models/teardown_test.rb", TESTS)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");

    let index: Value = serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index")).unwrap();
    let classes: Vec<&Value> = index["sources"].as_array().unwrap().iter().flat_map(|s| s["classes"].as_array().unwrap().iter()).collect();
    // A refused class is a named survey gap in the index, not a class
    // with tests.
    let mut indexed: Vec<String> = classes
        .iter()
        .filter(|c| CLASSES.contains(&c["class"].as_str().unwrap_or_default()))
        .flat_map(|c| {
            let class = c["class"].as_str().unwrap().to_string();
            c["tests"].as_array().into_iter().flatten().map(move |t| format!("{class}#{}", t.as_str().unwrap()))
        })
        .collect();
    indexed.sort();
    assert_eq!(indexed, native.keys().cloned().collect::<Vec<_>>(), "index lists exactly the native ids:\n{index:#}");

    let mut results = BTreeMap::new();
    for class in CLASSES {
        let file = classes
            .iter()
            .find(|c| c["class"] == class)
            .and_then(|c| c["file"].as_str())
            .unwrap_or_else(|| panic!("{class} is not in the index"))
            .to_string();
        let source = std::fs::read_to_string(emitted.join(&file)).expect("emitted test file");
        assert!(!source.contains("include TeardownLog"), "the concern is spliced onto {class}:\n{source}");
        let out_file = emitted.join(format!("{class}.results.jsonl"));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), &file])
            .current_dir(&emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &out_file)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!("{source}\nstdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{log}");
        let text = std::fs::read_to_string(&out_file).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        results.extend(rows(&text));
    }
    assert_eq!(results, native, "emitted results vs native");
}
