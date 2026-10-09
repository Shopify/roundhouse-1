//! Shared test modules: a module that declares tests, included by test
//! classes that run them. Core's `SharedTests` concern (its
//! `ClassMethods#included` hook wraps each test in `<prefix>_setup` /
//! `<prefix>_teardown`) and `TestTags` are loaded verbatim by the native
//! oracle; the emitted CRuby tree must list and run the same
//! `IncluderClass#test_name` ids with the same outcomes.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;

/// Core's `components/platform/essentials/test/support/shared_tests.rb`,
/// verbatim but for its `require`s (the oracle loads the same libraries).
const SHARED_TESTS: &str = r##"
# @requires_ancestor: ActiveRecordTestHelper
module SharedTests
  extend ActiveSupport::Concern
  include Minitest::Assertions

  def class_name
    self.class.name.to_s.gsub(/Test$/, "")
  end

  def load_fixture(fixture)
    fixture_names = class_name.underscore.tr("/", "_").pluralize
    send(fixture_names, fixture)
  end

  # @requires_ancestor: Module
  module ClassMethods
    include ActiveSupport::Testing::Declarative
    include ::TestTags

    def setup(*args, &block)
      return super if is_a?(Class)

      define_method("#{method_prefix}_setup", &block)
    end

    def teardown(*args, &block)
      return super if is_a?(Class)

      define_method("#{method_prefix}_teardown", &block)
    end

    def method_prefix
      @method_prefix ||= name #: as !nil
        .gsub(/SharedTests$/, "").underscore.tr("/", "_").to_s
    end

    def generate_tests(&block)
      @test_generator = block
    end

    def included(test_case)
      locations = caller_locations(1, 1) #: as !nil
      location = locations.first #: as !nil

      methods = instance_methods

      if @test_generator
        mod = Module.new
        mod.extend(ActiveSupport::Testing::Declarative)
        mod.class_exec(test_case, &@test_generator)
        test_case.include(mod)
        methods += mod.instance_methods
      end

      setup = "#{method_prefix}_setup"
      teardown = "#{method_prefix}_teardown"

      methods.each do |name|
        next unless name.to_s.start_with?("test_")

        test_case.instance_eval(<<-EOS, location.path, location.lineno)
          define_method(#{name.inspect}) do
            begin
              #{setup} if respond_to?(:#{setup})
              super()
            ensure
              #{teardown} if respond_to?(:#{teardown})
            end
          end
        EOS
      end
    end
  end
end
"##;

/// Core's `components/platform/essentials/lib/platform/testing/test_tags.rb`,
/// verbatim but for its `require` (oracle only: the emitted tree defines
/// each test under its final name).
const TEST_TAGS: &str = r##"
module TestTags
  METHOD_DEFINED = Module.instance_method(:method_defined?)
  private_constant :METHOD_DEFINED

  def tag(tags, &block)
    before_tags = test_tags.dup
    test_tags.merge!(tags)

    unless block
      _install_persistent_tags_hook
      return
    end

    begin
      block.call
    ensure
      test_tags.clear
      test_tags.merge!(before_tags) # Restore previous tags
    end
  end

  def test(name, tags = nil, &block)
    all_tags = tags ? test_tags.merge(tags) : test_tags
    name = TestTags.generate_name_with_tags(name, all_tags)
    name_symbol = name.to_sym
    return name_symbol if METHOD_DEFINED.bind_call(self, name_symbol, false)
    super(name, &block)
  end

  class << self
    def generate_name_with_tags(name, tags)
      return name if tags.nil? || tags.empty?
      tag_string = tags.map { |*pair| pair.join(":").prepend("tag:") }.join("_")
      tag_string.empty? ? name : "#{name}_#{tag_string}"
    end
  end

  private

  def test_tags
    @test_tags ||= {}
  end

  def _install_persistent_tags_hook
    return if @_persistent_tags_hook_installed
    @_persistent_tags_hook_installed = true

    singleton_class.prepend(Module.new do
      def runnable_methods
        _process_persistent_tags
        super
      end

      def unordered_runnable_methods
        _process_persistent_tags
        super
      end
    end)
  end

  def _process_persistent_tags
    return if @_persistent_tags_processed
    @_persistent_tags_processed = true

    tags = test_tags
    return if tags.empty?

    instance_methods(false).grep(/^test_/).each do |method_name|
      next if method_name.to_s.include?("tag:")

      tagged_name = TestTags.generate_name_with_tags(method_name.to_s, tags).to_sym
      alias_method(tagged_name, method_name)
      undef_method(method_name)
    end
  end
end

ActiveSupport.on_load(:active_support_test_case) do
  class << self
    prepend TestTags
  end
end
"##;

/// Setup and teardown wrap each test (after the class's own setup
/// callbacks); tags inside the module; a constant read both ways; `def`
/// tests; a test the includer overrides; a private `attr_reader`.
const GREETING: &str = r##"
module GreetingSharedTests
  include SharedTests

  GREETING = "hello"

  setup do
    @log << :greeting_setup
  end

  teardown do
    @log << :greeting_teardown
  end

  test "greets" do
    assert_equal [:class_setup, :greeting_setup], log
    assert_equal "hello", greeting
  end

  test "reads its constant qualified" do
    assert_equal "hello!", GreetingSharedTests::GREETING + "!"
  end

  tag slow: true do
    test "tagged in a block" do
      assert_equal :greeting_setup, @log.last
    end
  end

  test "tagged inline", owner: "x" do
    assert_equal :greeting_setup, @log.last
  end

  test "overridden by the class" do
    assert_equal :greeting_setup, @log.last
  end

  def test_def_in_module
    assert_equal :greeting_setup, @log.last
  end

  def test_shadowed_by_the_wrapper
    assert_equal :greeting_setup, @log.last
  end

  def greeting
    GREETING
  end

  private

  attr_reader :log
end
"##;

/// A module including another: the inner wrapper runs inside the outer
/// one, and a helper's `super` reaches the inner module's definition.
const LAYERED: &str = r##"
module BaseSharedTests
  include SharedTests

  setup do
    @log << :base_setup
  end

  test "base runs inside both wrappers" do
    assert_equal [:class_setup, :extended_setup, :base_setup], @log
    assert_equal "extended+base", shout
  end

  def shout
    "base"
  end
end

module ExtendedSharedTests
  include SharedTests
  include BaseSharedTests

  setup do
    @log << :extended_setup
  end

  test "extended runs" do
    assert_equal [:class_setup, :extended_setup], @log
  end

  def shout
    "extended+" + super
  end
end
"##;

/// `include Left, Right`: Left is first in the lookup, and its wrapper is
/// the one `included` defines last.
const PAIR: &str = r##"
module LeftSharedTests
  include SharedTests

  test "collides" do
    assert_nil @right
    assert_equal :left, side
  end

  def side
    :left
  end
end

module RightSharedTests
  include SharedTests

  setup do
    @right = true
  end

  test "collides" do
    flunk "LeftSharedTests is first in the lookup"
  end

  test "only on the right" do
    assert_equal true, @right
    assert_equal :left, side
  end

  def side
    :right
  end
end
"##;

/// A plain Declarative module (no wrapper, no tags), and a concern whose
/// `included do` block tags the class and declares a test on it.
const CONCERNS: &str = r##"
module PlainTests
  extend ActiveSupport::Testing::Declarative

  test "plain declared" do
    assert_equal :plain, plain_helper
  end

  def plain_helper
    :plain
  end
end

module BillingConcern
  extend ActiveSupport::Concern

  included do
    tag area: "billing"

    test "from the included block" do
      assert_equal [:class_setup], @log.first(1)
    end
  end
end
"##;

const GREETER_TEST: &str = r##"
class GreeterTest < ActiveSupport::TestCase
  def test_shadowed_by_the_wrapper
    flunk "the wrapper `included` defines replaces this def"
  end

  include GreetingSharedTests

  setup do
    @log = [:class_setup]
  end

  teardown do
    assert_equal :greeting_teardown, @log.last if @log.include?(:greeting_setup)
  end

  def test_overridden_by_the_class
    assert_equal [:class_setup], @log
  end

  test "names its class" do
    assert_equal "Greeter", class_name
  end
end
"##;

const LAYERED_TEST: &str = r##"
class LayeredTest < ActiveSupport::TestCase
  setup do
    @log = [:class_setup]
  end

  include ExtendedSharedTests
end
"##;

const PAIR_TEST: &str = r##"
class PairTest < ActiveSupport::TestCase
  include LeftSharedTests, RightSharedTests
end
"##;

const TAGGED_TEST: &str = r##"
class TaggedTest < ActiveSupport::TestCase
  setup do
    @log = [:class_setup]
  end

  test "before the concern" do
    assert true
  end

  include PlainTests
  include GreetingSharedTests
  include BillingConcern

  test "own" do
    assert_equal :plain, plain_helper
  end

  def test_own_def
    assert true
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
        "gem \"minitest\", \"={}\"\nrequire \"minitest/test\"\nMinitest.seed = 4242\nrequire \"json\"\nrequire \"active_support\"\nrequire \"active_support/test_case\"\n{TEST_TAGS}\n",
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
    puts JSON.generate(id: "#{{klass.name}}##{{name}}", status: status)
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
        assert!(map.insert(id.clone(), row["status"].as_str().unwrap().to_string()).is_none(), "{id} reported twice");
    }
    map
}

