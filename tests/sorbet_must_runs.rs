//! `T.must(x)` as sorbet-runtime runs it, in emitted Ruby: the value
//! when it is not nil (`false` included), else `TypeError` with
//! sorbet's message ("Passed `nil` into T.must"). Shopify Core does not
//! replace `T::Configuration`'s inline type error handler, whose default
//! re-raises. The argument is evaluated once, whether it is a local, an
//! ivar or a call. No tree loads sorbet-runtime: the check is lowered.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn app(lib: &str, test: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/lookup.rb", lib)
        .write("test/lookup_test.rb", test)
}

const LIB: &str = r#"class Lookup
  def initialize(names)
    @names = names
    @calls = 0
  end
  def first_name
    T.must(@names.first).upcase
  end
  def names
    T.must(@names)
  end
  def pass(value)
    T.must(value)
  end
  def counted
    T.must(bump)
    @calls
  end
  def bump
    @calls += 1
  end
end
"#;

const TEST: &str = r#"require "test_helper"
require "lookup"
class LookupTest < ActiveSupport::TestCase
  test "present values pass through" do
    lookup = Lookup.new(["ada", "grace"])
    assert_equal "ADA", lookup.first_name
    assert_equal ["ada", "grace"], lookup.names
    assert_equal 0, lookup.pass(0)
    assert_equal false, lookup.pass(false)
    assert_equal 1, lookup.counted
  end
  test "nil raises sorbet's TypeError" do
    error = assert_raises(TypeError) { Lookup.new([]).first_name }
    assert_equal "Passed `nil` into T.must", error.message
    local = assert_raises(TypeError) { Lookup.new([]).pass(nil) }
    assert_equal "Passed `nil` into T.must", local.message
    ivar = assert_raises(TypeError) { Lookup.new(nil).names }
    assert_equal "Passed `nil` into T.must", ivar.message
  end
end
"#;

#[test]
fn t_must_runs_in_emitted_ruby() {
    app(LIB, TEST).run_test("test/models/lookup_test.rb").assert_passes();
}
