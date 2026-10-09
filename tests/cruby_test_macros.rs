//! Tests a class body declares through a call it makes, expanded at
//! compile time, and loops over the app's constants, unrolled:
//!
//! - a test macro (a class method whose body is `generate_test` / `test`
//!   calls), defined in the test class's `class << self` or in an
//!   `ActiveSupport::Concern`'s `module ClassMethods` the class includes,
//!   declares at each call the tests Ruby declares, under the names Ruby
//!   builds (`to_sentence`, interpolation), with the parameters bound in
//!   each test body;
//! - a class-body loop over a collection computed from app constants
//!   (`HELPERS.keys - (SEEDS + [X])`, `Module.constants`, a constant
//!   Array of Hashes naming modules) declares one test per element;
//! - a macro or loop that cannot be evaluated before it runs is a named
//!   survey gap, and the class's other tests still run.
//!
//! The native oracle is Core's TestGenerator and ActiveSupport's `test`
//! under Minitest; ids, outcomes and assertion counts must match.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

const STAGED_CHANGE: &str = r##"module StagedChange
  ADD = "add"
  REMOVE = "remove"
  TIP = "tip"
  HELPERS = { ADD => :adder, REMOVE => :remover, TIP => :tipper }.freeze
  SEEDS = [ADD].freeze
end
"##;

const REASONS: &str = r##"module Reasons
  ALPHA = :alpha
  BETA = :beta

  class << self
    def all
      [ALPHA, BETA]
    end
  end
end
"##;

/// Core's `platform/testing/test_generator.rb` (`test(name, tags)`, an
/// alias of the method it defines, and a wrapper that `send`s to it),
/// with `define_method` for its `class_eval` string and no fallback
/// location: the emitted program compiles this file, and Core's
/// `class_eval` / `alias_method` on an unproven receiver are refused
/// at compile time (a separate gap, not this test's).
const TEST_GENERATOR: &str = r##"module TestGenerator
  def generate_test(name, location = nil, tags: nil, &block)
    method_name = test(name, tags, &block)
    real_method_name = "real_#{method_name}"
    alias_method(real_method_name.to_sym, method_name)
    private(real_method_name)
    define_method(method_name) { send(real_method_name) }
    method_name
  end
end
"##;

/// The shape of Core's `AccessAndAuth::InputFieldsAuthorizationHelper`.
const PERMISSION_HELPER: &str = r##"module Access
  module PermissionHelper
    extend ActiveSupport::Concern

    module ClassMethods
      include TestGenerator

      def test_permission(subject, key, sufficient:, insufficient: [])
        location = caller_locations.find { |location| location.to_s !~ /sorbet/ }

        generate_test(
          "#{key} is allowed " \
            "for #{sufficient.to_sentence}",
          location
        ) do
          assert_equal subject, Reasons
          assert_operator sufficient.size, :>, insufficient.size
        end

        generate_test("#{key} is refused for #{insufficient.to_sentence}", location) do
          assert_equal :beta, key
        end
      end
    end
  end
end
"##;

const MACRO_TEST: &str = r##"class MacroTest < ActiveSupport::TestCase
  extend TestGenerator
  include Access::PermissionHelper

  class << self
    def generate_checks(key, perms:, extra: nil)
      location = caller_locations.find { |l| l.to_s !~ /sorbet/ }

      generate_test(
        "check #{key} " \
          "with #{perms.to_sentence}",
        location,
      ) do
        assert_kind_of Symbol, key
        assert_nil extra
      end

      generate_test("check #{key} fails when #{perms.size} is two", location) do
        refute_equal 2, perms.size
      end

      test "tagged #{key}", slow: true do
        assert_includes [:alpha, :beta, :gamma], key
      end
    end

    def generate_each(*names)
      names.each { |n| test("each #{n}") { assert n } }
    end
  end

  test "plain" do
    assert true
  end

  generate_checks(:alpha, perms: [:read])
  generate_checks(:beta, perms: [:read, :write])
  generate_checks(:gamma, perms: [:read, :write, :admin], extra: nil)
  generate_each(:x, :y)

  test_permission(Reasons, :alpha, sufficient: [:products, :manage_products, :edit_price], insufficient: [:products])
  test_permission(Reasons, :beta, sufficient: [:products, :manage_products])
end
"##;