/// class => (emitted file, tests) from `test/roundhouse_tests.json`.
fn index(dir: &std::path::Path) -> BTreeMap<String, (String, Vec<String>)> {
    let json: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("test/roundhouse_tests.json")).expect("test index")).unwrap();
    let mut classes = BTreeMap::new();
    for source in json["sources"].as_array().unwrap() {
        for c in source["classes"].as_array().unwrap() {
            let tests = c["tests"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
            classes.insert(c["class"].as_str().unwrap().to_string(), (c["file"].as_str().unwrap().to_string(), tests));
        }
    }
    classes
}

#[test]
fn included_shared_tests_run_under_the_includers_native_ids() {
    let classes = ["GreeterTest", "LayeredTest", "PairTest", "TaggedTest"];
    let native = native_ids(
        &[SHARED_TESTS, GREETING, LAYERED, PAIR, CONCERNS, GREETER_TEST, LAYERED_TEST, PAIR_TEST, TAGGED_TEST],
        &classes,
    );
    // The oracle is the reference; the literal list pins what it means.
    let expected = [
        "GreeterTest#test_def_in_module",
        "GreeterTest#test_greets",
        "GreeterTest#test_names_its_class",
        "GreeterTest#test_overridden_by_the_class",
        "GreeterTest#test_reads_its_constant_qualified",
        "GreeterTest#test_shadowed_by_the_wrapper",
        "GreeterTest#test_tagged_in_a_block_tag:slow:true",
        "GreeterTest#test_tagged_inline_tag:owner:x",
        "LayeredTest#test_base_runs_inside_both_wrappers",
        "LayeredTest#test_extended_runs",
        "PairTest#test_collides",
        "PairTest#test_only_on_the_right",
        "TaggedTest#test_before_the_concern_tag:area:billing",
        "TaggedTest#test_def_in_module_tag:area:billing",
        "TaggedTest#test_from_the_included_block_tag:area:billing",
        "TaggedTest#test_greets_tag:area:billing",
        "TaggedTest#test_overridden_by_the_class_tag:area:billing",
        "TaggedTest#test_own_def_tag:area:billing",
        "TaggedTest#test_own_tag:area:billing",
        "TaggedTest#test_plain_declared",
        "TaggedTest#test_reads_its_constant_qualified_tag:area:billing",
        "TaggedTest#test_shadowed_by_the_wrapper_tag:area:billing",
        "TaggedTest#test_tagged_in_a_block_tag:slow:true",
        "TaggedTest#test_tagged_inline_tag:owner:x",
    ];
    let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(native.keys().cloned().collect::<Vec<_>>(), want, "native oracle");
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        // As the Core slice lists the directories of required test files.
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/support/shared_tests.rb", SHARED_TESTS)
        .write("test/support/greeting_shared_tests.rb", GREETING)
        .write("test/support/layered_shared_tests.rb", LAYERED)
        .write("test/support/pair_shared_tests.rb", PAIR)
        .write("test/support/concerns.rb", CONCERNS)
        .write("test/models/greeter_test.rb", GREETER_TEST)
        .write("test/models/layered_test.rb", LAYERED_TEST)
        .write("test/models/pair_test.rb", PAIR_TEST)
        .write("test/models/tagged_test.rb", TAGGED_TEST)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");

    // The index lists exactly the native ids.
    let classes_index = index(&emitted);
    let mut indexed: Vec<String> = classes_index
        .iter()
        .flat_map(|(class, (_, tests))| tests.iter().map(move |t| format!("{class}#{t}")))
        .collect();
    indexed.sort();
    assert_eq!(indexed, want, "index");

    // Running the emitted files reports the same ids and outcomes.
    let mut ran = BTreeMap::new();
    for (file, _) in classes_index.values() {
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), file])
            .current_dir(&emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!("{file}\nstdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{log}");
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice");
        }
    }
    assert_eq!(ran, native, "emitted results vs native");
}
