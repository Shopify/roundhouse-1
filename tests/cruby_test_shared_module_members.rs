//! A shared test module that keeps classes, modules and state of its own.
//!
//! Core's `Resiliency::TestHelper` declares `class FailingRedisDriver`
//! inside the module its tests include; `Merchandising::CatalogTestHelper`
//! declares `module Factories` with `class << self`, and through a chain
//! of includes reaches `QueryEngineFixtures`, whose module-level
//! `@loaded = false` and `class << self` hold state every including class
//! shares. The splice (`ingest::test_mixin`) refused every includer of
//! such a module ("a nested class or module the spliced tests would not
//! find").
//!
//! Now the module is a library module too: the support file is carried
//! with what the splice does not take (its nested classes and modules,
//! `class << self`, module-level instance variables and constants), each
//! includer includes it where the shared module stands, and the bodies
//! spliced out of a module read constants in that module's lexical scope
//! (`Receiver` in `Faults::TestHelper`'s `included do` block is
//! `Faults::Receiver`, even in a class whose namespace has a `Receiver`
//! of its own).
//!
//! The native oracle is Minitest 5.27 on Core's pinned ActiveSupport; the
//! emitted CRuby tree must list and run the same ids with the same
//! outcomes and assertion counts.
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

/// `Resiliency::TestHelper`'s shape: a sibling class the `included do`
/// block's setup builds by a bare name, a constant, and a nested class a
/// helper builds.
const FAULTS_HELPER: &str = r##"module Faults
  class Receiver
    def initialize
      @seen = []
    end

    def note(event)
      @seen << event
    end

    def seen
      @seen
    end
  end

  module TestHelper
    extend ActiveSupport::Concern

    PREFIX = "fault"

    included do
      setup do
        @receiver = Receiver.new
      end
    end

    def failing_driver
      FailingDriver.new(PREFIX)
    end

    class FailingDriver
      def initialize(prefix)
        @prefix = prefix
      end

      def call
        "#{@prefix}: simulated failure"
      end
    end
  end
end
"##;

/// `QueryEngineFixtures`' shape: a concern whose `included do` setup loads
/// fixtures once, the state kept in the module's own instance variables
/// and read through `class << self`. The class that ran first loads them;
/// the next one finds them loaded (`loaded_by`), as natively, where every
/// includer shares the one module.
const FIXTURES_HELPER: &str = r##"module Shop
  module FixturesHelper
    extend ActiveSupport::Concern

    included do
      setup do
        FixturesHelper.load_once!(self.class.name)
      end
    end

    @loads = 0
    @loaded_by = nil

    class << self
      def load_once!(by)
        return if loaded?

        @loads += 1
        @loaded_by = by
      end

      def loaded_by
        @loaded_by
      end

      def loaded?
        @loads > 0
      end

      def load_count
        @loads
      end
    end

    def fixtures_loaded
      FixturesHelper.load_count
    end
  end
end
"##;

/// `OptionsTestHelper`'s place in the chain: no members of its own, a
/// helper that reads a constant of its namespace by a bare name.
const OPTIONS_HELPER: &str = r##"module Shop
  COLORS = ["red", "blue"].freeze

  module OptionsHelper
    extend ActiveSupport::Concern
    include FixturesHelper

    def option_colors
      COLORS
    end
  end
end
"##;

/// `CatalogTestHelper`'s shape: a nested module with `class << self`,
/// read by a bare name from a helper, and the chain of includes.
const CATALOG_HELPER: &str = r##"module Shop
  module CatalogHelper
    extend ActiveSupport::Concern
    include OptionsHelper

    module Factories
      class << self
        def define(log)
          log << :catalog_factories
          log
        end
      end
    end

    def catalog_factories
      Factories.define([])
    end
  end
end
"##;

