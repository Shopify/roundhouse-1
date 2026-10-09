//! Core's `TestAccessBlocker` (components/platform/essentials/test/support/
//! test_access_blocker.rb): `class TestAccessBlocker < Module`, whose
//! `initialize` defines `block_<name>`, `unblock_<name>`, `before_setup` and
//! `after_teardown` with `define_method`, and whose `BlockedObject <
//! BasicObject` delegates `class`, `is_a?`, `respond_to?` and `inspect`.
//! Test helpers (`TestMacros::GcsHelper`) make one in their module body,
//! `register` getters and setters on it there, and `self.include` it, so
//! every test including the helper has its access blocked in `before_setup`
//! until it calls `unblock_<name>`.
//!
//! The class's file sits in a test directory beside its own test, declares
//! no test class, and is a library class of the tree; the helper's
//! `register` calls and its computed `include` run in its body; and the
//! emitted test runs Minitest's lifecycle hooks, so the module's
//! `before_setup` / `after_teardown` and their `super` run. The emitted
//! results are the native ones (a Minitest oracle over the same sources).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;

const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";

const BUCKET_CONFIG: &str = r##"
class BucketConfig
  class << self
    attr_accessor :bucket
  end
  self.bucket = "real-bucket"
end
"##;

/// A class elsewhere in the app with anonymous positional rests named
/// like the blocked object's delegations.
const REST_THING: &str = r##"
class RestThing
  def inspect(*)
    "rt"
  end

  def respond_to?(*)
    true
  end
end
"##;

/// Core's file, trimmed of comments and the prewarm path.
const TEST_ACCESS_BLOCKER: &str = r##"
class TestAccessBlocker < Module
  class ObjectBlocker
    attr_reader :key

    def initialize(name, getter, setter, key, message)
      super()
      @blocked_object = BlockedObject.new(name, getter, key, message)
      @key = key
      @key.freeze
      @setter = setter
    end

    def hash
      key.hash
    end

    def eql?(other)
      key == other.key
    end

    def block
      @blocked_object.__object
      @setter.call(@blocked_object)
    end

    def unblock
      @setter.call(@blocked_object.__object)
    end
  end

  class BlockedObject < BasicObject
    delegate(:class, :is_a?, :nil?, :respond_to?, to: :__object)

    def initialize(name, getter, key, message = nil)
      super()
      @object = getter
      @getter = getter
      @key = key
      @message = message
      @name = name
      @loaded = false
    end

    def to_s
      "#<#{__object.class}>"
    end
    delegate :inspect, to: :to_s

    def __object
      return @object if @loaded

      @object = @object.call if @object.respond_to?(:call)
      @loaded = true
      @object
    end

    private

    def method_missing(method_name, ...)
      ::Kernel.raise(::Minitest::Assertion, blocked_access_message)
    end

    def respond_to_missing?(*)
      false
    end

    def blocked_access_message
      @message || "Test blocked access to #{@key}."
    end
  end

  def initialize(name)
    super()
    @blocked = false
    @name = name
    @object_blockers = Set.new

    blocker = self

    define_method(:"block_#{name}") { blocker.block }
    define_method(:"unblock_#{name}") { blocker.unblock }

    define_method(:before_setup) do
      skip_blocker = respond_to?(:"skip_#{name}_block?") && public_send(:"skip_#{name}_block?")
      blocker.block unless skip_blocker
      super()
    end
    define_method(:after_teardown) do
      super()
    ensure
      blocker.unblock
    end
  end

  def inspect
    "#<TestAccessBlocker (#{@name})>"
  end

  def register(getter:, setter:, key:, message: nil)
    @object_blockers << ObjectBlocker.new(@name, getter.to_proc, setter.to_proc, key, message)
  end

  def blocked?
    @blocked
  end

  def block
    return if @blocked

    @object_blockers.each(&:block)
    @blocked = true
  end

  def unblock
    return unless @blocked

    @object_blockers.each(&:unblock)
    @blocked = false
  end
end
"##;

/// `TestMacros::GcsHelper`'s shape.
const GCS_HELPER: &str = r##"
module TestMacros
  module GcsHelper
    extend(ActiveSupport::Concern)

    @test_access_blocker = TestAccessBlocker.new("gcs") #: TestAccessBlocker

    class << self
      #: TestAccessBlocker
      attr_reader :test_access_blocker
    end

    @test_access_blocker.register(
      getter: BucketConfig.method(:bucket),
      setter: BucketConfig.method(:bucket=),
      key: "bucket",
      message: "GCS requires setup/teardown. To use it, add use_gcs to your test case.",
    )
    self #: untyped
      .include(@test_access_blocker)

    # Its reader and writer share one span.
    attr_accessor :original_buckets

    def reset_gcs_marker
      :reset
    end
  end
end
"##;

const GCS_TEST: &str = r##"
class BlockedGcsTest < ActiveSupport::TestCase
  include TestMacros::GcsHelper

  test "access is blocked" do
    error = assert_raises(Minitest::Assertion) { BucketConfig.bucket.upcase }
    assert_match(/GCS requires setup/, error.message)
    assert_equal "#<String>", BucketConfig.bucket.to_s
    assert_equal "\"#<String>\"", BucketConfig.bucket.inspect
    assert_equal String, BucketConfig.bucket.class
    assert TestMacros::GcsHelper.test_access_blocker.blocked?
    assert_equal :reset, reset_gcs_marker
  end
end

