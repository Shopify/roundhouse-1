//! A class-body statement that declares tests is either modeled exactly or
//! named by itself as a survey gap; the class's other tests still run.
//!
//! - a name of adjacent literals (`"a " "b"`) is one literal;
//! - a loop over a literal collection (`%w[]`, an Array, a Hash, a Range,
//!   `n.times`, or a same-class `CONST = [...].freeze` of frozen values)
//!   declares one test per element, under the name Ruby builds for it,
//!   with the block's parameters bound to that element in the test body;
//! - any other statement that declares tests (a loop over a computed
//!   collection or a constant that is not a literal of frozen values, a
//!   computed name, a conditional `test`) is a named gap: strict ingest
//!   fails, survey mode records it and emits the rest.
//!
//! The native oracle is ActiveSupport's `test` under Minitest.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

const UNROLL: &str = r##"# frozen_string_literal: true
class UnrollTest < ActiveSupport::TestCase
  CASES = [
    { name: "plain", input: 1, expected: 2 },
    { name: "zero", input: 0, expected: 1 },
  ].freeze

  test "adjacent " "literal " \
    "names" do
    assert true
  end

  %w[alpha beta].each do |word|
    test "word #{word} is downcase" do
      assert_equal word.downcase, word
    end
  end

  [1, 2, 3].each_with_index do |n, i|
    test "item #{i} is #{n}" do
      assert_equal i + 1, n
    end
  end

  { "a" => 1, b: :two, "c d": nil }.each do |key, value|
    test "pair #{key.inspect} => #{value.inspect}" do
      assert_includes [1, :two, nil], value
    end
  end

  { x: 1 }.each_pair do |k, v|
    test "each_pair #{k}=#{v}" do
      assert_equal 1, v
    end
  end

  (1...3).each do |n|
    test "range #{n}" do
      assert_operator n, :>, 0
    end
  end

  CASES.each do |c|
    test "case #{c[:name]} #{c}" do
      assert_equal c.fetch(:expected), c[:input] + 1
    end
  end

  [[1, "one"], [2, "two"]].each do |n, word|
    test "#{word} is #{n}", slow: true do
      assert_equal n, %w[zero one two].index(word)
    end
  end

  [:a, :b].each do |outer|
    [true, false].each do |flag|
      test "nested #{outer} #{flag ? "on" : "off"}" do
        assert_includes [true, false], flag
        assert_includes [:a, :b], outer
      end
    end
  end

  [[1.5, nil, 1e20]].each do |f, missing, big|
    test "float #{f} #{big} #{missing.inspect} #{[f, :s, "q", [nil, 2]].join("-")}" do
      assert_nil missing
    end
  end

  2.times do |i|
    test "times #{i}" do
      assert_operator i, :<, 2
    end
  end

  [2].each do |n|
    test "fails for #{n}" do
      assert_equal 3, n
    end
  end

  test "no implementation"

  def test_after_loops
    assert true
  end
end
"##;

const PARTIAL: &str = r##"class PartialTest < ActiveSupport::TestCase
  COMPUTED = %w[x y].map(&:upcase)

  test "before the gaps" do
    assert true
  end

  COMPUTED.each do |name|
    test "computed #{name}" do
      assert true
    end
  end

  test "conditional" do
    assert true
  end unless ENV.key?("ROUNDHOUSE_NEVER_SET")

  %w[p q].each do |name|
    test name.upcase do
      assert true
    end
  end

  test "after the gaps" do
    assert_equal %w[X Y], COMPUTED
  end
end
"##;