const TESTS: &str = r##"module Other
  COLORS = ["wrong"].freeze

  class Receiver
    def initialize
      raise "the includer's namespace has its own Receiver"
    end
  end

  class FaultsTest < ActiveSupport::TestCase
    include Faults::TestHelper

    test "a helper builds the module's nested class" do
      assert_equal "fault: simulated failure", failing_driver.call
      assert_kind_of Faults::TestHelper::FailingDriver, failing_driver
    end

    test "the included setup builds the module's sibling class" do
      @receiver.note(:called)
      assert_equal [:called], @receiver.seen
      assert_instance_of Faults::Receiver, @receiver
    end

    test "the includer finds the module's constants through its ancestry" do
      assert_equal "fault", PREFIX
      assert_same Faults::TestHelper::FailingDriver, FailingDriver
      assert_includes self.class.ancestors, Faults::TestHelper
    end
  end

  class CatalogATest < ActiveSupport::TestCase
    include Shop::CatalogHelper

    test "a helper calls the nested module's singleton method" do
      assert_equal [:catalog_factories], catalog_factories
      assert_equal [:catalog_factories], Shop::CatalogHelper::Factories.define([])
    end

    test "a chained module's helper reads its namespace's constant" do
      assert_equal ["red", "blue"], option_colors
    end

    test "fixtures load once for every including class" do
      assert_equal 1, fixtures_loaded
      assert Shop::FixturesHelper.loaded?
    end
  end
end

class CatalogBTest < ActiveSupport::TestCase
  include Shop::CatalogHelper

  test "fixtures load once for every including class" do
    assert_equal 1, fixtures_loaded
    assert_equal "Other::CatalogATest", Shop::FixturesHelper.loaded_by
  end
end
"##;

const SUPPORT: [(&str, &str); 4] = [
    ("faults_helper.rb", FAULTS_HELPER),
    ("fixtures_helper.rb", FIXTURES_HELPER),
    ("options_helper.rb", OPTIONS_HELPER),
    ("catalog_helper.rb", CATALOG_HELPER),
];

const CLASSES: [&str; 3] = ["Other::FaultsTest", "Other::CatalogATest", "CatalogBTest"];

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

