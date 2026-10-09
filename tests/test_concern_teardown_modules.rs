//! The modules around a spliced class's teardown and a concern's
//! `included do` block, compared with native Minitest.
//!
//! `ingest::test_mixin` rebuilds the `teardown` of a class whose concern
//! registers teardown callbacks, and Rails runs those callbacks after the
//! whole `def teardown` chain. A module a subclass includes sits in that
//! chain before the base's teardown, so:
//!
//! - one whose `def teardown` ends in `super` keeps the callbacks last;
//! - one whose def runs code after `super` (a shared concern's or a
//!   plain module's), or one no test file declares, is refused by name
//!   (a named survey gap, not a listed class).
//!
//! The splice also has to place and keep what the concern brings:
//!
//! - a plain module included after a shared one comes first in the
//!   lookup, so its `def teardown` runs its `super` before the shared one;
//! - an `included do` block's `def self.label` leaves the class's own
//!   instance `def label`;
//! - `setup :name, …` in an `included do` block registers callbacks that
//!   call the methods, and a bare `private` there hides the defs after it;
//! - a statement of the block the splice does not model (`cattr_accessor`,
//!   `setup(prepend: true) do`) refuses the class by name.
//!
//! The native oracle is Core's pinned activesupport; the emitted CRuby
//! tree must list and run the same ids with the same outcomes and
//! assertion counts, less the refused classes.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::ingest::survey;
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

/// The callbacks the subclasses' modules must leave last.
const CB_LOG: &str = r##"module CbLog
  extend ActiveSupport::Concern

  included do
    setup do
      @log = [:setup]
    end

    teardown do
      @log << :cb
      assert_equal @expected, @log if @expected
    end
  end
end
"##;

const LOG_SETUP: &str = r##"module LogSetup
  extend ActiveSupport::Concern

  included do
    setup do
      @log = [:setup]
    end
  end
end
"##;

/// Its `def teardown` runs code after `super`.
const SHARED_DEF: &str = r##"module SharedDefTeardown
  extend ActiveSupport::Concern

  included do
    setup do
      @log << :shared_setup
    end
  end

  #: -> void
  def teardown
    super
    @log << :shared_def
  end
end
"##;

/// Its `def teardown` ends in `super`.
const GOOD_DEF: &str = r##"module GoodDefTeardown
  extend ActiveSupport::Concern

  included do
    setup do
      @log << :good_setup
    end
  end

  #: -> void
  def teardown
    @log << :good_def
    super
  end
end
"##;

const SELF_LABEL: &str = r##"module SelfLabel
  extend ActiveSupport::Concern

  included do
    setup do
      @labelled = true
    end

    def self.label
      "class_method"
    end
  end
end
"##;

const SYM_SETUP: &str = r##"module SymSetup
  extend ActiveSupport::Concern

  included do
    extend ActiveSupport::Testing::Declarative

    setup :first_step, :second_step
    setup(:third_step)

    test "from the block" do
      assert_equal [:first, :second, :third], @steps
      assert_raises(NoMethodError) { public_send(:hidden_helper) }
      assert_equal :hidden, hidden_helper
      assert_raises(NoMethodError) { public_send(:other_hidden) }
      assert_equal :shown, shown_helper
    end

    private

    def hidden_helper
      :hidden
    end

    public def shown_helper
      :shown
    end

    private def other_hidden
      :other
    end
  end

  def first_step
    @steps = [:first]
  end

  def second_step
    @steps << :second
  end

  def third_step
    @steps << :third
  end
end
"##;

const ACCESSOR_BLOCK: &str = r##"module AccessorBlock
  extend ActiveSupport::Concern

  included do
    cattr_accessor :flavor, default: :plain

    setup do
      @flavor_seen = flavor
    end
  end
end
"##;

const PREPEND_SETUP: &str = r##"module PrependSetup
  extend ActiveSupport::Concern

  included do
    setup(prepend: true) do
      (@order ||= []) << :prepended
    end
  end
end
"##;

/// Not in a test file: the scan cannot read its `def teardown`.
const PLAIN_CLEANUP: &str = r##"module PlainCleanup
  #: -> void
  def teardown
    super
  ensure
    @log << :plain
  end
end
"##;

const PLAIN_DEF: &str = r##"module PlainDefTeardown
  #: -> void
  def teardown
    super
    @log << :plain_def
  end
end
"##;

const TESTS: &str = r##"class BaseCbTest < ActiveSupport::TestCase
  include CbLog
  include ActiveSupport::Testing::TimeHelpers

  test "base" do
    @log << :test
    assert true
  end
end

class SubGoodTest < BaseCbTest
  include GoodDefTeardown
  include ActiveSupport::Testing::TimeHelpers

  test "a module def teardown ending in super leaves the callbacks last" do
    @log << :test
    @expected = [:setup, :good_setup, :test, :good_def, :cb]
    assert true
  end
end

class SubPlainTest < BaseCbTest
  include PlainCleanup

  test "sub plain" do
    @log << :test
    @expected = [:setup, :test, :plain, :cb]
    assert true
  end
end

class SubSharedTest < BaseCbTest
  include SharedDefTeardown

  test "sub shared" do
    @log << :test
    @expected = [:setup, :shared_setup, :test, :shared_def, :cb]
    assert true
  end
end

class SharedThenPlainTest < ActiveSupport::TestCase
  include LogSetup
  include SharedDefTeardown
  include PlainDefTeardown

  teardown do
    assert_equal [:setup, :shared_setup, :test, :shared_def, :plain_def], @log
  end

  test "a plain module included after a shared one comes first" do
    @log << :test
    assert true
  end