const CONST_LOOP_TEST: &str = r##"class ConstLoopTest < ActiveSupport::TestCase
  REFERENCING = StagedChange::HELPERS.keys - (StagedChange::SEEDS + [StagedChange::REMOVE])
  private_constant :REFERENCING

  StagedChange::SEEDS.each do |seed|
    REFERENCING.each do |other|
      test "#{other} unrelated to #{seed}" do
        assert_equal "tip", other
        assert_includes StagedChange::SEEDS, seed
      end
    end
  end

  Reasons.constants.each do |reason|
    test "#{reason} is a reason" do
      assert_equal Reasons.const_get(reason), reason.to_s.downcase.to_sym
    end
  end

  [StagedChange::ADD, StagedChange::TIP].each do |kind|
    label = kind == StagedChange::ADD ? "Adding" : "Tipping"
    test "label #{label}" do
      assert_equal(kind == "add" ? "Adding" : "Tipping", label)
    end
  end

  CASES = [{ name: "first", mod: StagedChange }, { name: "second", mod: Reasons }].freeze
  CASES.each do |c|
    test "case #{c[:name]}" do
      assert_kind_of Module, c[:mod]
      assert_same CASES.find { |x| x[:name] == c[:name] }[:mod], c[:mod]
    end
  end

  StagedChange::HELPERS.each do |key, helper|
    test "helper #{key}" do
      assert_equal key, helper.to_s.delete_suffix("per").delete_suffix("der").delete_suffix("ver")
    end
  end

  Reasons.all.each do |r|
    test "computed #{r}" do
      assert r
    end
  end
end
"##;

/// The tests the gaps name: native Minitest runs them, the emitted
/// program does not.
const NOT_MODELED: [&str; 4] =
    ["ConstLoopTest#test_computed_alpha", "ConstLoopTest#test_computed_beta", "MacroTest#test_each_x", "MacroTest#test_each_y"];

/// Core's TestTags naming (`_tag:<key>:<value>` per tag) for `test`'s
/// tags Hash, prepended on ActiveSupport::TestCase's singleton.
const ORACLE_TAGS: &str = r#"
module OracleTags
  def test(name, tags = nil, &block)
    name = ([name] + tags.map { |key, value| "tag:" + [key, value].join(":") }).join("_") if tags
    super(name, &block)
  end
end
ActiveSupport::TestCase.singleton_class.prepend(OracleTags)
"#;

fn ruby() -> Command {
    Command::new(std::env::var("RUBY").unwrap_or_else(|_| "ruby".into()))
}

fn minitest_version() -> String {
    include_str!("../runtime/spinel/scaffold/Gemfile.lock")
        .lines()
        .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
        .expect("scaffold Minitest lock")
        .to_string()
}

/// `id => (status, assertions)` for every test Minitest runs natively in
/// `classes`.
fn native_results(sources: &[&str], classes: &[&str]) -> BTreeMap<String, (String, u64)> {
    let mut oracle = format!(
        "gem \"minitest\", \"={}\"\nrequire \"minitest/test\"\nMinitest.seed = 4242\nrequire \"json\"\nrequire \"active_support\"\nrequire \"active_support/test_case\"\nrequire \"active_support/core_ext/array/conversions\"\n{ORACLE_TAGS}\n",
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
    puts JSON.generate(id: "#{{klass.name}}##{{name}}", status: status, assertions: result.assertions)
  end
end
"##,
        classes.join(", ")
    ));
    let out = ruby().args(["-e", &oracle]).output().expect("native Minitest");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    rows(&String::from_utf8_lossy(&out.stdout))
}

