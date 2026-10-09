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
//!   Array of Hashes naming modules) declares one test per element, when
//!   each constant is a literal of frozen values that every source uses
//!   only in the few ways known to leave it as written;
//! - a macro or loop that cannot be evaluated before it runs is a named
//!   survey gap, and the class's other tests still run;
//! - each case below the first test is one where an earlier version
//!   invented or silently dropped tests (an `excluding` Array argument, a
//!   constant changed at load, an ancestor's constant, `Module#constants`
//!   order, helpers beside a macro, a loop local assigned again, writes
//!   after a module body, a redefined macro, `send` of a macro, element
//!   identity, a singleton macro's constant scope, a concern macro in a
//!   test-root helper, a later `extend` or plain concern method, a
//!   subclass's call of an inherited macro, a test source registered
//!   after the test, `A = B =`, changes through `const_get` or a block,
//!   a class-body local a loop's tests read, a concern macro that cannot
//!   be expanded or is registered later, a module mixed in again, a
//!   prepended module, `alias_method`, a `ClassMethods` reopened in
//!   another source, a concern the superclass has, a macro called in a
//!   loop, a condition or a plain method, a change through a part of a
//!   constant: `first`, `to_a`, a block call's result, an unfrozen String
//!   another constant holds; a constant passed on, `class_methods` from
//!   outside the concern, a test class declared in another file, a macro
//!   called from an `included` block, by `send("gen")` or `method(:gen)`;
//!   `constants` defined again in a module body, `class << self`, under
//!   `extend self` / `module_function`, by `define_method`, in a mixed-in
//!   module or by a `class Module` reopen; a change through a literal
//!   `constantize` / `safe_constantize`; a nested class's superclass that
//!   writes a constant; a change to a constant or module found by a name
//!   not known before the program runs).
//!
//! The native oracle is Core's TestGenerator and ActiveSupport's `test`
//! under Minitest; ids, outcomes and assertion counts must match.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

/// As Core's sources are, with frozen String literals: an unfrozen
/// String constant another constant holds is a gap
/// (`changes_through_parts_of_a_constant_are_gaps`).
const STAGED_CHANGE: &str = r##"# frozen_string_literal: true

module StagedChange
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

  CASES = [{ name: :first, mod: StagedChange }.freeze, { name: :second, mod: Comparable }.freeze].freeze
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
/// program does not. `Reasons.constants` is one: `MacroTest` passes
/// `Reasons` to a method that may add to it.
const NOT_MODELED: [&str; 6] = [
    "ConstLoopTest#test_ALPHA_is_a_reason",
    "ConstLoopTest#test_BETA_is_a_reason",
    "ConstLoopTest#test_computed_alpha",
    "ConstLoopTest#test_computed_beta",
    "MacroTest#test_each_x",
    "MacroTest#test_each_y",
];

/// The loop gaps of a constant (`consts::NOT_LITERAL`, `consts::USED`).
const NOT_LITERAL: &str =
    "tests declared by a loop over a constant whose value is not a literal of frozen values are not modeled; they will not run";
const USED: &str = "tests declared by a loop over a constant a source uses in a way not known to leave it unchanged are not modeled; they will not run";
/// The loop gaps of `Module#constants` (`consts::REOPENED`,
/// `consts::MODULE_BODY`) and of a constant not bound once
/// (`consts::UNCERTAIN`).
const REOPENED: &str = "tests declared by a loop over the constants of a module changed from outside its body are not modeled; they will not run";
const MODULE_BODY: &str = "tests declared by a loop over the constants of a module whose body is not one listed as declaring only the constants it shows are not modeled; they will not run";
const UNCERTAIN: &str = "tests declared by a loop over a constant not assigned once with certainty are not modeled; they will not run";

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
        "gem \"minitest\", \"={}\"\nrequire \"minitest/test\"\nMinitest.seed = 4242\nrequire \"json\"\nrequire \"active_support\"\nrequire \"active_support/test_case\"\nrequire \"active_support/core_ext/array/conversions\"\nrequire \"active_support/core_ext/array/access\"\n{ORACLE_TAGS}\n",
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
        "tests declared by a loop over the constants of a module changed from outside its body are not modeled; they will not run (`ConstLoopTest` line 14, first test at line 15: `Reasons.constants`: `Reasons` may be changed at `test_permission(Reasons, :alpha, sufficient: [:products, :manage_products, :edit…` (test/models/macro_test.rb line 41, a test source not registered before this one))",
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

// ---- The review's cases, each against native Minitest -------------

/// Native Minitest's results for `files` (path, source; the Ruby ones)
/// loaded in order, then the emitted program's (in survey mode): the index lists every
/// native id `gapped` does not take, the run reports native's outcomes
/// and assertion counts for them, and the survey names each of `gaps`
/// once and records no other unmodeled test. Returns the native results.
fn matches_native(
    files: &[(&str, &str)],
    classes: &[&str],
    gapped: &dyn Fn(&str) -> bool,
    gaps: &[&str],
) -> BTreeMap<String, (String, u64)> {
    let sources: Vec<String> = files
        .iter()
        .filter(|(path, _)| path.ends_with(".rb"))
        .map(|(_, text)| text.lines().filter(|l| l.trim() != r#"require "test_helper""#).map(|l| format!("{l}\n")).collect())
        .collect();
    let native = native_results(&sources.iter().map(String::as_str).collect::<Vec<_>>(), classes);
    assert!(!native.is_empty(), "native declares tests");

    roundhouse::ingest::survey::activate();
    let mut app = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n");
    for (path, text) in files {
        app = app.write(path, text);
    }
    let (emitted, _errors) = app.emit(BuildTarget::Ruby);
    let recorded: Vec<String> = roundhouse::ingest::survey::drain().iter().map(|e| e.to_string()).collect();
    for gap in gaps {
        assert_eq!(recorded.iter().filter(|g| g.contains(gap)).count(), 1, "{gap}\n{recorded:#?}");
    }
    let unmodeled = recorded.iter().filter(|g| g.contains("tests declared by") || g.contains("not modeled")).count();
    assert_eq!(unmodeled, gaps.len(), "{recorded:#?}");

    let want: BTreeMap<String, (String, u64)> =
        native.iter().filter(|(id, _)| !gapped(id)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let listed = index(&emitted);
    let mut indexed: Vec<String> = listed
        .iter()
        .filter(|(class, _)| classes.contains(&class.as_str()))
        .flat_map(|(class, (_, tests))| tests.iter().map(move |t| format!("{class}#{t}")))
        .collect();
    indexed.sort();
    assert_eq!(indexed, want.keys().cloned().collect::<Vec<_>>(), "index vs native {native:#?}");
    assert_eq!(run_emitted(&emitted, &listed, classes), want, "emitted results vs native");
    native
}

/// `id => (status, assertions)` from running each emitted file of
/// `classes` that lists a test.
fn run_emitted(
    emitted: &std::path::Path,
    listed: &BTreeMap<String, (String, Vec<String>)>,
    classes: &[&str],
) -> BTreeMap<String, (String, u64)> {
    let mut ran = BTreeMap::new();
    for (class, (file, tests)) in listed {
        if tests.is_empty() || !classes.contains(&class.as_str()) {
            continue;
        }
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), file])
            .current_dir(emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            .output()
            .expect("emitted CRuby");
        let log = format!("{file}\nstdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, result) in rows(&text) {
            if id.starts_with(&format!("{class}#")) {
                assert!(ran.insert(id.clone(), result).is_none(), "{id} ran twice\n{log}");
            }
        }
    }
    ran
}

const LEVELS: &str = r##"module Levels
  ZED = :zed
  ALPHA = :alpha
  MID = :mid
  ALL = [:a, :b, :c, :d].freeze
end
"##;

const LV: &str = r##"module Lv
  ZQX_NEW = 1
  VERSION = 2
  QQQ_ZZ = 3
  Error = 4
  NAME = 5
end
"##;

/// `excluding`/`without` flatten their arguments one level, as
/// ActiveSupport does; a loop that depends on `Module#constants`' order
/// (an index, `first`, `join`) is a named gap, since Ruby orders them by
/// its symbol table.
#[test]
fn excluding_flattens_and_constants_order_is_a_gap() {
    let order_test = r##"class OrderTest < ActiveSupport::TestCase
  Levels.constants.each_with_index do |c, i|
    test "#{i} is #{c}" do
      assert_equal c, Levels.constants[i]
    end
  end

  [Levels.constants.join("-")].each do |joined|
    test "joined #{joined}" do
      assert joined
    end
  end

  Levels::ALL.excluding([:a, :b]).each do |x|
    test "excluding #{x}" do
      refute_includes [:a, :b], x
    end
  end

  Levels::ALL.without(:c, [:d]).each do |x|
    test "without #{x}" do
      assert x
    end
  end
end
"##;
    let lv_test = r##"class LvOrderTest < ActiveSupport::TestCase
  Lv.constants.each_with_index do |c, i|
    test "#{i} #{c}" do
      assert c
    end
  end

  test "first is #{Lv.constants.first}" do
    assert true
  end

  Lv.constants.each do |c|
    test "any order #{c}" do
      assert Lv.const_get(c)
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/levels.rb", LEVELS), ("app/models/lv.rb", LV), ("test/models/order_test.rb", order_test), ("test/models/lv_order_test.rb", lv_test)],
        &["OrderTest", "LvOrderTest"],
        &|id| {
            let name = id.split_once("#test_").map_or("", |(_, n)| n);
            name.starts_with(|c: char| c.is_ascii_digit()) || name.starts_with("joined_") || name.starts_with("first_is_")
        },
        &[
            "tests declared by a loop whose order Ruby does not fix (`Module#constants`) are not modeled; they will not run (`OrderTest` line 2",
            "tests declared by a loop over a collection that is not a literal are not modeled; they will not run (`OrderTest` line 8",
            "tests declared by a loop whose order Ruby does not fix (`Module#constants`) are not modeled; they will not run (`LvOrderTest` line 2",
            "(`LvOrderTest` line 8",
        ],
    );
    for id in ["OrderTest#test_excluding_c", "OrderTest#test_excluding_d", "OrderTest#test_without_a", "OrderTest#test_without_b"] {
        assert_eq!(native.get(id).map(|r| r.0.as_str()), Some("pass"), "{id}: {native:#?}");
    }
    assert_eq!(native.keys().filter(|id| id.starts_with("OrderTest#test_excluding_") || id.starts_with("OrderTest#test_without_")).count(), 4);
    assert_eq!(native.keys().filter(|id| id.starts_with("LvOrderTest#test_any_order_")).count(), 5);
}

