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

const HELPER: &str = r#"module LinkHelper
  def article_url(id)
    "module-helper:#{id}"
  end
end
"#;

const INCLUDER: &str = r##"require "test_helper"
require "support/helpers/link_helper"

class IncludedHelperTest < ActiveSupport::TestCase
  include LinkHelper

  test "a support module's helper answers, not the route" do
    assert_equal "module-helper:3", article_url(3)
  end
end
"##;

/// Core's `OnlineStore::VersionControl::ThemeAssetsIntegrityCheckerTest`
/// calls `generate_file_path`, a method of the support module its base test
/// class includes; the `_path` suffix made it `RouteHelpers.generate_file_path`.
#[test]
fn a_test_calls_an_included_modules_url_helper() {
    let run = emit_and_run::real_blog()
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n  - test/models\n")
        .write("test/support/helpers/link_helper.rb", HELPER)
        .write("test/models/included_helper_test.rb", INCLUDER)
        .run_test("test/models/included_helper_test.rb");
    run.assert_passes();
}
