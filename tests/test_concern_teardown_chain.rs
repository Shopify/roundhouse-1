//! Teardown callbacks a concern's `included do` block registers, in the
//! chains Rails builds around them.
//!
//! `ingest::test_mixin` rebuilds a spliced class's `teardown` from the
//! callbacks its includes and body register. Rails runs them from
//! ActiveSupport's `after_teardown`, after the whole `def teardown` chain
//! and whatever that raised, the latest registered first. Each class
//! below checks one part of that from the callback that runs last:
//!
//! - a plain module's or a base class's `def teardown` still runs first;
//! - a concern the superclass already included does not register its
//!   `setup` and `teardown` callbacks a second time
//!   (`Concern#append_features` returns early when `base < self`);
//! - `next` ends only its own callback (the block it is in);
//! - a class-body `def teardown` and an included block's replace each
//!   other in source order, as any two `def`s do;
//! - the callbacks still run when the `def teardown` raises.
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

/// Asserts the base class's `def teardown` ran before it.
const BASE_AWARE: &str = r##"module BaseAwareTeardown
  extend ActiveSupport::Concern

  included do
    teardown do
      assert_equal [:setup, :test, :base_def], @log
    end
  end
end
"##;

/// Counts its own `setup` and `teardown` callbacks.
const COUNTING: &str = r##"module CountingConcern
  extend ActiveSupport::Concern

  included do
    setup do
      @setups = (@setups || 0) + 1
    end

    teardown do
      @teardowns = (@teardowns || 0) + 1
      assert_equal 1, @teardowns
    end
  end
end
"##;

/// `next` at a callback's top level, and in a block inside one.
const SKIPPING: &str = r##"module SkippingCallbacks
  extend ActiveSupport::Concern

  included do
    setup do
      @log = [:setup]
      next if @log.size == 1
      @log << :setup_after_next
    end

    teardown do
      [1, 2].each do |i|
        next if i == 1
        @log << i
      end
      next if @log.include?(:skip)
      @log << :teardown_after_next
    end
  end
end
"##;

/// An included block that defines `def teardown` on the class.
const BLOCK_DEF: &str = r##"module BlockDefTeardown
  extend ActiveSupport::Concern

  included do
    setup do
      @log = [:setup]
    end

    def teardown
      @log << :block_def
    end

    teardown do
      @log << :block_callback
    end
  end
end
"##;

/// Not a concern: a plain module's `def teardown`, reached through the
/// spliced class's `super`.
const PLAIN_CLEANUP: &str = r##"module PlainCleanup
  #: -> void
  def teardown
    super
  ensure
    @log << :plain_module_teardown
  end
end
"##;

const TESTS: &str = r##"class PlainModuleTeardownTest < ActiveSupport::TestCase
  teardown do
    assert_equal [:setup, :test, :plain_module_teardown, :concern_teardown], @log
  end

  include TeardownLog
  include PlainCleanup

  test "a plain module's def teardown runs before the callbacks" do
    @log << :test
    assert true
  end
end

class BaseWithDefTest < ActiveSupport::TestCase
  setup do
    @log = [:setup]
  end

  def teardown
    @log << :base_def
  end

  test "base" do
    @log << :test
    assert true
  end
end

class ChildConcernTest < BaseWithDefTest
  include BaseAwareTeardown

  test "child" do
    @log << :test
    assert true
  end
end

class BaseCountingTest < ActiveSupport::TestCase
  include CountingConcern

  test "base counts" do
    assert_equal 1, @setups
  end
end

class SubCountingTest < BaseCountingTest
  include CountingConcern

  test "sub counts" do
    assert_equal 1, @setups
  end
end

class PlainSubCountingTest < BaseCountingTest
  test "plain sub counts" do
    assert_equal 1, @setups
  end
end

class SkippingCallbackTest < ActiveSupport::TestCase
  teardown do
    expected = @log.include?(:skip) ? [:setup, :skip, 2] : [:setup, :run, 2, :teardown_after_next]
    assert_equal expected, @log
  end

  include SkippingCallbacks

  test "next ends the callback it is in" do
    @log << :skip
    assert_equal [:setup, :skip], @log
  end

  test "a callback runs on past a next in its inner block" do
    @log << :run
    assert_equal [:setup, :run], @log
  end
end

class LaterIncludeDefTest < ActiveSupport::TestCase
  teardown do
    assert_equal [:setup, :test, :block_def, :block_callback], @log
  end

  def teardown
    @log << :class_def
  end

  include BlockDefTeardown

  test "an included block's def teardown replaces the class's earlier one" do
    @log << :test
    assert true
  end
end