/// A loop over a constant some source changes after assigning it (`<<`,
/// `[]=`, `concat`, `delete`, in its module or the test class) is a
/// named gap; one nothing changes still unrolls.
#[test]
fn constants_changed_after_assignment_are_gaps() {
    let kinds = r##"module Kinds
  ALL = [:a]
  ALL << :b
  NAMES = { "x" => 1 }
  NAMES["y"] = 2
  LIST = %i[p q]
  LIST.concat(%i[r])
  FIXED = [:z].freeze
end
"##;
    let flags = "module Flags\n  ALL = [:on, :off, :legacy]\n  ALL.delete(:legacy)\nend\n";
    let test = r##"class MutatedConstTest < ActiveSupport::TestCase
  Kinds::ALL.each do |k|
    test "kind #{k}" do
      assert k
    end
  end

  Kinds::NAMES.each do |name, n|
    test "name #{name}" do
      assert n
    end
  end

  Kinds::LIST.each do |l|
    test "list #{l}" do
      assert l
    end
  end

  Kinds::FIXED.each do |f|
    test "fixed #{f}" do
      assert_equal :z, f
    end
  end

  CASES = [:a]
  CASES << :b
  CASES.each do |c|
    test "c #{c}" do
      assert c
    end
  end

  Flags::ALL.each do |f|
    test "flag #{f}" do
      assert_includes Flags::ALL, f
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/flags.rb", flags), ("test/models/mutated_const_test.rb", test)],
        &["MutatedConstTest"],
        &|id| !id.ends_with("#test_fixed_z"),
        &[
            &format!("{USED} (`MutatedConstTest` line 2, first test at line 3: `Kinds::ALL`: `ALL << :b` (app/models/kinds.rb line 3))"),
            // (A Hash with String keys: its keys are frozen copies.)
            &format!("{USED} (`MutatedConstTest` line 8, first test at line 9: `Kinds::NAMES`: `NAMES[\"y\"] = 2` (app/models/kinds.rb line 5))"),
            &format!("{USED} (`MutatedConstTest` line 14, first test at line 15: `Kinds::LIST`: `LIST.concat(%i[r])` (app/models/kinds.rb line 7))"),
            &format!("{USED} (`MutatedConstTest` line 28, first test at line 29: `MutatedConstTest::CASES`: `CASES << :b` (test/models/mutated_const_test.rb line 27))"),
            &format!("{USED} (`MutatedConstTest` line 34, first test at line 35: `Flags::ALL`: `ALL.delete(:legacy)` (app/models/flags.rb line 3))"),
        ],
    );
    assert_eq!(native.len(), 12, "{native:#?}");
}

/// A constant the lexical scopes do not have is looked up in the
/// class's ancestors before the top level; one an ancestor without a
/// registered source may declare is a named gap.
#[test]
fn ancestor_constants_come_before_the_top_level() {
    let helper = "class BaseModeTest < ActiveSupport::TestCase\n  MODES = [:base_x].freeze\nend\n";
    let far_base = "class FarBase < ActiveSupport::TestCase\n  MODES = [:far].freeze\nend\n";
    let sub = r##"require "test_helper"

class SubModeTest < BaseModeTest
  MODES.each do |m|
    test "mode #{m}" do
      assert_equal :base_x, m
    end
  end
end
"##;
    let far = r##"require "test_helper"

class FarModeTest < FarBase
  MODES.each do |m|
    test "far #{m}" do
      assert_equal :far, m
    end
  end
end
"##;
    matches_native(
        &[
            ("app/models/modes.rb", "MODES = [:global_a, :global_b].freeze\n"),
            ("test/test_helper.rb", helper),
            ("test/support/far_base.rb", far_base),
            ("test/models/sub_mode_test.rb", sub),
            ("test/models/far_mode_test.rb", far),
        ],
        &["SubModeTest", "FarModeTest"],
        &|id| id.starts_with("FarModeTest#"),
        &["tests declared by a loop over a constant an ancestor of its class may declare are not modeled; they will not run (`FarModeTest` line 4, first test at line 5: `MODES`, looked up past the ancestors of `FarModeTest`: `FarBase` is declared in test/support/far_base.rb, a test source not registered before this one)"],
    );
}

/// The methods beside a macro in `class << self` stay class methods.
#[test]
fn singleton_helpers_beside_a_macro_still_run() {
    let test = r##"class HelperSingletonTest < ActiveSupport::TestCase
  class << self
    def gen(k)
      test("gen #{k}") do
        assert_equal "help", self.class.helper
      end
    end

    def helper
      "help"
    end
  end

  gen(:a)

  test "uses helper" do
    assert_equal "help", self.class.helper
  end
end
"##;
    let native = matches_native(&[("test/models/helper_singleton_test.rb", test)], &["HelperSingletonTest"], &|_| false, &[]);
    assert_eq!(native.len(), 2, "{native:#?}");
}

/// Tests share the variables of the loop body that declares them; one
/// assigned again, or an object one changes, is a named gap.
#[test]
fn loop_locals_assigned_again_are_gaps() {
    let test = r##"class LocalReassignTest < ActiveSupport::TestCase
  [:a, :b].each do |k|
    label = "first"
    test "label #{k} #{label}" do
      assert_equal "second", label
    end
    label = "second"
  end

  [:c].each do |k|
    count = 1
    test "count #{k}" do
      count += 1
      assert_equal 2, count
    end
    test "count again #{k}" do
      count += 1
      assert_equal 2, count
    end
  end

  [:m].each do |k|
    seen = []
    test "push #{k}" do
      seen << k
      assert_equal [k], seen
    end
    test "push again #{k}" do
      seen << k
      assert_equal [k], seen
    end
  end

  [:e].each do |k|
    label = "fixed #{k}"
    test "fixed #{k}" do
      assert_equal "fixed e", label
    end
  end
end
"##;
    let native = matches_native(
        &[("test/models/local_reassign_test.rb", test)],
        &["LocalReassignTest"],
        &|id| !id.ends_with("#test_fixed_e"),
        &[
            "tests declared by a loop whose tests share a variable that is assigned again are not modeled; they will not run (`LocalReassignTest` line 2",
            "tests declared by a loop whose tests share a variable that is assigned again are not modeled; they will not run (`LocalReassignTest` line 10",
            "tests declared by a loop whose tests change an object they share are not modeled; they will not run (`LocalReassignTest` line 22",
        ],
    );
    // Shared variables are why: of two tests of one iteration, the one
    // Minitest runs second sees the other's change and fails.
    for pair in [["count_c", "count_again_c"], ["push_m", "push_again_m"]] {
        let failed = pair.iter().filter(|t| native[&format!("LocalReassignTest#test_{t}")].0 == "fail").count();
        assert_eq!(failed, 1, "{pair:?}: {native:#?}");
    }
    assert!(["label_a_first", "label_b_first", "fixed_e"].iter().all(|t| native[&format!("LocalReassignTest#test_{t}")].0 == "pass"));
}

/// `Module#constants` is what the module's whole source defines: writes
/// after its body (`Codes::C =`, `const_set`) make the loop a gap, and a
/// `private_constant` is not listed.
#[test]
fn module_constants_count_writes_outside_the_body() {
    let codes = "module Codes\n  A = :a\n  B = :b\nend\nCodes::C = :c\nCodes.const_set(:D, :d)\n";
    let marks = "module Marks\n  X = :x\n  private_constant :X\n  Y = :y\nend\n";
    let test = r##"class CodesTest < ActiveSupport::TestCase
  Codes.constants.each do |c|
    test "code #{c}" do
      assert c
    end
  end

  Marks.constants.each do |m|
    test "mark #{m}" do
      assert_equal :Y, m
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/codes.rb", codes), ("app/models/marks.rb", marks), ("test/models/codes_test.rb", test)],
        &["CodesTest"],
        &|id| id.starts_with("CodesTest#test_code_"),
        &["(`CodesTest` line 2, first test at line 3: "],
    );
    assert_eq!(native.keys().filter(|id| id.starts_with("CodesTest#test_code_")).count(), 4, "{native:#?}");
}

/// The last definition of a class method is the one a call runs: a
/// redefinition that declares nothing declares nothing.
#[test]
fn a_redefined_macro_declares_what_its_last_definition_does() {
    let test = r##"class RedefTest < ActiveSupport::TestCase
  class << self
    def gen(k)
      test("gen #{k}") { assert true }
    end
  end

  class << self
    def gen(k)
      # replaced: declares nothing
    end
  end

  gen(:a)

  def self.other(k)
    test("other #{k}") { assert true }
  end

  def self.other(k)
    nil
  end

  other(:b)

  test "kept" do
    assert true
  end
end
"##;
    let native = matches_native(&[("test/models/redef_test.rb", test)], &["RedefTest"], &|_| false, &[]);
    assert_eq!(native.keys().collect::<Vec<_>>(), ["RedefTest#test_kept"]);
}

/// A `def self.` macro used other than by a call from the class body
/// (`send(:gen, …)`) is named; its plain calls expand.
#[test]
fn a_macro_used_other_than_by_a_call_is_named() {
    let test = r##"class SelfDefTest < ActiveSupport::TestCase
  def self.gen(k, suffix = "x")
    test("gen #{k} #{suffix}") do
      assert_equal "x", suffix
      assert_kind_of Symbol, k
    end
  end

  gen(:a)
  gen(:b, "y")
  send(:gen, :c)
end
"##;
    matches_native(
        &[("test/models/self_def_test.rb", test)],
        &["SelfDefTest"],
        &|id| id == "SelfDefTest#test_gen_c_x",
        &["tests declared by the class method `gen` where it is used other than called from the class body are not modeled; they will not run (`SelfDefTest`, line 2: `send(:gen, :c)` at line 11)"],
    );
}

/// Each test reads its element from the app constant, so an Array or
/// String element is the constant's own object. A loop over a literal
/// with an unfrozen String is a gap, and so is one whose mutable
/// elements (an Array in a frozen Array) a source reads other than as
/// the loop.
#[test]
fn constant_elements_keep_their_identity() {
    let frozen = "# frozen_string_literal: true\n\nmodule FrozenKinds\n  ALL = [\"a\", \"b\"].freeze\n  NESTED = [[\"x\", \"y\"]].freeze\n  NESTED_USED = [[\"u\"]].freeze\nend\n";
    let thawed = "# frozen_string_literal: true\n\nmodule ThawedKinds\n  ALL = [\"c\"]\nend\n";
    let loose = "module LooseKinds\n  ALL = [\"d\"].freeze\nend\n";
    let test = r##"class IdentityTest < ActiveSupport::TestCase
  FrozenKinds::ALL.each do |k|
    test "same #{k}" do
      assert_same FrozenKinds::ALL.find { |x| x == k }, k
    end
  end

  FrozenKinds::NESTED.each do |pair|
    test "pair #{pair.join}" do
      refute_predicate pair, :frozen?
    end
  end

  FrozenKinds::NESTED_USED.each do |pair|
    test "used #{pair.join}" do
      assert_same FrozenKinds::NESTED_USED.first, pair
    end
  end

  ThawedKinds::ALL.each do |k|
    test "thawed #{k}" do
      assert_same ThawedKinds::ALL.first, k
    end
  end

  LooseKinds::ALL.each do |k|
    test "loose #{k}" do
      assert_kind_of String, k
    end
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/frozen_kinds.rb", frozen),
            ("app/models/thawed_kinds.rb", thawed),
            ("app/models/loose_kinds.rb", loose),
            ("test/models/identity_test.rb", test),
        ],
        &["IdentityTest"],
        &|id| id.ends_with("#test_used_u") || id.ends_with("#test_loose_d"),
        &[
            &format!("{USED} (`IdentityTest` line 14, first test at line 15: `FrozenKinds::NESTED_USED`: `assert_same FrozenKinds::NESTED_USED.first, pair` (test/models/identity_test.rb line 16))"),
            &format!("{NOT_LITERAL} (`IdentityTest` line 26, first test at line 27: `LooseKinds::ALL`: `ALL = [\"d\"].freeze` (app/models/loose_kinds.rb line 2))"),
        ],
    );
    assert_eq!(native.len(), 6, "{native:#?}");
    assert!(native.values().all(|(s, _)| s == "pass"), "{native:#?}");
}