class UnblockedGcsTest < ActiveSupport::TestCase
  include TestMacros::GcsHelper

  setup do
    unblock_gcs
  end

  test "access is unblocked" do
    assert_equal "REAL-BUCKET", BucketConfig.bucket.upcase
    refute TestMacros::GcsHelper.test_access_blocker.blocked?
    assert_equal "#<TestAccessBlocker (gcs)>", TestMacros::GcsHelper.test_access_blocker.inspect
  end
end
"##;

const BLOCKER_TEST: &str = r##"
class TestAccessBlockerTest < ActiveSupport::TestCase
  test "an instance is a module that defines the blocker methods" do
    blocker = TestAccessBlocker.new("thing")
    assert_kind_of Module, blocker
    assert_equal "#<TestAccessBlocker (thing)>", blocker.inspect
    holder = Class.new { attr_accessor :thing }.new
    holder.thing = "value"
    blocker.register(getter: -> { holder.thing }, setter: ->(value) { holder.thing = value }, key: "thing")
    user = Class.new { include blocker }.new
    user.block_thing
    assert blocker.blocked?
    assert_raises(Minitest::Assertion) { holder.thing.upcase }
    user.unblock_thing
    assert_equal "VALUE", holder.thing.upcase
    assert_equal "rt", RestThing.new.inspect(1, 2)
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

/// `id => status` for every test Minitest runs natively in `classes`.
fn native_ids(sources: &[&str], classes: &[&str]) -> BTreeMap<String, String> {
    let mut oracle = format!(
        "gem \"minitest\", \"={}\"\nrequire \"minitest/test\"\nMinitest.seed = 4242\nrequire \"json\"\nrequire \"set\"\nrequire \"active_support\"\nrequire \"active_support/test_case\"\nrequire \"active_support/core_ext/module/delegation\"\n",
        minitest_version()
    );
    for source in sources {
        oracle.push_str(source);
    }
    oracle.push_str(&format!(
        r##"
[{}].each do |klass|
  klass.runnable_methods.each do |name|
    result = klass.new(name).run
    status = result.passed? ? "pass" : (result.skipped? ? "skip" : (result.error? ? "error" : "fail"))
    puts JSON.generate(id: "#{{klass.name}}##{{name}}", status: status, failures: result.failures.map(&:message))
  end
end
"##,
        classes.join(", ")
    ));
    let out = emit_and_run::ruby().args(["-e", &oracle]).output().expect("native Minitest");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    rows(&String::from_utf8_lossy(&out.stdout))
}

fn rows(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        let status = row["status"].as_str().unwrap().to_string();
        let status = if status == "pass" { status } else { format!("{status} {}", row) };
        assert!(map.insert(id.clone(), status).is_none(), "{id} reported twice");
    }
    map
}

/// Run every emitted test file in `test/roundhouse_tests.json`, one
/// process each; `id => status`.
fn emitted_ids(emitted: &std::path::Path) -> BTreeMap<String, String> {
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
    for file in files {
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), &file])
            .current_dir(emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!(
            "{file}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice");
        }
    }
    ran
}

#[test]
fn module_subclass_blocker_is_carried_and_blocks_until_unblocked() {
    let native = native_ids(
        &[BUCKET_CONFIG, REST_THING, TEST_ACCESS_BLOCKER, GCS_HELPER, GCS_TEST, BLOCKER_TEST],
        &["BlockedGcsTest", "UnblockedGcsTest", "TestAccessBlockerTest"],
    );
    assert_eq!(native.len(), 3, "native oracle: {native:#?}");
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", APP_CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("app/models/bucket_config.rb", BUCKET_CONFIG)
        .write("app/models/rest_thing.rb", REST_THING)
        .write("roundhouse.yml", "test_paths:\n  - test/support\n  - test/support/helpers\n  - test/models\n")
        .write(
            "test/support/test_access_blocker.rb",
            &format!("# frozen_string_literal: true\n{TEST_ACCESS_BLOCKER}"),
        )
        // The class's own test, beside it in the support directory.
        .write(
            "test/support/test_access_blocker_test.rb",
            &format!("# frozen_string_literal: true\n\nrequire \"test_helper\"\nrequire \"support/test_access_blocker\"\n{BLOCKER_TEST}"),
        )
        .write(
            "test/support/helpers/test_macros/gcs_helper.rb",
            &format!("# frozen_string_literal: true\n\nrequire \"support/test_access_blocker\"\n{GCS_HELPER}"),
        )
        .write(
            "test/models/gcs_test.rb",
            &format!("require \"test_helper\"\nrequire \"support/helpers/test_macros/gcs_helper\"\n{GCS_TEST}"),
        )
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    // A library class of the tree, not a test class of its file.
    let blocker = std::fs::read_to_string(emitted.join("app/models/test_access_blocker.rb")).expect("emitted blocker class");
    assert!(blocker.contains("class TestAccessBlocker < Module"), "{blocker}");
    // The helper's body statements, where the source has them.
    let helper = std::fs::read_to_string(emitted.join("app/models/test_macros/gcs_helper.rb")).expect("emitted helper");
    let register = helper.find("@test_access_blocker.register(").expect(&helper);
    let include = helper.find("self.include(@test_access_blocker)").expect(&helper);
    assert!(register < include && include < helper.find("def reset_gcs_marker").unwrap(), "{helper}");
    assert_eq!(emitted_ids(&emitted), native, "emitted results vs native");
}
