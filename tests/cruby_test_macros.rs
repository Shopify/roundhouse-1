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
//!   be expanded or is registered later).
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
  LIST = %w[p q]
  LIST.concat(%w[r])
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
    let changed = "tests declared by a loop over a constant that is changed after it is assigned are not modeled; they will not run";
    let native = matches_native(
        &[("app/models/kinds.rb", kinds), ("app/models/flags.rb", flags), ("test/models/mutated_const_test.rb", test)],
        &["MutatedConstTest"],
        &|id| !id.ends_with("#test_fixed_z"),
        &[
            &format!("{changed} (`MutatedConstTest` line 2, first test at line 3: `Kinds::ALL`: "),
            &format!("{changed} (`MutatedConstTest` line 8, first test at line 9: `Kinds::NAMES`: "),
            &format!("{changed} (`MutatedConstTest` line 14, first test at line 15: `Kinds::LIST`: "),
            &format!("{changed} (`MutatedConstTest` line 28, first test at line 29: `MutatedConstTest::CASES`: "),
            &format!("{changed} (`MutatedConstTest` line 34, first test at line 35: `Flags::ALL`: "),
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
/// String element is the constant's own object.
#[test]
fn constant_elements_keep_their_identity() {
    let frozen = "module FrozenKinds\n  ALL = [\"a\", \"b\"].freeze\n  NESTED = [[\"x\", \"y\"]].freeze\nend\n";
    let thawed = "module ThawedKinds\n  ALL = [\"c\"]\nend\n";
    let test = r##"class IdentityTest < ActiveSupport::TestCase
  FrozenKinds::ALL.each do |k|
    test "same #{k}" do
      assert_same FrozenKinds::ALL.find { |x| x == k }, k
    end
  end

  FrozenKinds::NESTED.each do |pair|
    test "pair #{pair.join}" do
      refute_predicate pair, :frozen?
      assert_same FrozenKinds::NESTED.first, pair
    end
  end

  ThawedKinds::ALL.each do |k|
    test "thawed #{k}" do
      assert_same ThawedKinds::ALL.first, k
    end
  end
end
"##;
    let native = matches_native(
        &[("app/models/frozen_kinds.rb", frozen), ("app/models/thawed_kinds.rb", thawed), ("test/models/identity_test.rb", test)],
        &["IdentityTest"],
        &|_| false,
        &[],
    );
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
        &["tests declared by a loop over a constant that is changed after it is assigned are not modeled; they will not run (`LevelsTest` line 2, first test at line 3: `Levels::ALL`: `Levels::ALL << :extra` (test/support/levels_ext.rb line 1, a test source not registered before this one))"],
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
        &["tests declared by a loop over a constant that is changed after it is assigned are not modeled; they will not run (`LevelsTest` line 2, first test at line 3: `Levels::ALL`: `Levels::ALL << :extra` (components/z/test/support/levels_ext.rb line 1, a test source not registered before this one))"],
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
        &["tests declared by a loop over a collection that is not a literal are not modeled; they will not run (`ChainedTest` line 2, first test at line 3: "],
    );
    assert_eq!(native.len(), 3, "{native:#?}");
}

/// A constant changed through `const_get`, or through its elements in a
/// block (`each(&:upcase!)`, `each { |k| k << "z" }`, a local bound to
/// one), is a gap; a block that only reads them is not.
#[test]
fn constants_changed_through_const_get_or_elements_are_gaps() {
    let levels = "module Levels\n  ALL = [:a, :b]\n  NAMES = %w[x y]\n  KEYS = %w[k]\n  COPIED = %w[m]\n  SAFE = %w[s]\nend\n";
    let setup = r##"Levels.const_get(:ALL) << :c
Levels::NAMES.each(&:upcase!)
Levels::KEYS.each { |k| k << "z" }
Levels::COPIED.each do |n|
  m = n
  m.concat("q")
end
Levels::SAFE.each { |s| s.upcase }
Levels.const_get(:SAFE).map(&:upcase)
"##;
    let test = r##"class EscapeTest < ActiveSupport::TestCase
  Levels::ALL.each do |l|
    test "level #{l}" do
      assert_kind_of Symbol, l
    end
  end

  Levels::NAMES.each do |n|
    test "name #{n}" do
      assert_kind_of String, n
    end
  end

  Levels::KEYS.each do |k|
    test "key #{k}" do
      assert_equal "kz", k
    end
  end

  Levels::COPIED.each do |c|
    test "copied #{c}" do
      assert_equal "mq", c
    end
  end

  Levels::SAFE.each do |s|
    test "safe #{s}" do
      assert_equal "s", s
    end
  end
end
"##;
    let changed = "tests declared by a loop over a constant that is changed after it is assigned are not modeled; they will not run";
    let native = matches_native(
        &[("app/models/levels.rb", levels), ("app/models/levels_setup.rb", setup), ("test/models/escape_test.rb", test)],
        &["EscapeTest"],
        &|id| !id.ends_with("#test_safe_s"),
        &[
            &format!("{changed} (`EscapeTest` line 2, first test at line 3: `Levels::ALL`: `Levels.const_get(:ALL) << :c` (app/models/levels_setup.rb line 1))"),
            &format!("{changed} (`EscapeTest` line 8, first test at line 9: `Levels::NAMES`: `Levels::NAMES.each(&:upcase!)` (app/models/levels_setup.rb line 2))"),
            &format!("{changed} (`EscapeTest` line 14, first test at line 15: `Levels::KEYS`: "),
            &format!("{changed} (`EscapeTest` line 20, first test at line 21: `Levels::COPIED`: "),
        ],
    );
    for id in ["EscapeTest#test_level_c", "EscapeTest#test_name_X", "EscapeTest#test_key_kz", "EscapeTest#test_copied_mq"] {
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