/// A block in a `class << self` macro reads constants through the
/// singleton class, not the class's includes: where they may differ,
/// the call is a named gap.
#[test]
fn a_singleton_macro_block_reads_constants_through_its_singleton_class() {
    let test = r##"class ScopeTest < ActiveSupport::TestCase
  include LimitHelper

  class << self
    def gen(k)
      test("gen #{k}") do
        assert_equal 10, LIMIT
      end
    end
  end

  gen(:a)

  test "direct" do
    assert_equal 5, LIMIT
  end
end
"##;
    matches_native(
        &[
            ("config/initializers/limit.rb", "LIMIT = 10\n"),
            ("app/models/limit_helper.rb", "module LimitHelper\n  LIMIT = 5\nend\n"),
            ("test/models/scope_test.rb", test),
        ],
        &["ScopeTest"],
        &|id| id == "ScopeTest#test_gen_a",
        &["tests declared by a call of the class method `gen` are not modeled; they will not run (`ScopeTest` line 12: "],
    );
}

/// A concern's macro in a helper under a test root expands with no gap,
/// and strict ingest accepts the class.
#[test]
fn a_concern_macro_from_a_test_root_helper_has_no_gap() {
    let test = "class ConcernMacroTest < ActiveSupport::TestCase\n  include Access::PermissionHelper\n\n  test_permission(Reasons, :alpha, sufficient: [:a, :b], insufficient: [:a])\nend\n";
    let native = native_results(&[REASONS, TEST_GENERATOR, PERMISSION_HELPER, test], &["ConcernMacroTest"]);
    assert_eq!(native.len(), 2, "{native:#?}");
    let (emitted, _errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/reasons.rb", REASONS)
        .write("lib/test_generator.rb", TEST_GENERATOR)
        .write("test/helpers/access/permission_helper.rb", PERMISSION_HELPER)
        .write("test/models/concern_macro_test.rb", test)
        .emit(BuildTarget::Ruby);
    let listed = index(&emitted);
    let mut tests: Vec<String> = listed["ConcernMacroTest"].1.iter().map(|t| format!("ConcernMacroTest#{t}")).collect();
    tests.sort();
    assert_eq!(tests, native.keys().cloned().collect::<Vec<_>>());
}

const GEN_CONCERN: &str = r##"module GenConcern
  extend ActiveSupport::Concern

  class_methods do
    def gen(k)
      test("concern #{k}") { assert_kind_of Symbol, k }
    end
  end
end
"##;

/// A class method comes from the latest of the modules a class body
/// includes or extends: an extended module's method that declares other
/// tests, or none, or a later concern's plain method, is the one a call
/// runs.
#[test]
fn the_latest_module_gives_the_class_method() {
    let other = "module OtherGen\n  def gen(k)\n    test(\"other #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n";
    let quiet = "module QuietGen\n  def gen(k)\n  end\nend\n";
    let plain = "module PlainConcern\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def gen(k)\n      k\n    end\n  end\nend\n";
    let test = r##"class OverrideTest < ActiveSupport::TestCase
  include GenConcern
  extend OtherGen

  gen(:a)
end

class QuietTest < ActiveSupport::TestCase
  include GenConcern
  extend QuietGen

  gen(:b)

  test "quiet kept" do
    assert true
  end
end

class TwoConcernsTest < ActiveSupport::TestCase
  include GenConcern
  include PlainConcern

  gen(:a)

  test "kept" do
    assert true
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/gen_concern.rb", GEN_CONCERN),
            ("app/models/other_gen.rb", other),
            ("app/models/quiet_gen.rb", quiet),
            ("app/models/plain_concern.rb", plain),
            ("test/models/override_test.rb", test),
        ],
        &["OverrideTest", "QuietTest", "TwoConcernsTest"],
        &|_| false,
        &[],
    );
    assert_eq!(
        native.keys().collect::<Vec<_>>(),
        ["OverrideTest#test_other_a", "QuietTest#test_quiet_kept", "TwoConcernsTest#test_kept"]
    );
}

/// A subclass's call of a macro its superclass gets (from its own
/// `class << self`, or a concern it includes) is named; the inherited
/// tests still run.
#[test]
fn a_subclass_call_of_an_inherited_macro_is_named() {
    let base = "class ABaseMacroTest < ActiveSupport::TestCase\n  class << self\n    def gen(k)\n      test(\"gen #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\n\n  gen(:a)\nend\n";
    let child = "class BChildMacroTest < ABaseMacroTest\n  gen(:b)\nend\n";
    let parent = "class AParentTest < ActiveSupport::TestCase\n  include GenConcern\n\n  gen(:p)\nend\n";
    let concern_child = "class BChildTest < AParentTest\n  gen(:c)\nend\n";
    let inherited = "the method it runs is not one the class body, or a module it includes or extends, defines (one it inherits, or another), and a method of that name declares tests in ";
    let native = matches_native(
        &[
            ("app/models/gen_concern.rb", GEN_CONCERN),
            ("test/models/a_base_macro_test.rb", base),
            ("test/models/a_parent_test.rb", parent),
            ("test/models/b_child_macro_test.rb", child),
            ("test/models/b_child_test.rb", concern_child),
        ],
        &["ABaseMacroTest", "BChildMacroTest", "AParentTest", "BChildTest"],
        &|id| id == "BChildMacroTest#test_gen_b" || id == "BChildTest#test_concern_c",
        &[
            &format!("tests declared by a call of the class method `gen` are not modeled; they will not run (`BChildMacroTest` line 2: {inherited}"),
            &format!("tests declared by a call of the class method `gen` are not modeled; they will not run (`BChildTest` line 2: {inherited}"),
        ],
    );
    assert_eq!(native.len(), 6, "{native:#?}");
}

/// A test source that sorts after the test (one its helper requires
/// first) is not registered when the test is read: a loop over a
/// constant it changes, or one it may declare in an ancestor of the
/// class (reopening `ActiveSupport::TestCase`), is a named gap.
#[test]
fn later_test_sources_that_change_a_loop_are_gaps() {
    let levels = "module Levels\n  ALL = [:a, :b]\nend\n";
    let levels_test = r##"class LevelsTest < ActiveSupport::TestCase
  Levels::ALL.each do |l|
    test "level #{l}" do
      assert_kind_of Symbol, l
    end
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/levels.rb", levels),
            ("test/support/levels_ext.rb", "Levels::ALL << :extra\n"),
            ("test/models/levels_test.rb", levels_test),
        ],
        &["LevelsTest"],
        &|_| true,
        &["tests declared by a loop over a constant a source uses in a way not known to leave it unchanged are not modeled; they will not run (`LevelsTest` line 2, first test at line 3: `Levels::ALL`: `Levels::ALL << :extra` (test/support/levels_ext.rb line 1, a test source not registered before this one))"],
    );
    assert!(native.contains_key("LevelsTest#test_level_extra"), "{native:#?}");

    // The same under `roundhouse.yml` test roots outside `test/` (Core's
    // `components/*/test/…`).
    matches_native(
        &[
            ("roundhouse.yml", "test_paths:\n  - components/a/test/models\n  - components/z/test/support\n"),
            ("app/models/levels.rb", levels),
            ("components/z/test/support/levels_ext.rb", "Levels::ALL << :extra\n"),
            ("components/a/test/models/levels_test.rb", levels_test),
        ],
        &["LevelsTest"],
        &|_| true,
        &["tests declared by a loop over a constant a source uses in a way not known to leave it unchanged are not modeled; they will not run (`LevelsTest` line 2, first test at line 3: `Levels::ALL`: `Levels::ALL << :extra` (components/z/test/support/levels_ext.rb line 1, a test source not registered before this one))"],
    );

    let modes_test = r##"class ModesTest < ActiveSupport::TestCase
  MODES.each do |m|
    test "mode #{m}" do
      assert_equal :support, m
    end
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/modes.rb", "MODES = [:top]\n"),
            ("test/support/base_ext.rb", "class ActiveSupport::TestCase\n  MODES = [:support]\nend\n"),
            ("test/models/modes_test.rb", modes_test),
        ],
        &["ModesTest"],
        &|_| true,
        &["tests declared by a loop over a constant an ancestor of its class may declare are not modeled; they will not run (`ModesTest` line 2, first test at line 3: `MODES`, looked up past the ancestors of `ModesTest`: `ActiveSupport::TestCase` is declared in test/support/base_ext.rb, a test source not registered before this one)"],
    );
    assert_eq!(native.keys().collect::<Vec<_>>(), ["ModesTest#test_mode_support"]);
}

/// `A = B = :x` declares both: a loop over the module's constants is a
/// gap rather than one that drops `B`.
#[test]
fn a_chained_constant_write_is_not_dropped() {
    let chained = "module Chained\n  A = B = :x\n  C = :c\nend\n";
    let test = r##"class ChainedTest < ActiveSupport::TestCase
  Chained.constants.each do |c|
    test "const #{c}" do
      assert_equal Chained.const_get(c), Chained.const_get(c)
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/chained.rb", chained), ("test/models/chained_test.rb", test)],
        &["ChainedTest"],
        &|_| true,
        &[&format!("{MODULE_BODY} (`ChainedTest` line 2, first test at line 3: `Chained.constants`: its body runs `A = B = :x` (line 2), not listed as adding no constant)")],
    );
    assert_eq!(native.len(), 3, "{native:#?}");
}

