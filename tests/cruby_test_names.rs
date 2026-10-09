//! The CRuby (Minitest) tree defines, indexes, and reports each test under
//! the method name Minitest runs natively: ActiveSupport's
//! `"test_#{name.gsub(/\s+/, '_')}"`, over the name Core's TestTags tagged
//! (`_tag:<key>:<value>` per tag), or a `def`'s own name. Case, `#`, `?`,
//! `'`, `(`, `.`, `-` stay; nothing is lowercased or collapsed.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

const NAMES: &str = r##"
class NamesTest < ActiveSupport::TestCase
  test "#status validates inclusion in allowed values" do
    assert_equal 2, 1 + 1
  end

  test "DeliveryAddressProtoCodec round-trips X" do
    assert true
  end

  test "build returns a userError per element (not an error) for ids" do
    assert true
  end

  test "won't raise, ok?" do
    assert true
  end

  test "CamelCase stays" do
    assert true
  end

  test "a.b" do
    assert true
  end

  test "a b" do
    assert true
  end

  test "runs   of\twhitespace" do
    assert true
  end

  test 'keeps #{x} and #@y literal' do
    assert true
  end

  test "slow one", slow: true do
    assert true
  end

  def test_fooBar
    assert true
  end

  tag(pg_only: true) do
    test "pg one" do
      assert true
    end

    test "pg reasons", postgres_failure_reasons: [:row_ordering, :locking], pg_only: false do
      assert true
    end
  end

  test "after the block" do
    assert true
  end
end
"##;

/// A blockless `tag`: the tags stay for the rest of the body, and the
/// class's untagged tests (declared before it, and `def`s) get them too.
const PERSISTENT: &str = r#"
class PersistentTest < ActiveSupport::TestCase
  test "declared before" do
    assert true
  end

  def test_camelCase_def
    assert true
  end

  tag slow: true, owner: "team x"

  test "declared after", extra: 1 do
    assert true
  end
end
"#;

/// The tag DSL the native oracle loads: written for this test with the
/// behavior of Core's TestTags (prepended on ActiveSupport::TestCase's
/// singleton), so the oracle names tests as Core does.
const ORACLE_TAGS: &str = r#"
module OracleTags
  def self.suffixed(name, tags)
    return name if tags.empty?
    ([name] + tags.map { |key, value| "tag:" + [key, value].join(":") }).join("_")
  end

  def tag(tags)
    saved = oracle_tags.dup
    oracle_tags.merge!(tags)
    if block_given?
      begin
        yield
      ensure
        oracle_tags.replace(saved)
      end
    else
      @oracle_persistent = true
    end
  end

  def test(name, tags = nil, &block)
    super(OracleTags.suffixed(name, tags ? oracle_tags.merge(tags) : oracle_tags), &block)
  end

  def runnable_methods
    if @oracle_persistent && !@oracle_renamed
      @oracle_renamed = true
      instance_methods(false).grep(/\Atest_/).reject { |m| m.to_s.include?("tag:") }.each do |m|
        alias_method(OracleTags.suffixed(m.to_s, oracle_tags), m)
        undef_method(m)
      end
    end
    super
  end

  private

  def oracle_tags
    @oracle_tags ||= {}
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

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/models/names_test.rb", NAMES)
        .write("test/models/persistent_test.rb", PERSISTENT)
}

/// class => (emitted file, tests) from `test/roundhouse_tests.json`.
fn index(dir: &std::path::Path) -> (Value, BTreeMap<String, (String, Vec<String>)>) {
    let json: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("test/roundhouse_tests.json")).expect("test index")).unwrap();
    let mut classes = BTreeMap::new();
    for source in json["sources"].as_array().unwrap() {
        for c in source["classes"].as_array().unwrap() {
            let tests = c["tests"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
            classes.insert(c["class"].as_str().unwrap().to_string(), (c["file"].as_str().unwrap().to_string(), tests));
        }
    }
    (json["test_names"].clone(), classes)
}