end

class SelfLabelTest < ActiveSupport::TestCase
  def label
    "instance"
  end

  include SelfLabel

  test "an instance def survives a block class method of its name" do
    assert_equal "instance", label
  end
end

class SymSetupTest < ActiveSupport::TestCase
  include SymSetup

  test "own" do
    assert_equal [:first, :second, :third], @steps
  end
end

class AccessorBlockTest < ActiveSupport::TestCase
  include AccessorBlock

  test "flavor" do
    assert_equal :plain, @flavor_seen
  end
end

class PrependSetupTest < ActiveSupport::TestCase
  setup do
    (@order ||= []) << :own
  end

  include PrependSetup

  test "order" do
    assert_equal [:prepended, :own], @order
  end
end
"##;

const SUPPORT: [(&str, &str); 8] = [
    ("cb_log.rb", CB_LOG),
    ("log_setup.rb", LOG_SETUP),
    ("shared_def_teardown.rb", SHARED_DEF),
    ("good_def_teardown.rb", GOOD_DEF),
    ("self_label.rb", SELF_LABEL),
    ("sym_setup.rb", SYM_SETUP),
    ("accessor_block.rb", ACCESSOR_BLOCK),
    ("prepend_setup.rb", PREPEND_SETUP),
];

const LIB: [(&str, &str); 2] = [("plain_cleanup.rb", PLAIN_CLEANUP), ("plain_def_teardown.rb", PLAIN_DEF)];

const CLASSES: [&str; 9] = [
    "BaseCbTest",
    "SubGoodTest",
    "SubPlainTest",
    "SubSharedTest",
    "SharedThenPlainTest",
    "SelfLabelTest",
    "SymSetupTest",
    "AccessorBlockTest",
    "PrependSetupTest",
];

/// Refused by name, with the reason the gap gives.
const REFUSED: [(&str, &str); 4] = [
    ("SubPlainTest", "the module `PlainCleanup`, which it includes, is not one a test file declares"),
    ("SubSharedTest", "the `def teardown` of the module `SharedDefTeardown`, which it includes, does not end by calling `super`"),
    ("AccessorBlockTest", "runs `cattr_accessor :flavor, default: :plain` on the class"),
    ("PrependSetupTest", "passes arguments or a block argument (`prepend: true`"),
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
    let dir = std::env::temp_dir().join(format!("rh_test_concern_teardown_modules_native_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut files: Vec<&str> = Vec::new();
    for (path, text) in SUPPORT.iter().chain(LIB.iter()).copied().chain([("tests.rb", TESTS)]) {
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
require "active_support/testing/time_helpers"
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
fn modules_around_a_concern_teardown_run_as_rails_runs_them_or_are_refused_by_name() {
    let native = native();
    let outcome = |status: &str, n: u64| (status.to_string(), n);
    let expected: BTreeMap<String, (String, u64)> = [
        ("BaseCbTest#test_base", outcome("pass", 1)),
        ("SubGoodTest#test_base", outcome("pass", 1)),
        ("SubGoodTest#test_a_module_def_teardown_ending_in_super_leaves_the_callbacks_last", outcome("pass", 2)),
        ("SubPlainTest#test_base", outcome("pass", 1)),
        ("SubPlainTest#test_sub_plain", outcome("pass", 2)),
        ("SubSharedTest#test_base", outcome("pass", 1)),
        ("SubSharedTest#test_sub_shared", outcome("pass", 2)),
        ("SharedThenPlainTest#test_a_plain_module_included_after_a_shared_one_comes_first", outcome("pass", 2)),
        ("SelfLabelTest#test_an_instance_def_survives_a_block_class_method_of_its_name", outcome("pass", 1)),
        ("SymSetupTest#test_from_the_block", outcome("pass", 5)),
        ("SymSetupTest#test_own", outcome("pass", 1)),
        ("AccessorBlockTest#test_flavor", outcome("pass", 1)),
        ("PrependSetupTest#test_order", outcome("pass", 1)),
    ]
    .into_iter()
    .map(|(id, o)| (id.to_string(), o))
    .collect();
    assert_eq!(native, expected, "native oracle");

    let mut app = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/post.rb", "class Post < ApplicationRecord\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/models/teardown_modules_test.rb", TESTS);
    for (path, text) in SUPPORT {
        app = app.write(&format!("test/support/{path}"), text);
    }
    for (path, text) in LIB {
        app = app.write(&format!("lib/{path}"), text);
    }
    // Survey ingest: each refused class is a named gap, the rest emit.
    survey::activate();
    let (emitted, errors) = app.emit(BuildTarget::Ruby);
    let gaps: Vec<String> = survey::drain().iter().map(|g| g.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    for (class, why) in REFUSED {
        let refusal = format!("test class `{class}` refused");
        assert!(gaps.iter().any(|g| g.contains(&refusal) && g.contains(why)), "no gap names {class} ({why}):\n{gaps:#?}");
    }

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
    let refused = |id: &str| REFUSED.iter().any(|(class, _)| id.starts_with(&format!("{class}#")));
    let listed: BTreeMap<String, (String, u64)> = native.into_iter().filter(|(id, _)| !refused(id)).collect();
    assert_eq!(indexed, listed.keys().cloned().collect::<Vec<_>>(), "index lists exactly the native ids of the classes not refused:\n{index:#}");

    let mut results = BTreeMap::new();
    for class in CLASSES.iter().filter(|c| !REFUSED.iter().any(|(r, _)| r == *c)) {
        let file = classes
            .iter()
            .find(|c| c["class"] == *class)
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
    assert_eq!(results, listed, "emitted results vs native");
}