/// A constant changed through `const_get`, or through its elements in a
/// block (`each(&:reverse!)`, `each { |k| k << :z }`, a local bound to
/// one), is a gap; a block that only reads them is not.
#[test]
fn constants_changed_through_const_get_or_elements_are_gaps() {
    let levels = "module Levels\n  ALL = [:a, :b]\n  NAMES = [[:x, :w], [:y, :v]].freeze\n  KEYS = [[:k]].freeze\n  COPIED = [[:m]].freeze\n  SAFE = [[:s]].freeze\nend\n";
    let setup = r##"Levels.const_get(:ALL) << :c
Levels::NAMES.each(&:reverse!)
Levels::KEYS.each { |k| k << :z }
Levels::COPIED.each do |n|
  m = n
  m.concat([:q])
end
Levels::SAFE.each { |s| s.first }
Levels.const_get(:SAFE).map(&:first)
"##;
    let test = r##"class EscapeTest < ActiveSupport::TestCase
  Levels::ALL.each do |l|
    test "level #{l}" do
      assert_kind_of Symbol, l
    end
  end

  Levels::NAMES.each do |n|
    test "name #{n.first}" do
      assert_kind_of Symbol, n.first
    end
  end

  Levels::KEYS.each do |k|
    test "key #{k.join}" do
      assert_equal [:k, :z], k
    end
  end

  Levels::COPIED.each do |c|
    test "copied #{c.join}" do
      assert_equal [:m, :q], c
    end
  end

  Levels::SAFE.each do |s|
    test "safe #{s.first}" do
      assert_equal [:s], s
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/levels.rb", levels), ("app/models/levels_setup.rb", setup), ("test/models/escape_test.rb", test)],
        &["EscapeTest"],
        &|id| !id.ends_with("#test_safe_s"),
        &[
            &format!("{USED} (`EscapeTest` line 2, first test at line 3: `Levels::ALL`: `Levels.const_get(:ALL) << :c` (app/models/levels_setup.rb line 1))"),
            &format!("{USED} (`EscapeTest` line 8, first test at line 9: `Levels::NAMES`: `Levels::NAMES.each(&:reverse!)` (app/models/levels_setup.rb line 2))"),
            &format!("{USED} (`EscapeTest` line 14, first test at line 15: `Levels::KEYS`: `Levels::KEYS.each {{ |k| k << :z }}` (app/models/levels_setup.rb line 3))"),
            &format!("{USED} (`EscapeTest` line 20, first test at line 21: `Levels::COPIED`: `Levels::COPIED.each do |n|` (app/models/levels_setup.rb line 4))"),
        ],
    );
    for id in ["EscapeTest#test_level_c", "EscapeTest#test_name_w", "EscapeTest#test_key_kz", "EscapeTest#test_copied_mq"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// A loop's tests close over the class body's locals too; the emitted
/// tests bind only the loop's names, so a loop whose block reads one is
/// a gap. An inner loop's tests read the outer loop's parameter.
#[test]
fn loops_reading_class_body_locals_are_gaps() {
    let levels = "module Levels\n  ALL = [:a, :b].freeze\nend\n";
    let test = r##"class BodyLocalTest < ActiveSupport::TestCase
  prefix = "x"

  Levels::ALL.each do |l|
    test "level #{l}" do
      assert_equal "x", prefix
    end
  end

  [:c].each do |l|
    test "lit #{l}" do
      assert_equal "x", prefix
    end
  end

  [:o, :p].each do |o|
    [1].each do |i|
      test "nested #{o} #{i}" do
        assert_equal 1, i
        assert_includes [:o, :p], o
      end
    end
  end
end
"##;
    let outer = "tests declared by a loop whose block reads a local from outside it are not modeled; they will not run";
    let native = matches_native(
        &[("app/models/levels.rb", levels), ("test/models/body_local_test.rb", test)],
        &["BodyLocalTest"],
        &|id| !id.contains("#test_nested_"),
        &[
            &format!("{outer} (`BodyLocalTest` line 4, first test at line 5: `prefix` at line 6)"),
            &format!("{outer} (`BodyLocalTest` line 10, first test at line 11: `prefix` at line 12)"),
        ],
    );
    assert!(native.values().all(|(s, _)| s == "pass"), "{native:#?}");
    assert_eq!(native.len(), 5, "{native:#?}");
}

/// A concern macro that does more than declare tests is named, as is one
/// in a test source registered after the test (a helper its
/// `test_helper` requires); the class's other tests run.
#[test]
fn unexpandable_or_later_concern_macros_are_named() {
    let concern = "module LoudConcern\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def loud(k)\n      puts \"declaring\" if false\n      test(\"loud #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\nend\n";
    let test = "class UnexpandableTest < ActiveSupport::TestCase\n  include LoudConcern\n\n  loud(:a)\n\n  test \"kept\" do\n    assert true\n  end\nend\n";
    matches_native(
        &[("app/models/loud_concern.rb", concern), ("test/models/unexpandable_test.rb", test)],
        &["UnexpandableTest"],
        &|id| id == "UnexpandableTest#test_loud_a",
        &["tests declared by a call of the class method `loud` are not modeled; they will not run (`UnexpandableTest` line 4: its body does more than declare tests (`puts \"declaring\" if false`))"],
    );

    let test = "class ConcernMacroTest < ActiveSupport::TestCase\n  include Access::PermissionHelper\n\n  test_permission(Reasons, :alpha, sufficient: [:a, :b], insufficient: [:a])\nend\n";
    let native = matches_native(
        &[
            ("app/models/reasons.rb", REASONS),
            ("lib/test_generator.rb", TEST_GENERATOR),
            ("test/support/helpers/access/permission_helper.rb", PERMISSION_HELPER),
            ("test/models/concern_macro_test.rb", test),
        ],
        &["ConcernMacroTest"],
        &|_| true,
        &["tests declared by a call of the class method `test_permission` are not modeled; they will not run (`ConcernMacroTest` line 4: the module that defines it is not known (`Access::PermissionHelper` is declared in test/support/helpers/access/permission_helper.rb, a test source not registered before this one))"],
    );
    assert_eq!(native.len(), 2, "{native:#?}");
}

// ---- Round 3: mixin order, aliases, reopened modules, nested calls ----

/// The message prefix of a gap for a call of the macro `name`.
fn call_gap(name: &str, class: &str, line: u32) -> String {
    format!("tests declared by a call of the class method `{name}` are not modeled; they will not run (`{class}` line {line}: ")
}

const C_ONE: &str = "module COne\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def cgen(k)\n      test(\"one #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\nend\n";
const C_TWO: &str = "module CTwo\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def cgen(k)\n      test(\"two #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\nend\n";

/// Extending or including a module the class already has adds nothing:
/// the module mixed in between keeps its place ahead of it. Including
/// `A, B` puts `A` first.
#[test]
fn mixing_in_a_module_again_adds_nothing() {
    let gen_a = "module GenA\n  def gen(k)\n    test(\"a #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n";
    let gen_b = "module GenB\n  def gen(k)\n    test(\"b #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n";
    let test = r##"class ReextendTest < ActiveSupport::TestCase
  extend GenA
  extend GenB
  extend GenA

  gen(:x)
end

class ReincludeTest < ActiveSupport::TestCase
  include COne
  include CTwo
  include COne

  cgen(:y)
end

class ArgOrderTest < ActiveSupport::TestCase
  include COne, CTwo

  cgen(:z)
end
"##;
    let native = matches_native(
        &[
            ("app/models/c_one.rb", C_ONE),
            ("app/models/c_two.rb", C_TWO),
            ("app/models/gen_a.rb", gen_a),
            ("app/models/gen_b.rb", gen_b),
            ("test/models/reextend_test.rb", test),
        ],
        &["ReextendTest", "ReincludeTest", "ArgOrderTest"],
        &|_| false,
        &[],
    );
    assert_eq!(
        native.keys().collect::<Vec<_>>(),
        ["ArgOrderTest#test_one_z", "ReextendTest#test_b_x", "ReincludeTest#test_two_y"]
    );
}

/// A prepended concern's class methods come before the class's own
/// (`singleton_class.prepend`): the call is named, never expanded with
/// the class's own macro.
#[test]
fn a_prepended_module_is_named() {
    let pre = "module PreConcern\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def gen(k)\n      test(\"pre #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\nend\n";
    let test = r##"class PrependTest < ActiveSupport::TestCase
  class << self
    def gen(k)
      test("own #{k}") { assert_kind_of Symbol, k }
    end
  end

  prepend PreConcern

  gen(:z)

  test "kept" do
    assert true
  end
end
"##;
    let native = matches_native(
        &[("app/models/pre_concern.rb", pre), ("test/models/prepend_test.rb", test)],
        &["PrependTest"],
        &|id| id == "PrependTest#test_pre_z",
        &[&format!("{}a module the class body prepends (line 8) comes before its own methods", call_gap("gen", "PrependTest", 10))],
    );
    assert_eq!(native.keys().collect::<Vec<_>>(), ["PrependTest#test_kept", "PrependTest#test_pre_z"]);
}

/// `alias_method` after a def makes the name run the aliased method; a
/// concern's `ClassMethods` reopened in another source (which loads after
/// it) is named.
#[test]
fn aliases_and_reopened_class_methods_are_followed_or_named() {
    let alias = r##"module AliasConcern
  extend ActiveSupport::Concern

  module ClassMethods
    def gen(k)
      test("alias #{k}") { assert_kind_of Symbol, k }
    end

    def quiet(k)
    end

    alias_method :gen, :quiet
  end
end
"##;
    let plain = "module PlainConcern\n  extend ActiveSupport::Concern\n\n  module ClassMethods\n    def pgen(k)\n    end\n  end\nend\n";
    let plain_ext = "module PlainConcern\n  module ClassMethods\n    def pgen(k)\n      test(\"reopened #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\nend\n";
    let gen_ext = "module GenConcern\n  module ClassMethods\n    def gen(k)\n    end\n  end\nend\n";
    let test = r##"class AliasTest < ActiveSupport::TestCase
  include AliasConcern

  gen(:q)

  test "alias kept" do
    assert true
  end
end

class ReopenedTest < ActiveSupport::TestCase
  include PlainConcern

  pgen(:r)
end

class LateReopenTest < ActiveSupport::TestCase
  include GenConcern

  gen(:a)

  test "kept" do
    assert true
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/alias_concern.rb", alias),
            ("app/models/gen_concern.rb", GEN_CONCERN),
            ("app/models/gen_concern_ext.rb", gen_ext),
            ("app/models/plain_concern.rb", plain),
            ("app/models/plain_concern_ext.rb", plain_ext),
            ("test/models/alias_test.rb", test),
        ],
        &["AliasTest", "ReopenedTest", "LateReopenTest"],
        &|id| id == "ReopenedTest#test_reopened_r",
        &[
            &format!("{}the module that defines it is not known (", call_gap("pgen", "ReopenedTest", 14)),
            &format!("{}the module that defines it is not known (", call_gap("gen", "LateReopenTest", 20)),
        ],
    );
    assert_eq!(
        native.keys().collect::<Vec<_>>(),
        ["AliasTest#test_alias_kept", "LateReopenTest#test_kept", "ReopenedTest#test_reopened_r"]
    );
}

/// A concern the superclass already includes is not included again
/// (`append_features` returns false): the call runs what the superclass
/// gives, so it is named.
#[test]
fn a_concern_the_superclass_has_is_named() {
    let test = r##"class AParentTest < ActiveSupport::TestCase
  include GenConcern

  def self.gen(k)
  end

  test "parent" do
    assert true
  end
end

class BChildTest < AParentTest
  include GenConcern

  gen(:c)
end
"##;
    let native = matches_native(
        &[("app/models/gen_concern.rb", GEN_CONCERN), ("test/models/a_parent_test.rb", test)],
        &["AParentTest", "BChildTest"],
        &|_| false,
        &[&format!("{}`GenConcern` may already be an ancestor of the class (", call_gap("gen", "BChildTest", 15))],
    );
    assert_eq!(native.keys().collect::<Vec<_>>(), ["AParentTest#test_parent", "BChildTest#test_parent"]);
}