#[test]
fn emitted_tests_carry_native_minitest_names_in_the_index_and_results() {
    let native = native_ids(&[NAMES, PERSISTENT], &["NamesTest", "PersistentTest"]);
    let expected = [
        "NamesTest#test_#status_validates_inclusion_in_allowed_values",
        "NamesTest#test_DeliveryAddressProtoCodec_round-trips_X",
        "NamesTest#test_build_returns_a_userError_per_element_(not_an_error)_for_ids",
        "NamesTest#test_won't_raise,_ok?",
        "NamesTest#test_CamelCase_stays",
        "NamesTest#test_a.b",
        "NamesTest#test_a_b",
        "NamesTest#test_runs_of_whitespace",
        "NamesTest#test_keeps_#{x}_and_#@y_literal",
        "NamesTest#test_slow_one_tag:slow:true",
        "NamesTest#test_fooBar",
        "NamesTest#test_pg_one_tag:pg_only:true",
        "NamesTest#test_pg_reasons_tag:pg_only:false_tag:postgres_failure_reasons:row_ordering:locking",
        "NamesTest#test_after_the_block",
        "PersistentTest#test_declared_before_tag:slow:true_tag:owner:team x",
        "PersistentTest#test_camelCase_def_tag:slow:true_tag:owner:team x",
        "PersistentTest#test_declared_after_tag:slow:true_tag:owner:team_x_tag:extra:1",
    ];
    // The oracle is the reference; the literal list pins what it means.
    let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(native.keys().cloned().collect::<Vec<_>>(), want, "native oracle");

    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");

    // The index lists exactly the native method names.
    let (marker, classes) = index(&emitted);
    assert_eq!(marker, "minitest");
    let mut indexed: Vec<String> = classes
        .iter()
        .flat_map(|(class, (_, tests))| tests.iter().map(move |t| format!("{class}#{t}")))
        .collect();
    indexed.sort();
    assert_eq!(indexed, want, "index");

    // Running the emitted files reports the same ids, each passing.
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
        assert!(out.status.success(), "{log}");
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice");
        }
    }
    assert_eq!(ran, native, "emitted results vs native");
}

#[test]
fn identifier_targets_keep_sanitized_names() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("test/models/plain_test.rb", "class PlainTest < ActiveSupport::TestCase\n  test \"CamelCase #stays\" do\n    assert true\n  end\nend\n")
        .emit(BuildTarget::Jruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (marker, classes) = index(&emitted);
    assert_eq!(marker, "sanitized");
    assert_eq!(classes["PlainTest"].1, ["test_camelcase_stays"]);
}

#[test]
fn a_name_only_whitespace_separates_is_the_same_test_natively_and_refused() {
    let source = "class DupTest < ActiveSupport::TestCase\n  test \"a b\" do\n    assert true\n  end\n  test \"a_b\" do\n    assert true\n  end\nend\n";
    let oracle = format!("{}begin\n{source}\nrescue => e\n  puts e.message\nend\n", oracle_prelude());
    let out = ruby().args(["-e", &oracle]).output().expect("native");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "test_a_b is already defined in DupTest");

    let err = roundhouse::ingest::ingest_test_files(source.as_bytes(), "dup_test.rb").unwrap_err().to_string();
    assert!(err.contains("test_a_b is already defined in DupTest"), "{err}");

    // A tagged twin is a different method: no collision.
    let tagged = "class DupTest < ActiveSupport::TestCase\n  test \"a b\" do\n    assert true\n  end\n  test \"a_b\", slow: true do\n    assert true\n  end\nend\n";
    let tm = roundhouse::ingest::ingest_test_files(tagged.as_bytes(), "dup_test.rb").expect("tagged twin ingests");
    let names: Vec<&str> = tm.iter().flat_map(|m| m.tests.iter().map(|t| t.method_name.as_str())).collect();
    assert_eq!(names, ["test_a_b", "test_a_b_tag:slow:true"]);
}

/// A blockless `test "x"` flunks under the name TestTags passed on, one
/// assertion; a persistent rename onto a twin the body defined under the
/// tag leaves one test with the first body. The emitted tree lists and
/// runs exactly what native Minitest does, with the same outcomes,
/// assertion counts and failure messages.
const EDGES: &str = r#"
class EdgesTest < ActiveSupport::TestCase
  tag(slow: true) do
    test "tagged no block"
  end

  test "no block"

  test "x" do
    assert_equal 2, 1 + 1
  end

  tag owner: "a"

  test "x" do
    flunk "the second body"
  end