/// Block parameters (destructuring, padding, a single parameter over an
/// Array element) and the `to_s` / `inspect` texts Ruby builds names
/// from.
const EDGE: &str = r##"class EdgeTest < ActiveSupport::TestCase
  PAIRS = { "k" => [1, nil], sym?: { "n" => :"a b" } }.freeze
  LIST = %i[one two?].freeze

  PAIRS.each_with_index do |(k, v), i|
    test "ewi #{k} #{v} #{i}" do
      assert true
    end
  end

  PAIRS.each_with_index do |pair, i|
    test "ewi pair #{pair} #{i}" do
      assert_kind_of Array, pair
    end
  end

  [[1, [2, 3]], [4]].each do |a, (b, c)|
    test "nest #{a} #{b.inspect} #{c.inspect}" do
      assert true
    end
  end

  [[1, 2]].each { |x| test("whole #{x}") { assert_equal [1, 2], x } }

  LIST.each do |s|
    test "sym #{s} #{s.inspect}" do
      assert true
    end
  end

  [-1, 1_000, 0.30000000000000004, 1e-5, 100.0, 2.5e15, 1e16, -0.0, 12345678901234567.0].each do |n|
    test "num #{n}" do
      assert true
    end
  end

  ["tab\there", "q\"uote", "back\\slash", "uni é", "nl\nx", ""].each do |s|
    test "str #{s.inspect} #{s}" do
      assert true
    end
  end

  [nil, true, false].each do |v|
    test "val #{v.inspect} #{v}" do
      assert true
    end
  end

  [[:a, 1], [:b, 2]].to_h.each do |k, v|
    test "to_h #{k}" do
      assert true
    end
  end

  (1..3).each_with_index do |n, i|
    test "range ewi #{n} #{i}" do
      assert true
    end
  end
end
"##;

/// The tests the gaps name: native Minitest runs them, the emitted
/// program does not.
const NOT_MODELED: [&str; 7] = [
    "EdgeTest#test_to_h_a",
    "EdgeTest#test_to_h_b",
    "PartialTest#test_computed_X",
    "PartialTest#test_computed_Y",
    "PartialTest#test_conditional",
    "PartialTest#test_P",
    "PartialTest#test_Q",
];

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

fn oracle_prelude() -> String {
    format!(
        "gem \"minitest\", \"={}\"\nrequire \"minitest/test\"\nMinitest.seed = 4242\nrequire \"json\"\nrequire \"active_support\"\nrequire \"active_support/test_case\"\n{ORACLE_TAGS}\n",
        minitest_version()
    )
}