/// A macro called inside a loop or a condition, or by a plain method the
/// class calls, is named; the class's direct calls are expanded.
#[test]
fn macro_calls_not_made_by_the_class_body_are_named() {
    let ind = r##"module IndConcern
  extend ActiveSupport::Concern

  class_methods do
    def gen(k)
      test("ind #{k}") { assert_kind_of Symbol, k }
    end

    def gen_twice(k)
      gen(k)
      gen(:"#{k}2")
    end
  end
end
"##;
    let test = r##"class LoopConcernTest < ActiveSupport::TestCase
  include GenConcern

  [:d, :e].each { |k| gen(k) }
  gen(:f) if true
  gen(:g)
end

class IndirectTest < ActiveSupport::TestCase
  include IndConcern

  gen_twice(:h)
  gen(:i)
end
"##;
    let native = matches_native(
        &[
            ("app/models/gen_concern.rb", GEN_CONCERN),
            ("app/models/ind_concern.rb", ind),
            ("test/models/loop_concern_test.rb", test),
        ],
        &["LoopConcernTest", "IndirectTest"],
        &|id| !id.ends_with("_g") && !id.ends_with("_i"),
        &[
            &format!("{}it is called inside `[:d, :e].each {{ |k| gen(k) }}`, which is not expanded", call_gap("gen", "LoopConcernTest", 4)),
            &format!("{}it is called inside `gen(:f) if true`, which is not expanded", call_gap("gen", "LoopConcernTest", 5)),
            &format!("{}the method it runs, `IndConcern`'s, declares no test itself but calls `gen`, which may declare tests)", call_gap("gen_twice", "IndirectTest", 12)),
        ],
    );
    assert_eq!(native.len(), 7, "{native:#?}");
}

/// A load-time change through a part of a constant (`first`, `to_a`,
/// `to_h`, a block call's result, an unfrozen String another constant
/// holds) is a gap for a loop over it, and so is any read of a mutable
/// element other than as the loop, a test body's too; a constant frozen
/// through and through may be read anywhere.
#[test]
fn changes_through_parts_of_a_constant_are_gaps() {
    let levels = "# frozen_string_literal: true\n\nmodule Levels\n  NAMES = [[:x], [:y]].freeze\n  ALL = [:a, :b]\n  MAP = { k: 1 }\n  PICKED = [[:p], [:q]].freeze\n  KEPT = [[:m, :n]].freeze\n  FROZEN = [[:m, :n].freeze].freeze\nend\n";
    let setup = r##"first = Levels::NAMES.first
first << :z
list = Levels::ALL.to_a
list << :c
h = Levels::MAP.to_h
h[:j] = 2
picked = Levels::PICKED.select { |p| p.first == :p }
picked.first << :"!"
seed = Held::SEEDS.first
seed << "!"
"##;
    let test = r##"class PartsTest < ActiveSupport::TestCase
  Levels::NAMES.each do |n|
    test "name #{n.join}" do
      assert_kind_of Symbol, n.first
    end
  end

  Levels::ALL.each do |l|
    test "level #{l}" do
      assert_kind_of Symbol, l
    end
  end

  Levels::MAP.each do |k, v|
    test "map #{k}" do
      assert_kind_of Integer, v
    end
  end

  Levels::PICKED.each do |p|
    test "picked #{p.join}" do
      assert_kind_of Symbol, p.first
    end
  end

  Levels::KEPT.each do |pair|
    test "kept #{pair.join}" do
      assert_same Levels::KEPT.first, pair
      refute_predicate pair, :frozen?
    end
  end

  Levels::FROZEN.each do |pair|
    test "frozen #{pair.join}" do
      assert_same Levels::FROZEN.first, pair
      assert_predicate pair, :frozen?
    end
  end

  setup do
    @pair = Levels::FROZEN.first
  end

  def test_reads_a_part
    assert_same @pair, Levels::FROZEN[0]
  end

  Held::SEEDS.each do |seed|
    test "seed #{seed}" do
      assert_kind_of String, seed
    end
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/held.rb", "module Held\n  ADD = \"add\"\n  SEEDS = [ADD].freeze\nend\n"),
            ("app/models/levels.rb", levels),
            ("app/models/levels_setup.rb", setup),
            ("test/models/parts_test.rb", test),
        ],
        &["PartsTest"],
        &|id| !id.contains("#test_frozen_") && !id.ends_with("#test_reads_a_part"),
        &[
            &format!("{USED} (`PartsTest` line 2, first test at line 3: `Levels::NAMES`: `first = Levels::NAMES.first` (app/models/levels_setup.rb line 1))"),
            &format!("{USED} (`PartsTest` line 8, first test at line 9: `Levels::ALL`: `list = Levels::ALL.to_a` (app/models/levels_setup.rb line 3))"),
            &format!("{USED} (`PartsTest` line 14, first test at line 15: `Levels::MAP`: `h = Levels::MAP.to_h` (app/models/levels_setup.rb line 5))"),
            &format!("{USED} (`PartsTest` line 20, first test at line 21: `Levels::PICKED`: `picked = Levels::PICKED.select {{ |p| p.first == :p }}` (app/models/levels_setup.rb line 7))"),
            &format!("{USED} (`PartsTest` line 26, first test at line 27: `Levels::KEPT`: `assert_same Levels::KEPT.first, pair` (test/models/parts_test.rb line 28))"),
            // `[ADD]` holds an unfrozen String.
            &format!("{NOT_LITERAL} (`PartsTest` line 48, first test at line 49: `Held::ADD`: `ADD = \"add\"` (app/models/held.rb line 2))"),
        ],
    );
    for id in ["PartsTest#test_name_xz", "PartsTest#test_level_c", "PartsTest#test_map_j", "PartsTest#test_picked_p!", "PartsTest#test_kept_mn", "PartsTest#test_seed_add!"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// What may give a class its methods outside the body ingest reads makes
/// a macro call a named gap: `class_methods` of the concern from another
/// source, the class declared in another file.
#[test]
fn outside_changes_to_a_class_or_concern_are_named() {
    let ext = "GenConcern.class_methods do\n  def gen(k)\n  end\nend\n";
    let test = "class OutsideTest < ActiveSupport::TestCase\n  include GenConcern\n\n  gen(:a)\n\n  test \"kept\" do\n    assert true\n  end\nend\n";
    matches_native(
        &[("app/models/gen_concern.rb", GEN_CONCERN), ("app/models/gen_concern_ext.rb", ext), ("test/models/outside_test.rb", test)],
        &["OutsideTest"],
        &|id| id != "OutsideTest#test_kept",
        &[&format!(
            "{}the module that defines it is not known (`GenConcern` may be changed at `GenConcern.class_methods do` (app/models/gen_concern_ext.rb line 1)))",
            call_gap("gen", "OutsideTest", 4)
        )],
    );

    // Native: `a_reopen_test.rb`'s `def self.gen` is the class's own,
    // ahead of the concern's, and declares nothing.
    let first = "class ZReopenTest < ActiveSupport::TestCase\n  def self.gen(k)\n  end\nend\n";
    let second = "class ZReopenTest < ActiveSupport::TestCase\n  include GenConcern\n\n  gen(:z)\n\n  test \"z kept\" do\n    assert true\n  end\nend\n";
    let native = matches_native(
        &[("app/models/gen_concern.rb", GEN_CONCERN), ("test/models/a_reopen_test.rb", first), ("test/models/z_reopen_test.rb", second)],
        &["ZReopenTest"],
        &|_| false,
        &[&format!(
            "{}the class may be given methods outside this body (`ZReopenTest` is declared again in test/models/a_reopen_test.rb))",
            call_gap("gen", "ZReopenTest", 4)
        )],
    );
    assert_eq!(native.keys().collect::<Vec<_>>(), ["ZReopenTest#test_z_kept"]);
}

/// Tests a macro declares when it is called other than as a class-body
/// statement are named: from a concern's `included do` block as the
/// class includes it, through `send` of a String, through `method(:gen)`.
#[test]
fn hook_and_indirect_macro_calls_are_named() {
    let inc = "module IncGen\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def gen(k)\n      test(\"inc #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\n\n  included do\n    gen(:inc)\n  end\nend\n";
    let test = "class IncludedCallTest < ActiveSupport::TestCase\n  include IncGen\n\n  gen(:own)\nend\n";
    let native = matches_native(
        &[("app/models/inc_gen.rb", inc), ("test/models/included_call_test.rb", test)],
        &["IncludedCallTest"],
        &|id| id == "IncludedCallTest#test_inc_inc",
        &[&format!("{}a hook of `IncGen` calls it as `include IncGen` runs, which is not expanded)", call_gap("gen", "IncludedCallTest", 2))],
    );
    assert_eq!(native.len(), 2, "{native:#?}");

    let test = r##"class IndirectCallTest < ActiveSupport::TestCase
  include GenConcern

  method(:gen).call(:a)
  send("gen", :b)
  gen(:e)

  test "kept" do
    assert true
  end
end
"##;
    let native = matches_native(
        &[("app/models/gen_concern.rb", GEN_CONCERN), ("test/models/indirect_call_test.rb", test)],
        &["IndirectCallTest"],
        &|id| id.ends_with("_a") || id.ends_with("_b"),
        &[
            &format!("{}it is called inside `method(:gen).call(:a)`, which is not expanded)", call_gap("gen", "IndirectCallTest", 4)),
            &format!("{}it is called other than as `gen(…)` (`send(\"gen\", :b)`))", call_gap("gen", "IndirectCallTest", 5)),
        ],
    );
    assert_eq!(native.len(), 4, "{native:#?}");
}

/// A constant frozen through and through may be passed anywhere; one
/// whose Array is not frozen may not, and one whose elements may change
/// may be read only as the loop.
#[test]
fn only_a_deeply_frozen_constant_may_be_passed_on() {
    let passed = "# frozen_string_literal: true\n\nmodule Passed\n  DEEP = [:a, \"b\"].freeze\n  SHALLOW = [:c, :d]\n  NESTED = [[:e]].freeze\nend\n";
    let test = r##"class PassedTest < ActiveSupport::TestCase
  Passed::DEEP.each do |k|
    test "deep #{k}" do
      assert_includes Passed::DEEP, k
    end
  end

  Passed::SHALLOW.each do |k|
    test "shallow #{k}" do
      assert_includes Passed::SHALLOW, k
    end
  end

  Passed::NESTED.each do |pair|
    test "nested #{pair.join}" do
      assert_equal [:e], pair
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/passed.rb", passed), ("test/models/passed_test.rb", test)],
        &["PassedTest"],
        &|id| id.contains("#test_shallow_"),
        &[&format!(
            "{USED} (`PassedTest` line 8, first test at line 9: `Passed::SHALLOW`: `assert_includes Passed::SHALLOW, k` (test/models/passed_test.rb line 10))"
        )],
    );
    assert_eq!(native.len(), 5, "{native:#?}");
}

// ---- The allow-list: only uses listed as safe leave a loop to run ---