/// `classes` run by native Minitest on Core's pinned ActiveSupport, the
/// `support` files loaded in order, then `tests`.
fn native(label: &str, support: &[(&str, &str)], tests: &str, classes: &[&str]) -> BTreeMap<String, (String, u64)> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_test_shared_module_members_native_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (path, text) in support.iter().copied().chain([("tests.rb", tests)]) {
        std::fs::write(dir.join(path), text).unwrap();
    }
    let files: Vec<&str> = support.iter().map(|(path, _)| *path).chain(["tests.rb"]).collect();
    let program = format!(
        r##"require {oracle:?}
RailsOracle.activate("activesupport", "activejob", minitest: "5.27.0")
require "minitest"
require "json"
Minitest.seed = 4242
require "active_support/all"
require "active_support/test_case"
require "active_job"
require "active_job/test_helper"
ActiveJob::Base.logger = Logger.new(nil)
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

/// The emitted CRuby tree's index rows for `classes` and their results,
/// run in one process in the order of `classes`, and the emitted sources.
fn emitted(support: &[(&str, &str)], tests: &str, classes: &[&str], native: &BTreeMap<String, (String, u64)>) -> String {
    let mut app = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/post.rb", "class Post < ApplicationRecord\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/models/shared_module_members_test.rb", tests);
    for (path, text) in support {
        app = app.write(&format!("test/support/{path}"), text);
    }
    let (emitted, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");

    let index: Value = serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index")).unwrap();
    let listed: Vec<&Value> = index["sources"].as_array().unwrap().iter().flat_map(|s| s["classes"].as_array().unwrap().iter()).collect();
    let mut indexed: Vec<String> = listed
        .iter()
        .filter(|c| classes.contains(&c["class"].as_str().unwrap_or_default()))
        .flat_map(|c| {
            let class = c["class"].as_str().unwrap().to_string();
            c["tests"].as_array().into_iter().flatten().map(move |t| format!("{class}#{}", t.as_str().unwrap()))
        })
        .collect();
    indexed.sort();
    assert_eq!(indexed, native.keys().cloned().collect::<Vec<_>>(), "index lists exactly the native ids:\n{index:#}");

    // The classes run in one process, in the native order, so the state
    // a module keeps is shared as natively.
    let mut files: Vec<String> = classes
        .iter()
        .map(|class| {
            listed
                .iter()
                .find(|c| c["class"] == *class)
                .and_then(|c| c["file"].as_str())
                .unwrap_or_else(|| panic!("{class} is not in the index"))
                .to_string()
        })
        .collect();
    files.dedup();
    let mut sources = String::new();
    for file in &files {
        sources.push_str(&std::fs::read_to_string(emitted.join(file)).expect("emitted test file"));
    }
    for (path, _) in support {
        if let Ok(text) = std::fs::read_to_string(emitted.join("test/support").join(path)) {
            sources.push_str(&format!("\n# test/support/{path}\n{text}"));
        }
        // A library module's own copy.
        if let Ok(text) = std::fs::read_to_string(emitted.join("app/models/shop").join(path)) {
            sources.push_str(&format!("\n# app/models/shop/{path}\n{text}"));
        }
    }
    let out_file = emitted.join("results.jsonl");
    let out = emit_and_run::ruby()
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; ARGV.each {{ |f| load f }}", minitest_version())])
        .args(&files)
        .current_dir(&emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &out_file)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("emitted CRuby");
    let log = format!("{sources}\nstdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{log}");
    let text = std::fs::read_to_string(&out_file).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
    assert_eq!(rows(&text), *native, "emitted results vs native\n{log}");
    sources
}

#[test]
fn a_shared_modules_own_classes_modules_and_state_run_as_natively() {
    let native = native("members", &SUPPORT, TESTS, &CLASSES);
    let expected: BTreeMap<String, (String, u64)> = [
        ("Other::FaultsTest#test_a_helper_builds_the_module's_nested_class", 2),
        ("Other::FaultsTest#test_the_included_setup_builds_the_module's_sibling_class", 2),
        ("Other::FaultsTest#test_the_includer_finds_the_module's_constants_through_its_ancestry", 4),
        ("Other::CatalogATest#test_a_helper_calls_the_nested_module's_singleton_method", 2),
        ("Other::CatalogATest#test_a_chained_module's_helper_reads_its_namespace's_constant", 1),
        ("Other::CatalogATest#test_fixtures_load_once_for_every_including_class", 2),
        ("CatalogBTest#test_fixtures_load_once_for_every_including_class", 2),
    ]
    .iter()
    .map(|(id, n)| (id.to_string(), ("pass".to_string(), *n)))
    .collect();
    assert_eq!(native, expected, "native oracle");
    let sources = emitted(&SUPPORT, TESTS, &CLASSES, &native);
    assert!(sources.contains("::Faults::Receiver.new"), "the block's bare `Receiver` is the module's lexical one:\n{sources}");
}

/// A library module that includes a plain module (natively in its own
/// ancestry, so its constants are the module's) and a concern (deferred
/// to each includer, so not in the module's ancestry), with helpers of
/// its own: the module keeps the plain include and its helpers, as
/// natively, and the includer still gets both setups.
const CONSTS_HELPER: &str = r##"module Shop
  module Consts
    DEPTH = 7
  end

  module DepConcern
    extend ActiveSupport::Concern

    included do
      setup do
        @dep = true
      end
    end
  end
end
"##;

const INC_HELPER: &str = r##"module Shop
  module IncHelper
    extend ActiveSupport::Concern
    include Consts
    include DepConcern

    def self.depth
      DEPTH
    end

    included do
      setup do
        @depth = IncHelper.depth
      end
    end

    def helper_depth
      DEPTH
    end

    private

    def hidden_depth
      DEPTH + 1
    end
  end
end
"##;

const INC_TESTS: &str = r##"class IncHelperTest < ActiveSupport::TestCase
  include Shop::IncHelper

  test "the module's constants come through the plain module it includes" do
    assert_equal 7, Shop::IncHelper::DEPTH
    assert_equal 7, Shop::IncHelper.const_get(:DEPTH)
  end

  test "the plain module is in the module's own ancestry and the concern is not" do
    assert Shop::IncHelper.include?(Shop::Consts)
    refute Shop::IncHelper.include?(Shop::DepConcern)
  end

  test "the module keeps its helpers" do
    assert_includes Shop::IncHelper.instance_methods, :helper_depth
    assert_includes Shop::IncHelper.private_instance_methods, :hidden_depth
    assert_includes Shop::IncHelper.instance_methods(false), :helper_depth
  end

  test "the includer runs both setups and reads the helpers" do
    assert_equal 7, @depth
    assert @dep
    assert_equal 7, helper_depth
    assert_equal 18, hidden_depth
  end

  def hidden_depth
    super + 10
  end
end
"##;

#[test]
fn a_library_modules_own_includes_and_helpers_run_as_natively() {
    let support = [("consts_helper.rb", CONSTS_HELPER), ("inc_helper.rb", INC_HELPER)];
    let classes = ["IncHelperTest"];
    let native = native("includes", &support, INC_TESTS, &classes);
    let expected: BTreeMap<String, (String, u64)> = [
        ("IncHelperTest#test_the_module's_constants_come_through_the_plain_module_it_includes", 2),
        ("IncHelperTest#test_the_plain_module_is_in_the_module's_own_ancestry_and_the_concern_is_not", 2),
        // `assert_includes` asserts `include?` is there, then the member.
        ("IncHelperTest#test_the_module_keeps_its_helpers", 6),
        ("IncHelperTest#test_the_includer_runs_both_setups_and_reads_the_helpers", 4),
    ]
    .iter()
    .map(|(id, n)| (id.to_string(), ("pass".to_string(), *n)))
    .collect();
    assert_eq!(native, expected, "native oracle");
    emitted(&support, INC_TESTS, &classes, &native);
}

/// A library module that includes the testing gems' modules (as Core's
/// `Merchandising::CatalogTestHelper` includes `ActiveJob::TestHelper`)
/// beside a plain module of its own, one in the same `include`. The
/// library file loads with the app tree, before the test runtime defines
/// those modules, so they are left to each includer, which natively has
/// them right after the module; the app's plain module stays in the
/// module's own ancestry. (`Minitest::Assertions` and most of
/// `ActiveSupport::Testing` are not modules of the emitted test runtime,
/// whose `TestBase` has their methods: a test class that includes one
/// itself does not load either.)
const GEM_HELPER: &str = r##"module Shop
  module GemHelper
    extend ActiveSupport::Concern
    include ActiveJob::TestHelper
    include ActiveSupport::Testing::TimeHelpers, Consts

    def self.depth
      DEPTH
    end

    included do
      setup do
        @depth = GemHelper.depth
      end
    end

    def counted
      n = 0
      assert_difference -> { n }, 2 do
        n += 2
      end
      n
    end
  end
end
"##;

const GEM_TESTS: &str = r##"class GemHelperTest < ActiveSupport::TestCase
  include Shop::GemHelper

  test "the module keeps its own plain include" do
    assert_equal 7, @depth
    assert_equal 7, Shop::GemHelper::DEPTH
    assert Shop::GemHelper.include?(Shop::Consts)
  end

  test "the includer has the gems' methods" do
    assert_equal 2, counted
    assert_no_enqueued_jobs do
      @depth += 1
    end
    travel_to Time.utc(2020, 1, 2) do
      assert_equal 2020, Time.now.year
    end
  end

  test "the module comes before the gems' modules in the includer" do
    ancestors = self.class.ancestors
    assert ancestors.index(Shop::GemHelper) < ancestors.index(ActiveJob::TestHelper)
    assert_includes ancestors, Shop::Consts
  end
end

class UnrelatedGemTest < ActiveSupport::TestCase
  test "a class beside it loads" do
    assert_equal 2, 1 + 1
  end
end
"##;

#[test]
fn a_library_modules_includes_of_the_testing_gems_go_to_its_includers() {
    let support = [("consts_helper.rb", CONSTS_HELPER), ("gem_helper.rb", GEM_HELPER)];
    let classes = ["GemHelperTest", "UnrelatedGemTest"];
    let native = native("gems", &support, GEM_TESTS, &classes);
    let expected: BTreeMap<String, (String, u64)> = [
        ("GemHelperTest#test_the_module_keeps_its_own_plain_include", 3),
        // `assert_difference` and `assert_no_enqueued_jobs` count two each.
        ("GemHelperTest#test_the_includer_has_the_gems'_methods", 6),
        ("GemHelperTest#test_the_module_comes_before_the_gems'_modules_in_the_includer", 3),
        ("UnrelatedGemTest#test_a_class_beside_it_loads", 1),
    ]
    .iter()
    .map(|(id, n)| (id.to_string(), ("pass".to_string(), *n)))
    .collect();
    assert_eq!(native, expected, "native oracle");
    let sources = emitted(&support, GEM_TESTS, &classes, &native);
    let library = sources.split("# app/models/shop/gem_helper.rb").nth(1).expect("the module's library copy");
    assert!(library.contains("include Shop::Consts") && !library.contains("ActiveJob") && !library.contains("TimeHelpers"), "{library}");
}

/// A helper that calls `super` is spliced into each includer only, and a
/// `private :label` naming it goes with it; the module keeps the rest.
const LABEL_HELPER: &str = r##"module Shop
  module LabelHelper
    extend ActiveSupport::Concern

    PREFIX = "lbl"

    def self.prefix
      PREFIX
    end

    included do
      setup do
        @prefix = LabelHelper.prefix
      end
    end

    def label
      "#{PREFIX}-#{super}"
    end

    def shown
      label
    end
    private :label, :shown

    def visible
      shown
    end
  end
end
"##;

const LABEL_TESTS: &str = r##"class LabelBaseTest < ActiveSupport::TestCase
  def label
    "base"
  end
end

class LabelTest < LabelBaseTest
  include Shop::LabelHelper

  test "a private helper calls super into the superclass" do
    assert_equal "lbl-base", visible
    assert_equal "lbl-base", send(:label)
    refute_respond_to self, :label
    refute_respond_to self, :shown
    assert_equal "lbl", @prefix
  end

  test "the module keeps the visibility of the helpers it defines" do
    assert Shop::LabelHelper.private_method_defined?(:shown)
    assert Shop::LabelHelper.public_method_defined?(:visible)
  end
end
"##;

#[test]
fn a_visibility_change_naming_a_spliced_super_helper_goes_with_it() {
    let support = [("label_helper.rb", LABEL_HELPER)];
    let classes = ["LabelTest"];
    let native = native("label", &support, LABEL_TESTS, &classes);
    let expected: BTreeMap<String, (String, u64)> =
        [("LabelTest#test_a_private_helper_calls_super_into_the_superclass", 5), ("LabelTest#test_the_module_keeps_the_visibility_of_the_helpers_it_defines", 2)]
            .iter()
            .map(|(id, n)| (id.to_string(), ("pass".to_string(), *n)))
            .collect();
    assert_eq!(native, expected, "native oracle");
    let sources = emitted(&support, LABEL_TESTS, &classes, &native);
    // The reflection keeps its helpers in the library body: `label`, which
    // calls `super`, is left to the includer, and `private` keeps `:shown`.
    let library = sources.split("# app/models/shop/label_helper.rb").nth(1).expect("the module's library copy");
    assert!(library.contains("def shown") && library.contains("def visible") && !library.contains("def label"), "{library}");
    assert!(library.lines().any(|l| l.trim() == "private :shown"), "{library}");
}
