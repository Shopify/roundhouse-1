//! `Relation#first_or_initialize(attributes = nil, &block)` takes the
//! attributes and block Rails does, and its new record carries the
//! relation's where attributes. Core's `OnlineStore::Manager` calls
//! `first_or_initialize(survives_application_reinstall: true)`: the
//! runtime method took no arguments (ArgumentError, given 1, expected 0).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn first_or_initialize_takes_attributes_and_a_block() {
    let run = emit_and_run::real_blog()
        .write(
            "app/models/comment.rb",
            r#"
class Comment < ApplicationRecord
  belongs_to :article

  def self.owned_by(id)
    where(article_id: id)
  end

  def self.initialize_with(id)
    owned_by(id).first_or_initialize(commenter: "Alice") { |record| record.body = "from block" }
  end

  def self.initialize_bare(id)
    owned_by(id).first_or_initialize
  end
end
"#,
        )
        .run_ruby(
            r#"
Db.exec("INSERT INTO articles (id, title, body, created_at, updated_at) VALUES (41, 'Owner', 'A sufficiently long body', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)")
built = Comment.initialize_with(41)
puts [built.persisted?, built.article_id, built.commenter, built.body].inspect
bare = Comment.initialize_bare(41)
puts [bare.persisted?, bare.article_id].inspect
built.save!
found = Comment.initialize_with(41)
puts [found.persisted?, found.id == built.id, found.body].inspect
"#,
        )
        ;
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[false, 41, \"Alice\", \"from block\"]\n[false, 41]\n[true, true, \"from block\"]\n", "{}", run.stderr);
}
