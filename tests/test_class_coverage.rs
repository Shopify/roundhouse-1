//! Every test class an authored test file declares runs, or is refused by
//! name. Minitest runs every `Minitest::Runnable` subclass a test file
//! defines, so a class the compiler does not carry is a suite silently
//! missing from every tally. Four ways one went missing:
//!
//! - a test class in a test class's body (Core's `class
//!   PermissionGeneratorTest … class ValidateInitialTest <
//!   PermissionGeneratorTest`) rode along as an inner helper, emitted
//!   verbatim and never driven;
//! - a test class whose name and parent are not test-shaped (`class
//!   SmokeChecks < FirstTest`) was a helper of the file's test class;
//! - a class reopened in a second authored file was emitted to the same
//!   path, and the second written replaced the first;
//! - a class inside an `if` was not collected at all, and nothing said so.
//!
//! The authored file also maps to several emitted files (one per class),
//! so the emitted `test/roundhouse_tests.json` index names all of them
//! for a runner that is given the authored file.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::collections::BTreeMap;

use roundhouse::project::BuildTarget;

const PAIR: &str = r#"class FirstTest < ActiveSupport::TestCase
  test "one" do
    assert true
  end

  class NestedTest < FirstTest
    test "nested" do
      assert_equal 2, 1 + 1
    end
  end
end

class SecondTest < ActiveSupport::TestCase
  test "two" do
    assert true
  end
end

class SmokeChecks < FirstTest
  test "three" do
    assert true
  end
end
"#;

const AGAIN: &str = r#"class SecondTest < ActiveSupport::TestCase
  test "reopened" do
    assert true
  end
end
"#;

const WRAPPED: &str = r#"module Ns
  class WrappedTest < ActiveSupport::TestCase
    test "w" do
      assert true
    end
  end
end
"#;

/// The index's `source → [(class, file, tests)]`.
fn index(dir: &std::path::Path) -> BTreeMap<String, Vec<(String, String, Vec<String>)>> {
    let text = std::fs::read_to_string(dir.join("test/roundhouse_tests.json")).expect("test index");
    let json: serde_json::Value = serde_json::from_str(&text).expect("index json");
    assert_eq!(json["schema"], "roundhouse-test-index/v1");
    json["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .map(|s| {
            let classes = s["classes"]
                .as_array()
                .expect("classes")
                .iter()
                .map(|c| {
                    let tests = c["tests"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
                    (c["class"].as_str().unwrap().to_string(), c["file"].as_str().unwrap().to_string(), tests)
                })
                .collect();
            (s["source"].as_str().unwrap().to_string(), classes)
        })
        .collect()
}

#[test]
fn every_test_class_of_an_authored_file_is_emitted_indexed_and_run() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("test/lib/pair_test.rb", PAIR)
        .write("test/lib/again_test.rb", AGAIN)
        .write("test/lib/wrapped_test.rb", WRAPPED)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let index = index(&dir);
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        index["test/lib/pair_test.rb"],
        vec![
            ("FirstTest".into(), "test/models/first_test.rb".into(), s(&["test_one"])),
            // Its own test, then the one it inherits, as Minitest runs it.
            ("FirstTest::NestedTest".into(), "test/models/first_test_nested_test.rb".into(), s(&["test_nested", "test_one"])),
            // again_test.rb sorts first and took second_test.rb.
            ("SecondTest".into(), "test/models/second_2_test.rb".into(), s(&["test_two"])),
            ("SmokeChecks".into(), "test/models/smoke_checks_test.rb".into(), s(&["test_three", "test_one"])),
        ],
        "{index:#?}"
    );
    assert_eq!(
        index["test/lib/again_test.rb"],
        vec![("SecondTest".into(), "test/models/second_test.rb".into(), s(&["test_reopened"]))]
    );
    assert_eq!(
        index["test/lib/wrapped_test.rb"],
        vec![("Ns::WrappedTest".into(), "test/models/ns_wrapped_test.rb".into(), s(&["test_w"]))]
    );

    // Each emitted file runs its own class's tests: the nested class's
    // file loads FirstTest's (its parent) without running FirstTest's.
    let mut ran: BTreeMap<String, String> = BTreeMap::new();
    for classes in index.values() {
        for (_, file, _) in classes {
            let results = dir.join(format!("{}.results.jsonl", file.replace('/', "_")));
            let out = emit_and_run::ruby()
                .args(["-Itest", "-I.", file])
                .env("ROUNDHOUSE_TEST_RESULTS", &results)
                .current_dir(&dir)
                .output()
                .expect("spawn ruby");
            assert!(
                out.status.success(),
                "{file}:\n{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            for line in std::fs::read_to_string(&results).unwrap_or_default().lines() {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                let id = row["id"].as_str().unwrap().to_string();
                assert!(ran.insert(id.clone(), row["status"].as_str().unwrap().to_string()).is_none(), "{id} ran twice");
            }
        }
    }
    let ids: Vec<&str> = ran.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        [
            "FirstTest#test_one",
            "FirstTest::NestedTest#test_nested",
            "FirstTest::NestedTest#test_one",
            "Ns::WrappedTest#test_w",
            "SecondTest#test_reopened",
            "SecondTest#test_two",
            "SmokeChecks#test_one",
            "SmokeChecks#test_three",
        ]
    );
    assert!(ran.values().all(|status| status == "pass"), "{ran:#?}");
}

#[test]
fn a_test_class_ingest_cannot_reach_is_refused_by_name() {
    let src = "if ENV[\"X\"]\n  class GuardedTest < ActiveSupport::TestCase\n    test \"g\" do\n      assert true\n    end\n  end\nend\n";
    let err = roundhouse::ingest::ingest_test_files(src.as_bytes(), "guarded_test.rb").unwrap_err().to_string();
    assert!(err.contains("test class `GuardedTest` (line 2) refused"), "{err}");

    let src = "class LoopTest < ActiveSupport::TestCase\n  %w[a b].each do |x|\n    test \"#{x}\" do\n      assert true\n    end\n  end\nend\n";
    let err = roundhouse::ingest::ingest_test_files(src.as_bytes(), "loop_test.rb").unwrap_err().to_string();
    assert!(err.contains("test class `LoopTest` refused: a test declared at line 3"), "{err}");
}
