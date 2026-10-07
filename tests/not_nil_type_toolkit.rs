//! `x.not_nil!` — type_toolkit's nil assertion, Shopify Core's spelling
//! of Sorbet's `T.must` (Core's Gemfile pins `type_toolkit >= 0.0.5`).
//! The gem's definition: every object answers itself, and nil raises
//! `TypeToolkit::UnexpectedNilError` (an Exception, not a
//! StandardError). The CRuby tree carries that definition; every other
//! tree refuses the call. The `#: as !nil` comment is not this call: it
//! lowers to T.must's check (tests/rbs_as_not_nil_check.rs). The error class is not a modeled constant,
//! so the test reads it by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

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
  end
  def first_name
    @names.first.not_nil!.upcase
  end
  def find(name)
    @names.find { |n| n == name }.not_nil!
  end
  def swallowed?
    begin
      Lookup.new([]).first_name
      false
    rescue => e
      true
    end
  end
end
"#;

const TEST: &str = r#"require "test_helper"
require "lookup"
class LookupTest < ActiveSupport::TestCase
  test "present values pass through" do
    lookup = Lookup.new(["ada", "grace"])
    assert_equal "ADA", lookup.first_name
    assert_equal "grace", lookup.find("grace")
  end
  test "nil raises the gem's error, past a bare rescue" do
    error = assert_raises(Exception) { Lookup.new([]).first_name }
    assert_equal "TypeToolkit::UnexpectedNilError", error.class.name
    assert_equal "Called `not_nil!` on nil.", error.message
    assert_equal false, error.is_a?(StandardError)
    missing = assert_raises(Exception) { Lookup.new(["ada"]).find("bob") }
    assert_equal "TypeToolkit::UnexpectedNilError", missing.class.name
    escaped = assert_raises(Exception) { Lookup.new(["ada"]).swallowed? }
    assert_equal "TypeToolkit::UnexpectedNilError", escaped.class.name
  end
end
"#;

#[test]
fn not_nil_runs_in_emitted_ruby() {
    app(LIB, TEST).run_test("test/models/lookup_test.rb").assert_passes();
}

#[test]
fn not_nil_is_refused_where_no_tree_defines_it() {
    for target in [BuildTarget::Spinel, BuildTarget::Rust] {
        let (_, errors) = app(LIB, TEST).emit(target);
        assert!(
            errors.iter().any(|e| e.contains("not_nil!")),
            "{target:?} must refuse not_nil!: {errors:#?}"
        );
    }
}
