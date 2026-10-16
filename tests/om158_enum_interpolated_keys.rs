//! A Rails `enum` whose hash keys interpolate a nested T::Enum's
//! `serialize` (Core's Merchandising::ProductPublicationStatus):
//! `enum :status, {"#{Status::Approved.serialize}": 0, ...}`. The class
//! reader (`Article.severities`), the instance predicates and the stored
//! integers must all exist, as natively; fixtures read `Model.statuses[:x]`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn enum_with_interpolated_serialize_keys_defines_the_class_reader() {
    emit_and_run::real_blog()
        .edit(
            "db/schema.rb",
            "create_table \"articles\", force: :cascade do |t|",
            "create_table \"articles\", force: :cascade do |t|\n    t.integer \"severity\", default: 0, null: false",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            r##"class Article < ApplicationRecord
  has_many :comments, dependent: :destroy

  class Severity < T::Enum
    enums do
      Mild = new
      Severe = new
      NeedsAction = new("needs_action")
    end
  end

  enum :severity, {
    "#{Severity::Mild.serialize}": 0,
    "#{Severity::Severe.serialize}": 1,
    "#{Severity::NeedsAction.serialize}": 2,
  }"##,
        )
        .write(
            "test/models/article_interpolated_enum_test.rb",
            r#"require "test_helper"

class ArticleInterpolatedEnumTest < ActiveSupport::TestCase
  test "interpolated serialize keys define the enum" do
    assert_equal({"mild" => 0, "severe" => 1, "needs_action" => 2}, Article.severities)
    assert_equal 2, Article.severities[:needs_action]
    article = articles(:one)
    assert article.mild?
    article.needs_action!
    assert_equal 2, ActiveRecord.adapter.find("articles", article.id)["severity"]
    assert_equal "needs_action", Article.find(article.id).severity
    assert_equal article.id, Article.needs_action.first.id
  end
end
"#,
        )
        .run_test("test/models/article_interpolated_enum_test.rb")
        .assert_passes();
}
