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