end
"#;

/// `id => (status, assertions, first failure message)`.
fn outcomes(text: &str) -> BTreeMap<String, (String, u64, String)> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let message = row["exceptions"].as_array().and_then(|e| e.first()).map(|e| e["message"].as_str().unwrap_or("").to_string());
        let message = message.or_else(|| row["message"].as_str().map(str::to_string)).unwrap_or_default();
        let id = row["id"].as_str().unwrap().to_string();
        let value = (row["status"].as_str().unwrap().to_string(), row["assertions"].as_u64().unwrap(), message);
        assert!(map.insert(id.clone(), value).is_none(), "{id} reported twice");
    }
    map
}

#[test]
fn blockless_tests_and_persistent_twins_match_native() {
    let oracle = format!(
        "{}{EDGES}\nEdgesTest.runnable_methods.each do |name|\n  result = EdgesTest.new(name).run\n  status = result.passed? ? \"pass\" : (result.skipped? ? \"skip\" : (result.error? ? \"error\" : \"fail\"))\n  puts JSON.generate(id: \"EdgesTest##{{name}}\", status: status, assertions: result.assertions, message: result.failure&.message.to_s)\nend\n",
        oracle_prelude()
    );
    let out = ruby().args(["-e", &oracle]).output().expect("native Minitest");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let native = outcomes(&String::from_utf8_lossy(&out.stdout));
    let want: BTreeMap<String, (String, u64, String)> = [
        ("EdgesTest#test_tagged_no_block_tag:slow:true", ("fail", 1, "No implementation provided for tagged no block_tag:slow:true")),
        ("EdgesTest#test_no_block_tag:owner:a", ("fail", 1, "No implementation provided for no block")),
        ("EdgesTest#test_x_tag:owner:a", ("pass", 1, "")),
    ]
    .into_iter()
    .map(|(id, (status, n, message))| (id.to_string(), (status.to_string(), n, message.to_string())))
    .collect();
    assert_eq!(native, want, "native oracle");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/models/edges_test.rb", EDGES)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (_, classes) = index(&emitted);
    let (file, tests) = &classes["EdgesTest"];
    let mut listed: Vec<String> = tests.iter().map(|t| format!("EdgesTest#{t}")).collect();
    listed.sort();
    assert_eq!(listed, want.keys().cloned().collect::<Vec<_>>(), "index");
    let results = emitted.join("edges.results.jsonl");
    let out = ruby()
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), file])
        .current_dir(&emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("emitted CRuby");
    let log = format!("stdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
    assert_eq!(outcomes(&text), native, "emitted results vs native\n{log}");
}

/// ActiveSupport's `method_defined?` sees inherited methods: natively a
/// subclass's `test "shared"` under a parent's `test_shared` raises as
/// the class loads. Roundhouse refuses the subclass with that message.
#[test]
fn a_test_redefining_an_inherited_test_raises_natively_and_is_refused() {
    let base = "class BaseEdgeTest < ActiveSupport::TestCase\n  test \"shared\" do\n    assert true\n  end\nend\n";
    let sub = "class SubEdgeTest < BaseEdgeTest\n  test \"shared\" do\n    assert true\n  end\nend\n";
    let oracle = format!("{}{base}begin\n{sub}\nrescue => e\n  puts e.message\nend\n", oracle_prelude());
    let out = ruby().args(["-e", &oracle]).output().expect("native");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "test_shared is already defined in SubEdgeTest");

    let dir = std::env::temp_dir().join(format!("rh-inherited-redefinition-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in [
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("test/models/base_edge_test.rb", base),
        ("test/models/sub_edge_test.rb", sub),
    ] {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let err = roundhouse::ingest::ingest_app(&dir).map(|_| ()).unwrap_err().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.contains("test class `SubEdgeTest` refused"), "{err}");
    assert!(err.contains("`test_shared is already defined in SubEdgeTest`, inherited from `BaseEdgeTest`"), "{err}");
}