/// A change through any path that may reach the constant is a gap: a
/// rooted path through a class that includes its module
/// (`::Parent::MORE`), one through an alias (`::KAlias::THIRD`), a bare
/// name in a class whose ancestors have it (`ALL` in `Sub < Parent`,
/// though the file declares another `ALL`). A constant no use changes
/// is still unrolled, the alias counted by its own uses.
#[test]
fn changes_through_ancestors_aliases_and_rooted_paths_are_gaps() {
    let kinds = "module Kinds\n  ALL = [:a, :b]\n  MORE = [:m, :n]\n  THIRD = [:x, :y]\n  KEPT = [:k]\nend\n\nKAlias = Kinds\n";
    let parent = "class Parent\n  include Kinds\nend\n";
    let sub = "module Other\n  ALL = [:q]\nend\n\nclass Sub < Parent\n  ALL << :c\nend\n";
    let rooted = "::Parent::MORE << :d\n::KAlias::THIRD << :e\n";
    let test = r##"class AncTest < ActiveSupport::TestCase
  Kinds::ALL.each do |k|
    test "k #{k}" do
      assert k
    end
  end

  Kinds::MORE.each do |k|
    test "more #{k}" do
      assert k
    end
  end

  Kinds::THIRD.each do |k|
    test "third #{k}" do
      assert k
    end
  end

  Kinds::KEPT.each do |k|
    test "kept #{k}" do
      assert k
    end
  end
end
"##;
    let native = matches_native(
        &[
            ("app/models/kinds.rb", kinds),
            ("app/models/parent.rb", parent),
            ("app/models/sub.rb", sub),
            ("app/models/zz_rooted.rb", rooted),
            ("test/models/anc_test.rb", test),
        ],
        &["AncTest"],
        &|id| !id.contains("#test_kept_"),
        &[
            &format!("{USED} (`AncTest` line 2, first test at line 3: `Kinds::ALL`: `ALL << :c` (app/models/sub.rb line 6))"),
            &format!("{USED} (`AncTest` line 8, first test at line 9: `Kinds::MORE`: `::Parent::MORE << :d` (app/models/zz_rooted.rb line 1))"),
            &format!("{USED} (`AncTest` line 14, first test at line 15: `Kinds::THIRD`: `::KAlias::THIRD << :e` (app/models/zz_rooted.rb line 2))"),
        ],
    );
    for id in ["AncTest#test_k_c", "AncTest#test_more_d", "AncTest#test_third_e", "AncTest#test_kept_k"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// `Module#constants` is unrolled only for a module whose one body
/// declares constants by statements listed as adding no other, and whose
/// every use elsewhere is listed as only reading it (`include`, a method
/// of its own body that adds none). Any other use is a gap: a compact
/// `class Mod::X` (in its own file too), `module Mod::X` in a test
/// helper, `Mod::X ||= …`, `Mod::X, Mod::Y = …`, `Mod.autoload`, the
/// module kept in a data structure.
#[test]
fn constants_added_from_outside_a_module_body_are_gaps() {
    let modules = [
        ("app/models/kinds.rb", "module Kinds\n  A = 1\nend\n\nclass Kinds::SameFile\nend\n"),
        ("app/models/wide.rb", "module Wide\n  W1 = 1\nend\n"),
        ("app/models/fakes.rb", "module Fakes\n  F1 = 1\nend\n"),
        ("app/models/modes.rb", "module Modes\n  M1 = 1\nend\n"),
        ("app/models/levels.rb", "module Levels\n  L1 = 1\nend\n"),
        ("app/models/lazy.rb", "module Lazy\n  Z1 = 1\nend\n"),
        ("app/models/held.rb", "module Held\n  H1 = 1\nend\n"),
        ("app/models/plain.rb", "module Plain\n  P1 = :p1\n  P2 = :p2\n\n  class << self\n    def all\n      [P1, P2]\n    end\n  end\nend\n"),
        ("app/models/plain_user.rb", "class PlainUser\n  include Plain\n  LIST = Plain.all\nend\n"),
        ("app/models/zz_extra.rb", "class Wide::Extra\nend\n"),
        ("app/models/zz_modes.rb", "Modes::M2 ||= 2\n"),
        ("app/models/zz_levels.rb", "Levels::L2, Levels::L3 = 2, 3\n"),
        ("app/models/zz_lazy.rb", "Lazy.autoload(:Later, \"/nonexistent/later\")\n"),
        ("test/support/fakes_fake.rb", "module Fakes::Fake\nend\n"),
    ];
    let names = ["Kinds", "Wide", "Fakes", "Modes", "Levels", "Lazy", "Held", "Plain"];
    let mut test = String::new();
    for name in names {
        test.push_str(&format!(
            "class {name}ConstsTest < ActiveSupport::TestCase\n  HOLDERS = [{{ mod: Held }}.freeze].freeze\n\n  {name}.constants.each do |c|\n    test \"const #{{c}}\" do\n      assert c\n    end\n  end\nend\n\n"
        ));
    }
    let mut files: Vec<(&str, &str)> = modules.to_vec();
    files.push(("test/models/consts_test.rb", &test));
    let classes: Vec<String> = names.iter().map(|n| format!("{n}ConstsTest")).collect();
    let at = |name: &str| names.iter().position(|n| *n == name).unwrap() * 10 + 4;
    let gap = |name: &str, site: &str| {
        format!("{REOPENED} (`{name}ConstsTest` line {}, first test at line {}: `{name}.constants`: `{name}` may be changed at {site})", at(name), at(name) + 1)
    };
    let native = matches_native(
        &files,
        &classes.iter().map(String::as_str).collect::<Vec<_>>(),
        &|id| !id.starts_with("PlainConstsTest#"),
        &[
            &gap("Kinds", "`class Kinds::SameFile` (app/models/kinds.rb line 5)"),
            &gap("Wide", "`class Wide::Extra` (app/models/zz_extra.rb line 1)"),
            &gap("Fakes", "`module Fakes::Fake` (test/support/fakes_fake.rb line 1, a test source not registered before this one)"),
            &gap("Modes", "`Modes::M2 ||= 2` (app/models/zz_modes.rb line 1)"),
            &gap("Levels", "`Levels::L2, Levels::L3 = 2, 3` (app/models/zz_levels.rb line 1)"),
            &gap("Lazy", "`Lazy.autoload(:Later, \"/nonexistent/later\")` (app/models/zz_lazy.rb line 1)"),
            &gap("Held", "`HOLDERS = [{ mod: Held }.freeze].freeze` (test/models/consts_test.rb line 2)"),
        ],
    );
    for id in [
        "KindsConstsTest#test_const_SameFile",
        "WideConstsTest#test_const_Extra",
        "FakesConstsTest#test_const_Fake",
        "ModesConstsTest#test_const_M2",
        "LevelsConstsTest#test_const_L3",
        "LazyConstsTest#test_const_Later",
        "PlainConstsTest#test_const_P2",
    ] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// A macro module reopened through an alias (`module GM` of `GM =
/// GenMod`, `GCC.class_methods do` of `GCC = GenConcern`) may define
/// other methods than its body shows: the calls are gaps.
#[test]
fn a_macro_module_reopened_through_an_alias_is_named() {
    let gen_mod = "module GenMod\n  def gen(k)\n    test(\"gen #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n";
    let gen_concern = "module GenConcern\n  extend ActiveSupport::Concern\n\n  class_methods do\n    def cgen(k)\n      test(\"cgen #{k}\") { assert_kind_of Symbol, k }\n    end\n  end\nend\n";
    let aliases = "GM = GenMod\nmodule GM\n  def gen(k)\n    test(\"redef #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n\nGCC = GenConcern\nGCC.class_methods do\n  def cgen(k)\n    test(\"credef #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n";
    let test = "class AliasModTest < ActiveSupport::TestCase\n  extend GenMod\n\n  gen(:a)\n\n  test \"kept\" do\n    assert true\n  end\nend\n\nclass AliasConcernTest < ActiveSupport::TestCase\n  include GenConcern\n\n  cgen(:b)\nend\n";
    let native = matches_native(
        &[
            ("app/models/gen_mod.rb", gen_mod),
            ("app/models/gen_concern.rb", gen_concern),
            ("app/models/zz_aliases.rb", aliases),
            ("test/models/alias_reopen_test.rb", test),
        ],
        &["AliasModTest", "AliasConcernTest"],
        &|id| id != "AliasModTest#test_kept",
        &[
            &format!("{}the module that defines it is not known (`GenMod` may be changed at `GM = GenMod` (app/models/zz_aliases.rb line 1)))", call_gap("gen", "AliasModTest", 4)),
            &format!("{}the module that defines it is not known (`GenConcern` may be changed at `GCC = GenConcern` (app/models/zz_aliases.rb line 8)))", call_gap("cgen", "AliasConcernTest", 14)),
        ],
    );
    for id in ["AliasModTest#test_redef_a", "AliasConcernTest#test_credef_b"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// A constant bound by a multiple assignment (`X, ALL = …`, `Y,
/// Kinds::MORE = …` in another module) is not assigned once with
/// certainty.
#[test]
fn a_constant_bound_by_multiple_assignment_is_named() {
    let kinds = "module Kinds\n  ALL = [:a, :b].freeze\n  X, ALL = 1, [:z].freeze\n  MORE = [:m].freeze\n  SAFE = [:s].freeze\nend\n";
    let rebind = "module Rebind\n  Y, Kinds::MORE = 1, [:n].freeze\nend\n";
    let test = r##"class MultiTest < ActiveSupport::TestCase
  Kinds::ALL.each do |k|
    test "all #{k}" do
      assert k
    end
  end

  Kinds::MORE.each do |k|
    test "more #{k}" do
      assert k
    end
  end

  Kinds::SAFE.each do |k|
    test "safe #{k}" do
      assert k
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/rebind.rb", rebind), ("test/models/multi_test.rb", test)],
        &["MultiTest"],
        &|id| !id.contains("#test_safe_"),
        &[
            &format!("{UNCERTAIN} (`MultiTest` line 2, first test at line 3: `Kinds::ALL` is not assigned once, unconditionally, by `Kinds::ALL = …`)"),
            &format!("{UNCERTAIN} (`MultiTest` line 8, first test at line 9: `Kinds::MORE` is not assigned once, unconditionally, by `Kinds::MORE = …`)"),
        ],
    );
    for id in ["MultiTest#test_all_z", "MultiTest#test_more_n", "MultiTest#test_safe_s"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// A String a module evaluates (`module_eval("…")`, a heredoc) that has
/// the constant's name, and a path through a local (`ref::ALL`), may
/// change it.
#[test]
fn a_constant_named_in_an_evaluated_string_is_named() {
    let kinds = "module Kinds\n  ALL = [:a, :b]\n  MORE = [:m]\n  EVALED = [:e]\nend\nKinds.module_eval(\"EVALED << :f\")\nKinds.module_eval <<~RUBY\n  MORE << :n\nRUBY\nref = Kinds\nref::ALL << :c\n";
    let test = r##"class MetaTest < ActiveSupport::TestCase
  Kinds::ALL.each do |k|
    test "all #{k}" do
      assert k
    end
  end

  Kinds::MORE.each do |k|
    test "more #{k}" do
      assert k
    end
  end

  Kinds::EVALED.each do |k|
    test "evaled #{k}" do
      assert k
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("test/models/meta_test.rb", test)],
        &["MetaTest"],
        &|_| true,
        &[
            &format!("{USED} (`MetaTest` line 2, first test at line 3: `Kinds::ALL`: `ref::ALL << :c` (app/models/kinds.rb line 11))"),
            &format!("{USED} (`MetaTest` line 8, first test at line 9: `Kinds::MORE`: `Kinds.module_eval <<~RUBY` (app/models/kinds.rb line 7))"),
            &format!("{USED} (`MetaTest` line 14, first test at line 15: `Kinds::EVALED`: `Kinds.module_eval(\"EVALED << :f\")` (app/models/kinds.rb line 6))"),
        ],
    );
    for id in ["MetaTest#test_all_c", "MetaTest#test_more_n", "MetaTest#test_evaled_f"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// `define_method("test_…")` defines a test Minitest runs: in the class
/// body, in a loop, or in a macro, it is a named gap, not dropped.
#[test]
fn define_method_tests_are_named() {
    let direct = r##"class DmDirectTest < ActiveSupport::TestCase
  define_method("test_direct") do
    assert true
  end

  %i[a b].each do |n|
    define_method("test_loop_#{n}") do
      assert true
    end
  end

  test "plain" do
    assert true
  end
end
"##;
    let macro_test = r##"class DmMacroTest < ActiveSupport::TestCase
  def self.via_define(n)
    define_method("test_dm_#{n}") do
      assert true
    end
  end

  via_define :b

  test "plain" do
    assert true
  end
end
"##;
    let singleton = r##"class DmSingletonTest < ActiveSupport::TestCase
  class << self
    def via_define(n)
      define_method("test_dm_#{n}") do
        assert true
      end
    end
  end

  via_define :c

  test "plain" do
    assert true
  end
end
"##;
    let native = matches_native(
        &[("test/models/dm_direct_test.rb", direct), ("test/models/dm_macro_test.rb", macro_test), ("test/models/dm_singleton_test.rb", singleton)],
        &["DmDirectTest", "DmMacroTest", "DmSingletonTest"],
        &|id| !id.ends_with("#test_plain"),
        &[
            "tests declared by `define_method` of a test are not modeled; they will not run (`DmDirectTest` line 2: define_method(\"test_direct\") do)",
            "(`DmDirectTest` line 6, first test at line 7: ",
            "(`DmMacroTest` line 8: ",
            "(`DmSingletonTest` line 10: ",
        ],
    );
    for id in ["DmDirectTest#test_direct", "DmDirectTest#test_loop_b", "DmMacroTest#test_dm_b", "DmSingletonTest#test_dm_c"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// A path through a name no source declares is a gem's: it reaches an
/// app constant only through a module a source mixes into a gem's
/// (`Minitest::Test.include(ModesHelper)` gives every test class
/// `ModesHelper::MODES`), and otherwise names none of the app's
/// (`FixtureFactory::Registry::ClassMethods` is not `GemPathTest::ClassMethods`).
#[test]
fn paths_through_gem_modules_reach_what_a_source_mixes_in() {
    let modes = "MODES = [:top].freeze\n";
    let helper = "module ModesHelper\n  MODES = [:helper].freeze\nend\n\nMinitest::Test.include(ModesHelper)\n";
    let test = "class MtModesTest < ActiveSupport::TestCase\n  MODES.each do |m|\n    test \"mode #{m}\" do\n      assert_equal :helper, m\n    end\n  end\nend\n";
    let native = matches_native(
        &[("app/models/modes.rb", modes), ("app/models/modes_helper.rb", helper), ("test/models/mt_modes_test.rb", test)],
        &["MtModesTest"],
        &|_| true,
        &["tests declared by a loop over a constant an ancestor of its class may declare are not modeled; they will not run (`MtModesTest` line 2, first test at line 3: `MODES`, looked up past the ancestors of `MtModesTest`: `Minitest::Test` may be changed at `Minitest::Test.include(ModesHelper)` (app/models/modes_helper.rb line 5))"],
    );
    assert!(native.contains_key("MtModesTest#test_mode_helper"), "{native:#?}");

    let gen_mod = "module GenMod\n  def gen(k)\n    test(\"gen #{k}\") { assert_kind_of Symbol, k }\n  end\nend\n";
    let registry = "module Registered\n  def self.registry\n    FixtureFactory::Registry::ClassMethods\n  end\nend\n";
    let test = "class GemPathTest < ActiveSupport::TestCase\n  extend GenMod\n\n  gen(:a)\nend\n";
    let native = matches_native(
        &[("app/models/gen_mod.rb", gen_mod), ("app/models/registered.rb", registry), ("test/models/gem_path_test.rb", test)],
        &["GemPathTest"],
        &|_| false,
        &[],
    );
    assert!(native.contains_key("GemPathTest#test_gen_a"), "{native:#?}");
}

/// A test class one file declares by two class bodies is one class
/// whose body is split: it is named, not emitted as two test files (the
/// second without its superclass or the first's methods).
#[test]
fn a_test_class_reopened_in_its_file_is_named() {
    let test = "class SameFileTest < ActiveSupport::TestCase\n  def helper_value\n    :h\n  end\nend\n\nclass SameFileTest\n  test \"same kept\" do\n    assert_equal :h, helper_value\n  end\nend\n\nclass OtherTest < ActiveSupport::TestCase\n  test \"other\" do\n    assert true\n  end\nend\n";
    let native = matches_native(
        &[("test/models/same_file_test.rb", test)],
        &["SameFileTest", "OtherTest"],
        &|id| id.starts_with("SameFileTest#"),
        &["test class `SameFileTest` refused: it is declared by more than one class body in this file, which is not modeled; its tests would not run (`SameFileTest`, lines 1, 7)"],
    );
    assert!(native.contains_key("SameFileTest#test_same_kept"), "{native:#?}");
}

/// A test file of one loop over each of `modules`' `constants`
/// (`{M}Test`, 8 lines each: the loop at line 8k + 2, its test at 8k +
/// 3) and, when `kinds` is given, a loop over `Kinds::ALL` after them.
fn module_loops(modules: &[&str], kinds: bool) -> String {
    let mut test = String::new();
    for m in modules {
        test.push_str(&format!(
            "class {m}Test < ActiveSupport::TestCase\n  {m}.constants.each do |c|\n    test \"c #{{c}}\" do\n      assert c\n    end\n  end\nend\n\n"
        ));
    }
    if kinds {
        test.push_str("class KindsTest < ActiveSupport::TestCase\n  Kinds::ALL.each do |k|\n    test \"k #{k}\" do\n      assert_kind_of Symbol, k\n    end\n  end\nend\n");
    }
    test
}

/// `{kind} (`{M}Test` line …: `{M}.constants`: {why})` for the `k`th
/// loop of `module_loops`.
fn module_gap(kind: &str, k: usize, module: &str, why: &str) -> String {
    format!("{kind} (`{module}Test` line {}, first test at line {}: `{module}.constants`: {why})", 8 * k + 2, 8 * k + 3)
}

/// `Mod.constants` is a method call: a module that defines `constants`
/// for itself (`def self.constants`, in `class << self`, under `extend
/// self` or `module_function`, `define_singleton_method`) answers with
/// what that returns, not with what its body declares. Each is a gap;
/// and since one such definition may reach any module (through an
/// `extend` of a module found by name), so is every other module's loop
/// in the program. A loop over a frozen literal reads no method.
#[test]
fn constants_defined_again_in_a_module_body_are_gaps() {
    let modules = r##"module M6
  A = 1
  def self.constants(inherit = true)
    super + [:VIRTUAL]
  end
end

module M7
  A = 1
  class << self
    def constants(*)
      [:ONLY]
    end
  end
end

module M8
  extend self
  A = 1
  def constants(*)
    [:FAKE]
  end
end

module M9
  A = 1
  module_function

  def constants(*)
    [:MF]
  end
end

module M11
  A = 1
  module_function def constants(*) = [:MFD]
end

module M12
  A = 1
  define_singleton_method(:constants) { |*| [:DSM] }
end

module M13
  A = 1
end

module Kinds
  ALL = %i[a b].freeze
end
"##;
    let names = ["M6", "M7", "M8", "M9", "M11", "M12", "M13"];
    let test = module_loops(&names, true);
    let classes: Vec<String> = names.iter().map(|n| format!("{n}Test")).chain(["KindsTest".to_string()]).collect();
    let defines = |k: usize, m: &str, line: u32| {
        module_gap(MODULE_BODY, k, m, &format!("its body defines `constants` (line {line}), which `Module#constants` answers with"))
    };
    let native = matches_native(
        &[("app/models/ov.rb", modules), ("test/models/m_test.rb", &test)],
        &classes.iter().map(String::as_str).collect::<Vec<_>>(),
        &|id| !id.starts_with("KindsTest#"),
        &[
            &defines(0, "M6", 3),
            &defines(1, "M7", 11),
            &defines(2, "M8", 20),
            &defines(3, "M9", 29),
            &defines(4, "M11", 36),
            &module_gap(MODULE_BODY, 5, "M12", "its body runs `define_singleton_method(:constants) { |*| [:DSM] }` (line 41), not listed as adding no constant"),
            &module_gap(REOPENED, 6, "M13", "`constants` may be defined again at `def self.constants(inherit = true)` (app/models/ov.rb line 3)"),
        ],
    );
    for id in ["M6Test#test_c_VIRTUAL", "M7Test#test_c_ONLY", "M8Test#test_c_FAKE", "M9Test#test_c_MF", "M11Test#test_c_MFD", "M12Test#test_c_DSM", "KindsTest#test_k_b"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// `constants` defined again outside the module's body, for every module
/// (`class Module` reopened) or for one found by a name not known before
/// it runs (`name.constantize.extend(ConstPatch)`), or named for a
/// `define_method`/`delegate`: every `Mod.constants` loop is a gap,
/// while `Module` reopened only to wrap `autoload` (Core's boot
/// instrumentation) and a loop over a frozen literal are not.
#[test]
fn constants_defined_again_anywhere_are_gaps() {
    let kinds = "module Global\n  A = 1\nend\n\nmodule Kinds\n  ALL = %i[a b].freeze\nend\n";
    let reopen = r##"class Module
  alias_method :constants_before_extra, :constants

  def constants(inherit = true)
    constants_before_extra(inherit) + (name == "Global" ? [:EXTRA] : [])
  end
end
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_module.rb", reopen), ("test/models/m_test.rb", &module_loops(&["Global"], true))],
        &["GlobalTest", "KindsTest"],
        &|id| id.starts_with("GlobalTest#"),
        &[&module_gap(REOPENED, 0, "Global", "`constants` may be defined again at `def constants(inherit = true)` (app/models/zz_module.rb line 4)")],
    );
    assert!(native.contains_key("GlobalTest#test_c_EXTRA"), "{native:#?}");

    let kinds = "module M14\n  A = 1\nend\n\nmodule Kinds\n  ALL = %i[a b].freeze\nend\n";
    let patch = r##"module ConstPatch
  def constants(inherit = true)
    super + [:PATCHED]
  end
end

["M", "14"].join.constantize.extend(ConstPatch)
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_patch.rb", patch), ("test/models/m_test.rb", &module_loops(&["M14"], true))],
        &["M14Test", "KindsTest"],
        &|id| id.starts_with("M14Test#"),
        &[&module_gap(REOPENED, 0, "M14", "`constants` may be defined again at `def constants(inherit = true)` (app/models/zz_patch.rb line 2)")],
    );
    assert!(native.contains_key("M14Test#test_c_PATCHED"), "{native:#?}");

    let kinds = "module M15\n  A = 1\nend\n\nmodule Kinds\n  ALL = %i[a b].freeze\nend\n";
    let patch = r##"module Patch2
end

module Fwd
  delegate :constants, to: :Patch2
end

["M", "15"].join.constantize.extend(Fwd)
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_patch.rb", patch), ("test/models/m_test.rb", &module_loops(&["M15"], true))],
        &["M15Test", "KindsTest"],
        &|id| id.starts_with("M15Test#"),
        &[&module_gap(REOPENED, 0, "M15", "`constants` may be defined again at `delegate :constants, to: :Patch2` (app/models/zz_patch.rb line 5)")],
    );
    assert!(!native.contains_key("M15Test#test_c_A"), "{native:#?}");

    // Core's `config/boot_instrumentation.rb`, and names that define nothing.
    let kinds = "module M16\n  A = 1\nend\n\nmodule Kinds\n  ALL = %i[a b].freeze\nend\n";
    let wrap = r##"class Module
  alias_method :autoload_without_wrap, :autoload

  def autoload(name, path)
    autoload_without_wrap(name, path)
  end
end

READS = { constants: 1 }.freeze
READS.fetch(:constants)
M16.respond_to?(:constants)
"##;
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_wrap.rb", wrap), ("test/models/m_test.rb", &module_loops(&["M16"], true))],
        &["M16Test", "KindsTest"],
        &|_| false,
        &[],
    );
    assert!(native.contains_key("M16Test#test_c_A"), "{native:#?}");
}

/// `"Kinds::ALL".constantize` (`safe_constantize`,
/// `ActiveSupport::Inflector.constantize("…")`) is `Object.const_get` of
/// the path: a change through it is a change of the constant it names,
/// as through `Object.const_get(:K17)`; a read through it is not.
#[test]
fn changes_through_a_literal_constantize_are_gaps() {
    let kinds = r##"# frozen_string_literal: true

module K10
  TEN = %i[a b]
end

module K12
  A = 1
end

module K14
  FOURTEEN = %i[a b]
end

module K15
  FIFTEEN = %i[a b]
end

module K17
  A = 1
end

module K18
  EIGHTEEN = %i[a b]
end

module K19
  A = 1
end
"##;
    let changes = r##"# frozen_string_literal: true

"K10::TEN".constantize << :c
"K12".constantize.const_set(:B, 2)
ActiveSupport::Inflector.constantize("K14::FOURTEEN") << :c
"::K15::FIFTEEN".safe_constantize.push(:c)
Object.const_get(:K17).const_set(:B, 2)
"K18::EIGHTEEN".constantize.include?(:a)
"K19".constantize.constants
"##;
    let lists = [("K10", "TEN"), ("K14", "FOURTEEN"), ("K15", "FIFTEEN"), ("K18", "EIGHTEEN")];
    let mut test = module_loops(&["K12", "K17", "K19"], false);
    for (m, c) in lists {
        test.push_str(&format!(
            "class {m}Test < ActiveSupport::TestCase\n  {m}::{c}.each do |k|\n    test \"k #{{k}}\" do\n      assert_kind_of Symbol, k\n    end\n  end\nend\n\n"
        ));
    }
    let classes = ["K12Test", "K17Test", "K19Test", "K10Test", "K14Test", "K15Test", "K18Test"];
    let used = |k: usize, full: &str, line: u32, text: &str| {
        let m = full.split("::").next().unwrap();
        format!("{USED} (`{m}Test` line {}, first test at line {}: `{full}`: `{text}` (app/models/zz_mut.rb line {line}))", 8 * k + 2, 8 * k + 3)
    };
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_mut.rb", changes), ("test/models/k_test.rb", &test)],
        &classes,
        &|id| !(id.starts_with("K18Test#") || id.starts_with("K19Test#")),
        &[
            &module_gap(REOPENED, 0, "K12", "`K12` may be changed at `\"K12\".constantize.const_set(:B, 2)` (app/models/zz_mut.rb line 4)"),
            &module_gap(REOPENED, 1, "K17", "`K17` may be changed at `Object.const_get(:K17).const_set(:B, 2)` (app/models/zz_mut.rb line 7)"),
            &used(3, "K10::TEN", 3, "\"K10::TEN\".constantize << :c"),
            &used(4, "K14::FOURTEEN", 5, "ActiveSupport::Inflector.constantize(\"K14::FOURTEEN\") << :c"),
            &used(5, "K15::FIFTEEN", 6, "\"::K15::FIFTEEN\".safe_constantize.push(:c)"),
        ],
    );
    for id in ["K10Test#test_k_c", "K12Test#test_c_B", "K14Test#test_k_c", "K15Test#test_k_c", "K17Test#test_c_B", "K18Test#test_k_b"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// A nested class's superclass expression runs in the module body:
/// `class Foo < Class.new { Z = 1 }` and `class Foo < (Z = Object)` add
/// `Z`. `class Foo < Struct.new(:a)` adds nothing.
#[test]
fn a_nested_class_superclass_that_writes_a_constant_is_a_gap() {
    let modules = r##"module M1
  A = 1
  class Foo < Class.new { Z = 1 }
  end
end

module M5
  A = 1
  class Foo < (Z = Object)
  end
end

module M31
  A = 1
  class Foo < Struct.new(:a)
  end
end
"##;
    let runs = |k: usize, m: &str, text: &str, line: u32| {
        module_gap(MODULE_BODY, k, m, &format!("its body runs `{text}` (line {line}), not listed as adding no constant"))
    };
    let native = matches_native(
        &[("app/models/mods.rb", modules), ("test/models/m_test.rb", &module_loops(&["M1", "M5", "M31"], false))],
        &["M1Test", "M5Test", "M31Test"],
        &|id| !id.starts_with("M31Test#"),
        &[&runs(0, "M1", "class Foo < Class.new { Z = 1 }", 3), &runs(1, "M5", "class Foo < (Z = Object)", 9)],
    );
    for id in ["M1Test#test_c_Z", "M5Test#test_c_Z", "M31Test#test_c_Foo"] {
        assert!(native.contains_key(id), "{id}: {native:#?}");
    }
}

/// Code that looks a constant up by a name not known before it runs
/// (`Object.const_get(name)`, `name.constantize`) may find any constant:
/// a change it makes to what it finds is a gap for each loop whose shape
/// allows no such change. `<<` on it: an unfrozen collection's loop
/// (a frozen literal's is not); `const_set` on it: a module's
/// `constants` loop, and a loop over a constant of the name it sets;
/// `<<` on its `first`: a loop over nested collections. Reads (`each`
/// that only reads, `include?`, `constants`) are not. What it is passed
/// to, returns or keeps is not followed.
#[test]
fn changes_to_a_constant_found_by_a_computed_name_are_gaps() {
    let kinds = "# frozen_string_literal: true\n\nmodule M50\n  A = 1\nend\n\nmodule Kinds\n  ALL = %i[a b]\n  NESTED = [[:a], [:b]].freeze\n  FROZEN = %i[a b].freeze\nend\n";
    let mut test = module_loops(&["M50"], false);
    for (name, c, k, body) in [
        ("All", "ALL", "k", "assert_kind_of Symbol, k"),
        ("Nested", "NESTED", "k.join", "assert_kind_of Array, k"),
        ("Frozen", "FROZEN", "k", "assert_kind_of Symbol, k"),
    ] {
        test.push_str(&format!(
            "class {name}Test < ActiveSupport::TestCase\n  Kinds::{c}.each do |k|\n    test \"k #{{{k}}}\" do\n      {body}\n    end\n  end\nend\n\n"
        ));
    }
    let classes = ["M50Test", "AllTest", "NestedTest", "FrozenTest"];
    let found = |class: &str, k: usize, full: &str, text: &str, line: u32| {
        format!(
            "{USED} (`{class}` line {}, first test at line {}: `{full}`: a constant looked up by a name not known before it runs is changed at `{text}` (app/models/zz_mut.rb line {line}))",
            8 * k + 2,
            8 * k + 3
        )
    };

    let pushes = "# frozen_string_literal: true\n\nname = [\"Kinds\", \"ALL\"].join(\"::\")\nObject.const_get(name) << :c\n";
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_mut.rb", pushes), ("test/models/k_test.rb", &test)],
        &classes,
        &|id| id.starts_with("AllTest#") || id.starts_with("NestedTest#"),
        &[
            &found("AllTest", 1, "Kinds::ALL", "Object.const_get(name) << :c", 4),
            &found("NestedTest", 2, "Kinds::NESTED", "Object.const_get(name) << :c", 4),
        ],
    );
    assert!(native.contains_key("AllTest#test_k_c"), "{native:#?}");

    let sets = "# frozen_string_literal: true\n\nmod = [\"M\", \"50\"].join\nmod.safe_constantize.const_set(:B, 2)\n";
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_mut.rb", sets), ("test/models/k_test.rb", &test)],
        &classes,
        &|id| id.starts_with("M50Test#"),
        &[&module_gap(
            REOPENED,
            0,
            "M50",
            "a module looked up by a name not known before it runs is changed at `mod.safe_constantize.const_set(:B, 2)` (app/models/zz_mut.rb line 4)",
        )],
    );
    assert!(native.contains_key("M50Test#test_c_B"), "{native:#?}");

    let parts = "# frozen_string_literal: true\n\nlist = [\"Kinds\", \"NESTED\"].join(\"::\").constantize\nlist.first << :c\nObject.const_get(\"Kinds::#{:ALL}\").each { |k| k.to_s }\n[\"M\", \"50\"].join.constantize.constants\n";
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_mut.rb", parts), ("test/models/k_test.rb", &test)],
        &classes,
        &|id| id.starts_with("NestedTest#"),
        &[&found("NestedTest", 2, "Kinds::NESTED", "list = [\"Kinds\", \"NESTED\"].join(\"::\").constantize", 3)],
    );
    assert!(native.contains_key("NestedTest#test_k_ac"), "{native:#?}");

    // A constant of one name bound again (a mention of its name, as
    // `const_set(:FROZEN)` on any module is): loops over constants of
    // other names are not changed.
    let rebinds = "# frozen_string_literal: true\n\nmod = [\"Kin\", \"ds\"].join\nmod.constantize.const_set(:FROZEN, %i[x].freeze)\n";
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/zz_mut.rb", rebinds), ("test/models/k_test.rb", &test)],
        &classes,
        &|id| id.starts_with("M50Test#") || id.starts_with("FrozenTest#"),
        &[
            &module_gap(
                REOPENED,
                0,
                "M50",
                "a module looked up by a name not known before it runs is changed at `mod.constantize.const_set(:FROZEN, %i[x].freeze)` (app/models/zz_mut.rb line 4)",
            ),
            &format!("{USED} (`FrozenTest` line 26, first test at line 27: `Kinds::FROZEN`: `mod.constantize.const_set(:FROZEN, %i[x].freeze)` (app/models/zz_mut.rb line 4))"),
        ],
    );
    assert!(native.contains_key("FrozenTest#test_k_x"), "{native:#?}");
}
