//! `x #: as !nil` lowers to the check `T.must(x)` lowers to: the value
//! when it is not nil, else TypeError. Core writes the comment, `T.must`
//! and `not_nil!` for the same assertion. RBS inline checks nothing at
//! runtime, so natively a wrong annotation lets nil run on; the emitted
//! tree stops there instead of typing nil as a non-nil value.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/roster.rb", r#"class Roster
  def initialize(names)
    @names = names
  end

  def first_name
    name = @names.first #: as !nil
    name.upcase
  end

  def lookup(name)
    @names.find { |n| n == name } #: as !nil
  end
end
"#)
        .write("test/roster_test.rb", r#"require "test_helper"
require "roster"
class RosterTest < ActiveSupport::TestCase
  test "a present value passes through once" do
    assert_equal "ADA", Roster.new(["ada", "grace"]).first_name
    assert_equal "grace", Roster.new(["ada", "grace"]).lookup("grace")
  end
  test "nil raises TypeError" do
    error = assert_raises(TypeError) { Roster.new([]).first_name }
    assert_equal "Passed `nil` into `#: as !nil`", error.message
    assert_raises(TypeError) { Roster.new(["ada"]).lookup("bob") }
  end
end
"#)
}

#[test]
fn as_not_nil_runs_as_a_nil_check_in_emitted_ruby() {
    app().run_test("test/models/roster_test.rb").assert_passes();
}

#[test]
fn as_not_nil_is_not_refused() {
    let (_, errors) = app().emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("non-nil assertion")), "{errors:#?}");
}
