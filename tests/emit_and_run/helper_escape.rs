//! A template escapes an app helper's String as Rails does: a helper
//! whose every return is plain text (a literal, an interpolation, another
//! such helper) is escaped, and one that composes tag helpers is not.

use super::emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::real_blog()
        .write(
            "app/helpers/badge_helper.rb",
            "module BadgeHelper\n  def plain_badge\n    \"<b>plain</b>\"\n  end\n\n  def counted_badge(n)\n    n > 1 ? \"#{n} <items>\" : plain_badge\n  end\n\n  def linked_badge\n    link_to(\"home\", \"/\")\n  end\nend\n",
        )
        .edit("app/views/articles/index.html.erb", "<h1", "<p id=\"plain\"><%= plain_badge %></p><p id=\"counted\"><%= counted_badge(2) %></p><p id=\"linked\"><%= linked_badge %></p>\n<h1")
        .write(
            "test/controllers/badge_controller_test.rb",
            "require \"test_helper\"\n\nclass BadgeControllerTest < ActionDispatch::IntegrationTest\n  test \"helper strings\" do\n    get articles_url\n    assert_includes response.body, \"<p id=\\\"plain\\\">&lt;b&gt;plain&lt;/b&gt;</p>\"\n    assert_includes response.body, \"<p id=\\\"counted\\\">2 &lt;items&gt;</p>\"\n    assert_includes response.body, \"<p id=\\\"linked\\\"><a href=\\\"/\\\">home</a></p>\"\n  end\nend\n",
        )
}

#[test]
fn a_helper_s_plain_string_is_escaped_and_its_markup_is_not() {
    app().run_test("test/controllers/badge_controller_test.rb").assert_passes();
}