fn rows(text: &str) -> BTreeMap<String, (String, u64)> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        let result = (row["status"].as_str().unwrap().to_string(), row["assertions"].as_u64().expect("assertions"));
        assert!(map.insert(id.clone(), result).is_none(), "{id} reported twice");
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
fn test_macros_expand_and_constant_loops_unroll_as_ruby_declares_them() {
    let native = native_results(
        &[STAGED_CHANGE, REASONS, TEST_GENERATOR, PERMISSION_HELPER, MACRO_TEST, CONST_LOOP_TEST],
        &["MacroTest", "ConstLoopTest"],
    );
    // The oracle is the reference; the literal list pins what it means.
    let expected = [
        "ConstLoopTest#test_ALPHA_is_a_reason",
        "ConstLoopTest#test_BETA_is_a_reason",
        "ConstLoopTest#test_case_first",
        "ConstLoopTest#test_case_second",
        "ConstLoopTest#test_computed_alpha",
        "ConstLoopTest#test_computed_beta",
        "ConstLoopTest#test_helper_add",
        "ConstLoopTest#test_helper_remove",
        "ConstLoopTest#test_helper_tip",
        "ConstLoopTest#test_label_Adding",
        "ConstLoopTest#test_label_Tipping",
        "ConstLoopTest#test_tip_unrelated_to_add",
        "MacroTest#test_alpha_is_allowed_for_products,_manage_products,_and_edit_price",
        "MacroTest#test_alpha_is_refused_for_products",
        "MacroTest#test_beta_is_allowed_for_products_and_manage_products",
        "MacroTest#test_beta_is_refused_for_",
        "MacroTest#test_check_alpha_fails_when_1_is_two",
        "MacroTest#test_check_alpha_with_read",
        "MacroTest#test_check_beta_fails_when_2_is_two",
        "MacroTest#test_check_beta_with_read_and_write",
        "MacroTest#test_check_gamma_fails_when_3_is_two",
        "MacroTest#test_check_gamma_with_read,_write,_and_admin",
        "MacroTest#test_each_x",
        "MacroTest#test_each_y",
        "MacroTest#test_plain",
        "MacroTest#test_tagged_alpha_tag:slow:true",
        "MacroTest#test_tagged_beta_tag:slow:true",
        "MacroTest#test_tagged_gamma_tag:slow:true",
    ];
    assert_eq!(native.keys().map(String::as_str).collect::<Vec<_>>(), expected, "native oracle");
    let failing: Vec<&str> = native.iter().filter(|(_, (s, _))| s != "pass").map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        failing,
        [
            "ConstLoopTest#test_helper_add",
            "ConstLoopTest#test_helper_remove",
            "MacroTest#test_alpha_is_refused_for_products",
            "MacroTest#test_check_beta_fails_when_2_is_two",
        ],
        "native outcomes: {native:#?}"
    );

    roundhouse::ingest::survey::activate();
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/staged_change.rb", STAGED_CHANGE)
        .write("app/models/reasons.rb", REASONS)
        .write("lib/test_generator.rb", TEST_GENERATOR)
        .write("lib/access/permission_helper.rb", PERMISSION_HELPER)
        .write("test/models/macro_test.rb", MACRO_TEST)
        .write("test/models/const_loop_test.rb", CONST_LOOP_TEST)
        .emit(BuildTarget::Ruby);
    let gaps: Vec<String> = roundhouse::ingest::survey::drain().iter().map(|e| e.to_string()).collect();
    // TestGenerator's own body (`alias_method` / `define_method` on the
    // module it is mixed into) is a compile diagnostic of its own; no
    // emitted test calls it, since every call of a macro is expanded.
    let generator = ["alias_method requires a proven class/module object receiver", "define_method requires a proven class/module object receiver"];
    assert_eq!(errors.len(), generator.len(), "{errors:#?}");
    for (error, want) in errors.iter().zip(generator) {
        assert!(error.contains("Module protocol not supported") && error.contains(want), "{errors:#?}");
    }
    let named = [
        r#"tests declared by a call of the class method `generate_each` are not modeled; they will not run (`MacroTest` line 39: its body does more than declare tests (`names.each { |n| test("each #{n}") { assert n } }`))"#,
        "tests declared by a loop over a collection that is not a literal are not modeled; they will not run (`ConstLoopTest` line 41, first test at line 42: Reasons.all)",
    ];
    for gap in named {
        assert_eq!(gaps.iter().filter(|g| g.contains(gap)).count(), 1, "{gap}\n{gaps:#?}");
    }
    assert_eq!(gaps.iter().filter(|g| g.contains("tests declared by")).count(), named.len(), "{gaps:#?}");

    // The index lists exactly the native tests the gaps do not name.
    let want: BTreeMap<String, (String, u64)> =
        native.iter().filter(|(id, _)| !NOT_MODELED.contains(&id.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    let classes = index(&emitted);
    let mut indexed: Vec<String> = classes
        .iter()
        .flat_map(|(class, (_, tests))| tests.iter().map(move |t| format!("{class}#{t}")))
        .collect();
    indexed.sort();
    assert_eq!(indexed, want.keys().cloned().collect::<Vec<_>>(), "index");

    // Running the emitted files reports the same ids, outcomes and
    // assertion counts.
    let mut ran = BTreeMap::new();
    for (file, _) in classes.values() {
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), file])
            .current_dir(&emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!("{file}\nstdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, result) in rows(&text) {
            assert!(ran.insert(id.clone(), result).is_none(), "{id} ran twice\n{log}");
        }
    }
    assert_eq!(ran, want, "emitted results vs native");
}

/// Strict ingest names the first macro call it does not expand.
#[test]
fn an_unexpandable_macro_call_is_named() {
    let source = r##"class StrictTest < ActiveSupport::TestCase
  class << self
    def generate(name)
      puts name
      test("t #{name}") { assert true }
    end
  end

  generate(:a)
end
"##;
    let err = roundhouse::ingest::ingest_test_files(source.as_bytes(), "strict_test.rb").unwrap_err().to_string();
    assert!(
        err.contains("tests declared by a call of the class method `generate` are not modeled; they will not run (`StrictTest` line 9: its body does more than declare tests (`puts name`))"),
        "{err}"
    );
}