class EarlierIncludeDefTest < ActiveSupport::TestCase
  teardown do
    assert_equal [:setup, :test, :class_def, :block_callback], @log
  end

  include BlockDefTeardown

  def teardown
    @log << :class_def
  end

  test "a class def teardown after the include replaces the block's" do
    @log << :test
    assert true
  end
end

class RaisingDefTeardownTest < ActiveSupport::TestCase
  teardown do
    assert_equal [:setup, :test, :raising_def, :concern_teardown], @log
  end

  include TeardownLog

  def teardown
    @log << :raising_def
    raise "def teardown failed"
  end

  test "the callbacks still run when def teardown raises" do
    @log << :test
    assert true
  end
end
"##;

const SUPPORT: [(&str, &str); 5] = [
    ("teardown_log.rb", TEARDOWN_LOG),
    ("base_aware_teardown.rb", BASE_AWARE),
    ("counting_concern.rb", COUNTING),
    ("skipping_callbacks.rb", SKIPPING),
    ("block_def_teardown.rb", BLOCK_DEF),
];

const CLASSES: [&str; 10] = [
    "PlainModuleTeardownTest",
    "BaseWithDefTest",
    "ChildConcernTest",
    "BaseCountingTest",
    "SubCountingTest",
    "PlainSubCountingTest",
    "SkippingCallbackTest",
    "LaterIncludeDefTest",
    "EarlierIncludeDefTest",
    "RaisingDefTeardownTest",
];

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
    let dir = std::env::temp_dir().join(format!("rh_test_concern_teardown_chain_native_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut files: Vec<&str> = Vec::new();
    for (path, text) in SUPPORT.iter().copied().chain([("plain_cleanup.rb", PLAIN_CLEANUP), ("tests.rb", TESTS)]) {
        std::fs::write(dir.join(path), text).unwrap();
        files.push(path);
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
{files:?}.each {{ |f| load f }}
{classes:?}.each do |class_name|
  klass = Object.const_get(class_name)
  klass.runnable_methods.sort.each do |name|
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
fn concern_teardown_callbacks_chain_as_rails_chains_them() {
    let native = native();
    let pass = |id: &str| (id.to_string(), ("pass".to_string(), 2));
    let mut expected: BTreeMap<String, (String, u64)> = [
        "PlainModuleTeardownTest#test_a_plain_module's_def_teardown_runs_before_the_callbacks",
        "ChildConcernTest#test_base",
        "ChildConcernTest#test_child",
        "BaseCountingTest#test_base_counts",
        "SubCountingTest#test_base_counts",
        "SubCountingTest#test_sub_counts",
        "PlainSubCountingTest#test_base_counts",
        "PlainSubCountingTest#test_plain_sub_counts",
        "SkippingCallbackTest#test_next_ends_the_callback_it_is_in",
        "SkippingCallbackTest#test_a_callback_runs_on_past_a_next_in_its_inner_block",
        "LaterIncludeDefTest#test_an_included_block's_def_teardown_replaces_the_class's_earlier_one",
        "EarlierIncludeDefTest#test_a_class_def_teardown_after_the_include_replaces_the_block's",
    ]
    .iter()
    .map(|id| pass(id))
    .collect();
    expected.insert("BaseWithDefTest#test_base".into(), ("pass".into(), 1));
    // The def's error, after the callback's assertion ran and passed.
    expected.insert(
        "RaisingDefTeardownTest#test_the_callbacks_still_run_when_def_teardown_raises".into(),
        ("error".into(), 2),
    );
    assert_eq!(native, expected, "native oracle");

    let mut app = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/post.rb", "class Post < ApplicationRecord\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("lib/plain_cleanup.rb", PLAIN_CLEANUP)
        .write("test/models/teardown_chain_test.rb", TESTS);
    for (path, text) in SUPPORT {
        app = app.write(&format!("test/support/{path}"), text);
    }
    let (emitted, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");

    let index: Value = serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index")).unwrap();
    let classes: Vec<&Value> = index["sources"].as_array().unwrap().iter().flat_map(|s| s["classes"].as_array().unwrap().iter()).collect();
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
        let out_file = emitted.join(format!("{class}.results.jsonl"));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), &file])
            .current_dir(&emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &out_file)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!("{source}\nstdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(&out_file).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        // A subclass's file loads its base's, whose driver runs too.
        for (id, outcome) in rows(&text) {
            if id.starts_with(&format!("{class}#")) {
                results.insert(id, outcome);
            }
        }
        let own = results.keys().filter(|id| id.starts_with(&format!("{class}#"))).count();
        assert!(own > 0, "{class} ran no tests:\n{log}");
    }
    assert_eq!(results, native, "emitted results vs native");
}
