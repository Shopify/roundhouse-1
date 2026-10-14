//! A scope lambda's splat (`->(key, *rest)`, Core's `IndexScopes`) is a
//! parameter of the class method the scope lowers to, and a call that hands
//! it the relation (an association scope calling it, `Order#line_items`)
//! passes the relation apart from the splat.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_scope_with_a_splat_takes_its_arguments_and_the_association_relation() {
    emit_and_run::real_blog()
        .edit(
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  belongs_to :article",
            "class Comment < ApplicationRecord\n  belongs_to :article\n\n  scope :with_ids, ->(*ids) { where(id: ids) }\n  scope :first_of, lambda { |first, *rest| where(id: [first] + rest) }\n  scope :via_ids, ->(*ids) { with_ids(*ids) }\n  scope :hinted, ->(*keys) { with_ids(*Comment.pluck(:id).take(keys.size)) }",
        )
        .edit(
            "app/models/article.rb",
            "has_many :comments, dependent: :destroy",
            "has_many :comments, -> { hinted(:one).order(:id) }, dependent: :destroy",
        )
        .run_ruby(
            r#"article = Article.create!(title: "A", body: "x" * 20)
other = Article.create!(title: "B", body: "y" * 20)
a = Comment.create!(article: article, commenter: "a", body: "one")
b = Comment.create!(article: article, commenter: "b", body: "two")
c = Comment.create!(article: other, commenter: "c", body: "three")
raise "direct" unless Comment.with_ids(a.id, b.id).order(:id).pluck(:id) == [a.id, b.id]
raise "first and rest" unless Comment.first_of(a.id, c.id).order(:id).pluck(:id) == [a.id, c.id]
raise "forwarded splat" unless Comment.via_ids(b.id, c.id).order(:id).pluck(:id) == [b.id, c.id]
raise "no arguments" unless Comment.with_ids.pluck(:id) == []
# The association scope reaches the splat scope with the relation apart from it.
raise "association scope: #{article.comments.pluck(:id).inspect}" unless article.comments.pluck(:id) == [a.id]
"#,
        )
        .assert_passes();
}

/// Mid-chain, on a relation value, the splat scope still answers: the
/// relation delegate forwards the arguments and the relation.
#[test]
fn a_scope_with_a_splat_is_callable_on_a_relation() {
    emit_and_run::real_blog()
        .edit(
            "app/models/comment.rb",
            "class Comment < ApplicationRecord\n  belongs_to :article",
            "class Comment < ApplicationRecord\n  belongs_to :article\n\n  scope :with_ids, ->(*ids) { where(id: ids) }\n  scope :hinted, lambda { |key, *rest| where(id: rest) }",
        )
        .run_ruby(
            r#"article = Article.create!(title: "A", body: "x" * 20)
a = Comment.create!(article: article, commenter: "a", body: "one")
b = Comment.create!(article: article, commenter: "b", body: "two")
c = Comment.create!(article: article, commenter: "c", body: "three")
scope = Comment.where(commenter: ["a", "b"])
raise "mid-chain" unless scope.with_ids(a.id, c.id).pluck(:id) == [a.id]
raise "mid-chain with a leading key" unless scope.hinted("k", b.id, c.id).pluck(:id) == [b.id]
"#,
        )
        .assert_passes();
}
