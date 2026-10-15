//! `owner.assoc.create!(...)` rewritten onto the target model.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

/// Core's `@shop.themes.create!` inside `module OnlineStore`, which
/// also has an `OnlineStore::Theme` module: the rewrite's bare `Theme`
/// resolved lexically to the namespace module and raised NoMethodError
/// for `create!`. The target is now rooted (`::Comment`).
#[test]
fn an_association_create_in_a_namespace_with_a_same_named_module() {
    let run = emit_and_run::real_blog()
        .write("app/models/blog/comment/marker.rb", "module Blog\n  module Comment\n    module Marker\n      def self.mark = :marked\n    end\n  end\nend\n")
        .write(
            "test/models/blog/comment_create_test.rb",
            "require \"test_helper\"\n\nmodule Blog\n  class CommentCreateTest < ActiveSupport::TestCase\n    test \"creates through the association\" do\n      article = articles(:one)\n      comment = article.comments.create!(commenter: \"x\", body: \"y\")\n      assert_equal article.id, comment.article_id\n      assert_equal :marked, Blog::Comment::Marker.mark\n    end\n  end\nend\n",
        )
        .run_test("test/models/blog_comment_create_test.rb");
    run.assert_passes();
}

/// Core's `theme.files.build("sections/x.liquid")`: `Theme#files` is a
/// plain method returning an `AssetCloud::ThemeFiles`, not an
/// association. The by-name guess (`File` + `theme_id`) rewrote the call
/// to `::File.new("sections/x.liquid".merge(theme_id: id))`, which raised
/// "undefined method 'merge' for an instance of String".
#[test]
fn a_build_on_a_plain_method_reader_is_left_to_that_method() {
    let run = emit_and_run::real_blog()
        .write(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n\n  validates :title, presence: true\n  validates :body, presence: true, length: { minimum: 10 }\n\n  class Files\n    def build(path)\n      \"built:#{path}\"\n    end\n  end\n\n  def drafts\n    Files.new\n  end\nend\n",
        )
        .write(
            "test/models/plain_reader_build_test.rb",
            "require \"test_helper\"\n\nclass PlainReaderBuildTest < ActiveSupport::TestCase\n  test \"build on a method reader runs the method\" do\n    article = articles(:one)\n    assert_equal \"built:a.liquid\", article.drafts.build(\"a.liquid\")\n  end\nend\n",
        )
        .run_test("test/models/plain_reader_build_test.rb");
    run.assert_passes();
}