/// `id => status` for every test Minitest runs natively in `classes`.
fn native_ids(sources: &[&str], classes: &[&str]) -> BTreeMap<String, String> {
    let mut oracle = oracle_prelude();
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
    let out = ruby().args(["-e", &oracle]).output().expect("native Minitest");
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
fn literal_loops_unroll_and_other_declarations_are_named_gaps() {
    let native = native_ids(&[UNROLL, PARTIAL, EDGE], &["UnrollTest", "PartialTest", "EdgeTest"]);
    // The oracle is the reference; the literal list pins what it means.
    let expected = [
        "PartialTest#test_P",
        "PartialTest#test_Q",
        "PartialTest#test_after_the_gaps",
        "PartialTest#test_before_the_gaps",
        "PartialTest#test_computed_X",
        "PartialTest#test_computed_Y",
        "PartialTest#test_conditional",
        "UnrollTest#test_adjacent_literal_names",
        "UnrollTest#test_after_loops",
        "UnrollTest#test_case_plain_{name:_\"plain\",_input:_1,_expected:_2}",
        "UnrollTest#test_case_zero_{name:_\"zero\",_input:_0,_expected:_1}",
        "UnrollTest#test_each_pair_x=1",
        "UnrollTest#test_fails_for_2",
        "UnrollTest#test_float_1.5_1.0e+20_nil_1.5-s-q--2",
        "UnrollTest#test_item_0_is_1",
        "UnrollTest#test_item_1_is_2",
        "UnrollTest#test_item_2_is_3",
        "UnrollTest#test_nested_a_off",
        "UnrollTest#test_nested_a_on",
        "UnrollTest#test_nested_b_off",
        "UnrollTest#test_nested_b_on",
        "UnrollTest#test_no_implementation",
        "UnrollTest#test_one_is_1_tag:slow:true",
        "UnrollTest#test_pair_\"a\"_=>_1",
        "UnrollTest#test_pair_:\"c_d\"_=>_nil",
        "UnrollTest#test_pair_:b_=>_:two",
        "UnrollTest#test_range_1",
        "UnrollTest#test_range_2",
        "UnrollTest#test_times_0",
        "UnrollTest#test_times_1",
        "UnrollTest#test_two_is_2_tag:slow:true",
        "UnrollTest#test_word_alpha_is_downcase",
        "UnrollTest#test_word_beta_is_downcase",
    ];
    let pinned: Vec<&str> = native.keys().map(String::as_str).filter(|id| !id.starts_with("EdgeTest#")).collect();
    assert_eq!(pinned, expected, "native oracle");
    assert_eq!(native.keys().filter(|id| id.starts_with("EdgeTest#")).count(), 32, "native oracle: {native:#?}");
    assert_eq!(native["UnrollTest#test_fails_for_2"], "fail");
    assert_eq!(native["UnrollTest#test_no_implementation"], "fail");

    // Strict ingest names the first statement it does not model.
    let err = roundhouse::ingest::ingest_test_files(PARTIAL.as_bytes(), "partial_test.rb").unwrap_err().to_string();
    assert!(
        err.contains("tests declared by a loop over a constant whose value is not a literal of frozen values are not modeled; they will not run (`PartialTest` line 8, first test at line 9: `PartialTest::COMPUTED`: `COMPUTED = %w[x y].map(&:upcase)`"),
        "{err}"
    );
    // A literal-only class ingests strictly.
    roundhouse::ingest::ingest_test_files(UNROLL.as_bytes(), "unroll_test.rb").expect("strict unroll");

    // Survey mode records each such statement and emits the rest.
    roundhouse::ingest::survey::activate();
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/models/unroll_test.rb", UNROLL)
        .write("test/models/partial_test.rb", PARTIAL)
        .write("test/models/edge_test.rb", EDGE)
        .emit(BuildTarget::Ruby);
    let gaps: Vec<String> = roundhouse::ingest::survey::drain().iter().map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let named = [
        "tests declared by a loop over a constant whose value is not a literal of frozen values are not modeled; they will not run (`PartialTest` line 8, first test at line 9: `PartialTest::COMPUTED`: `COMPUTED = %w[x y].map(&:upcase)`",
        "tests declared by a loop over a collection that is not a literal are not modeled; they will not run (`EdgeTest` line 49, first test at line 50",
        "tests declared by a conditional are not modeled; they will not run (`PartialTest` line 14",
        "tests declared by a loop whose test names are not known before it runs are not modeled; they will not run (`PartialTest` line 18, first test at line 19",
    ];
    for gap in named {
        assert_eq!(gaps.iter().filter(|g| g.contains(gap)).count(), 1, "{gap}\n{gaps:#?}");
    }
    assert_eq!(gaps.iter().filter(|g| g.contains("tests declared by")).count(), named.len(), "{gaps:#?}");

    // The index lists exactly the native tests the gaps do not name.
    let want: BTreeMap<String, String> =
        native.iter().filter(|(id, _)| !NOT_MODELED.contains(&id.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    let classes = index(&emitted);
    let mut indexed: Vec<String> = classes
        .iter()
        .flat_map(|(class, (_, tests))| tests.iter().map(move |t| format!("{class}#{t}")))
        .collect();
    indexed.sort();
    assert_eq!(indexed, want.keys().cloned().collect::<Vec<_>>(), "index");

    // Running the emitted files reports the same ids and outcomes.
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
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice\n{log}");
        }
    }
    assert_eq!(ran, want, "emitted results vs native");
}

#[test]
fn a_loop_that_declares_a_test_twice_raises_as_activesupport_does() {
    let source = "class TwiceTest < ActiveSupport::TestCase\n  %w[a b].each do |x|\n    test \"same\" do\n      assert true\n    end\n  end\nend\n";
    let oracle = format!("{}begin\n{source}\nrescue => e\n  puts e.message\nend\n", oracle_prelude());
    let out = ruby().args(["-e", &oracle]).output().expect("native");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "test_same is already defined in TwiceTest");

    let err = roundhouse::ingest::ingest_test_files(source.as_bytes(), "twice_test.rb").unwrap_err().to_string();
    assert!(err.contains("test_same is already defined in TwiceTest"), "{err}");
}
