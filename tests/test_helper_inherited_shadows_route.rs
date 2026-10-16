//! A test class's `*_url` / `*_path` helper answers its subclasses' calls
//! too, ahead of the route helper of the same name.
//!
//! Core's `DisputeFileUploadsControllerIntegrationTest` defines
//! `create_dispute_file_upload_url(dispute_id)` and its nested `CreateTest`
//! subclass calls it. The shadow set was the calling class's own helpers
//! only, so the subclass's call became `RouteHelpers.…_path` — a route no
//! app draws.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const TEST: &str = r##"require "test_helper"

class UrlHelperParentTest < ActiveSupport::TestCase
  def article_url(id)
    "parent-helper:#{id}"
  end

  test "the class's own helper answers" do
    assert_equal "parent-helper:1", article_url(1)
  end
end

class UrlHelperChildTest < UrlHelperParentTest
  test "a subclass reaches the parent's helper, not the route" do
    assert_equal "parent-helper:2", article_url(2)
  end
end
"##;

#[test]
fn a_subclass_calls_its_parents_url_helper() {
    let run = emit_and_run::real_blog()
        .write("roundhouse.yml", "test_paths:\n  - test/models\n")
        .write("test/models/url_helper_inherit_test.rb", TEST)
        .run_test("test/models/url_helper_child_test.rb");
    run.assert_passes();
}
