//! `lower::defined_ivar_memo` must see the writers in test classes.
//! Core's `Customers::GraphApiTestHelper` guards its mutation helpers
//! with `flunk(...) unless defined?(@args)`; the including test sets
//! `@args` in its own test and `setup` bodies. The pass pooled only the
//! app's writers, so the helper read an `@args_defined` flag no test
//! ever set and every `assert_user_errors_on_mutation` failed with
//! "@args instance variable is not defined" (sweep-1, customers).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const HELPER: &str = r#"module ArgsHelper
  def assert_args_present
    flunk("@args instance variable is not defined") unless defined?(@args)

    selections = @selections || { customer: :id }
    assert_equal({ id: 1 }, @args)
    assert_equal 1, selections[:a]
  end

  def assert_selections_present
    flunk("@selections instance variable is not defined") unless defined?(@selections)

    assert_equal({ a: 1 }, @selections)
  end
end
"#;

const TEST: &str = r#"require "test_helper"
require "support/args_helper"

class ArgsTest < ActiveSupport::TestCase
  include ArgsHelper

  setup do
    @selections = { a: 1 }
  end

  test "writer in the test body" do
    @args = { id: 1 }
    assert_args_present
    assert_selections_present
  end

  test "guard fails when nothing wrote the ivar" do
    err = assert_raises(Minitest::Assertion) { assert_args_present }
    assert_equal "@args instance variable is not defined", err.message
  end
end
"#;

/// CRuby's own answer: the same test file under plain Minitest.
fn native() -> String {
    let dir = std::env::temp_dir().join(format!("rh-defined-ivar-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("support")).unwrap();
    std::fs::write(dir.join("support/args_helper.rb"), HELPER).unwrap();
    std::fs::write(dir.join("args_test.rb"), TEST.replace("require \"test_helper\"\n", "")).unwrap();
    let shim = "require 'minitest/autorun'\nmodule ActiveSupport; class TestCase < Minitest::Test\n\
                def self.test(name, &b) = define_method(\"test_#{name.tr(' ', '_')}\", &b)\n\
                def self.setup(&b) = define_method(:setup, &b)\nend; end\n";
    let out = emit_and_run::ruby()
        .args(["-I", dir.to_str().unwrap(), "-e", &format!("{shim}\nload '{}'", dir.join("args_test.rb").display())])
        .output()
        .expect("CRuby control");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

#[test]
fn a_helper_guard_sees_the_including_tests_writers() {
    let native = native();
    assert!(native.contains("2 runs, 6 assertions, 0 failures, 0 errors"), "{native}");

    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/support/args_helper.rb", HELPER)
        .write("test/models/args_test.rb", TEST)
        .run_test("test/models/args_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("ArgsTest: 2 tests passed"), "{}", run.stdout);
}
