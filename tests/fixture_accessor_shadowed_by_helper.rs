//! A test class's own `articles(identifier)` helper is what `articles(:one)`
//! calls, not the fixture set's reader.
//!
//! `ActiveRecord::TestFixtures.setup_fixture_accessors` defines the readers
//! on `fixture_accessor_module`, a module included into the test class, so a
//! method the class defines itself, or one from a module included after it
//! (a support helper), answers first. The rewrite that binds `xs(:label)` to
//! `XsFixtures.label` ran regardless, so a helper's lookup (by id, say) was
//! bypassed and its result never seen.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const HELPER: &str = r#"module ArticleHelper
  private

  def articles(identifier)
    "helper:#{identifier}"
  end
end
"#;

const TEST: &str = r##"require "test_helper"
require "support/helpers/article_helper"

class HelperShadowTest < ActiveSupport::TestCase
  include ArticleHelper

  test "an included helper answers the call" do
    assert_equal "helper:one", articles(:one)
  end
end

class OwnMethodShadowTest < ActiveSupport::TestCase
  def articles(identifier)
    "own:#{identifier}"
  end

  test "the class's own method answers the call" do
    assert_equal "own:two", articles(:two)
  end
end

class ChildShadowTest < HelperShadowTest
  test "a subclass keeps the parent's helper" do
    assert_equal "helper:two", articles(:two)
  end
end

class PlainFixtureTest < ActiveSupport::TestCase
  test "without a helper the fixture reader answers" do
    assert_kind_of Article, articles(:one)
    assert_kind_of Comment, comments(:one)
  end
end
"##;

#[test]
fn a_helper_named_like_a_fixture_set_answers_the_accessor_call() {
    let run = emit_and_run::real_blog()
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n  - test/models\n")
        .write("test/support/helpers/article_helper.rb", HELPER)
        .write("test/models/helper_shadow_test.rb", TEST)
        .run_test("test/models/helper_shadow_test.rb");
    run.assert_passes();
}
