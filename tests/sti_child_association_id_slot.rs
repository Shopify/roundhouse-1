//! A table's models share the `@__attr_<col>` storage slot
//! (`emit::ruby::column_slot`): a parent whose authored method names `@id`
//! moves the column's storage for the whole table, and an STI child's
//! compiled association readers read the id from the same slot. Core's
//! `ApiCarrierService < CarrierService` (`has_one :shipping_rate_provider`)
//! answered nil for every record: the reader asked `@id`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BASE: &str = r#"class Post < ApplicationRecord
  self.table_name = "articles"
  has_many :comments, foreign_key: :article_id

  def remembered_id
    @id ||= super
  end
end
"#;

const CHILD: &str = r#"class SpecialPost < Post
  has_many :remarks, class_name: "Comment", foreign_key: :article_id
  has_one :first_remark, class_name: "Comment", foreign_key: :article_id
end
"#;

const SCRIPT: &str = r#"article = Article.create!(title: 'A title long enough', body: 'A body long enough to pass validation')
Comment.create!(article_id: article.id, commenter: 'someone', body: 'a comment')
post = Post.find(article.id)
child = SpecialPost.find(article.id)
puts "base: #{post.comments.to_a.size}"
puts "child: #{child.remarks.to_a.size} #{child.first_remark&.commenter.inspect} #{child.comments.to_a.size}"
"#;

#[test]
fn an_sti_childs_association_readers_read_the_shared_id_slot() {
    let run = emit_and_run::real_blog().write("app/models/post.rb", BASE).write("app/models/special_post.rb", CHILD).run_ruby(SCRIPT);
    run.assert_passes();
    for line in ["base: 1", "child: 1 \"someone\" 1"] {
        assert!(run.stdout.lines().any(|l| l == line), "missing {line:?}\n{}", run.stdout);
    }
}
