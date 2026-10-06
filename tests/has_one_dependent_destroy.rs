//! `has_one …, dependent: :destroy` cascades through the synthesized
//! `before_destroy`, the same hook `has_many` already uses. Autosave
//! and preload are covered in `has_one_autosave.rs` /
//! `has_one_preload.rs`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn destroying_the_owner_destroys_the_has_one_child() {
    emit_and_run::real_blog()
        .edit(
            "db/schema.rb",
            "  create_table \"comments\", force: :cascade do |t|",
            "  create_table \"profiles\", force: :cascade do |t|\n    t.integer \"article_id\"\n    t.string \"bio\"\n  end\n\n  create_table \"comments\", force: :cascade do |t|",
        )
        .write(
            "app/models/profile.rb",
            "class Profile < ApplicationRecord\n  belongs_to :article\nend\n",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy\n  has_one :profile, dependent: :destroy",
        )
        .run_ruby(
            r#"
a = Article.create!(title: "Hello world", body: "abcdefghij")
child = Profile.create!(article_id: a.id, bio: "hi")
raise "reader missed child" unless a.profile.bio == "hi"
a.destroy
raise "child survived" unless Profile.find_by(id: child.id).nil?
raise "owner survived" unless Article.find_by(id: a.id).nil?
b = Article.create!(title: "Empty owner", body: "abcdefghij")
b.destroy
raise "nil child raised" unless Article.find_by(id: b.id).nil?
puts "has_one dependent destroy passed"
"#,
        )
        .assert_passes();
}
