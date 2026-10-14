//! ActiveRecord surface Core's unit tests reached and the ruby lane lacked
//! (Rails 8.2 semantics, run on the emitted tree):
//!
//! * `create_or_find_by` / `create_or_find_by!`: create in a savepoint; on
//!   RecordNotUnique find the winner, the block running only for a create.
//! * `insert!` / `insert` / `insert_all(!)` on the model, so a model's own
//!   `def self.insert!(attrs, **options); super(…); end` has a superclass
//!   method (GlobalDB::ShopRecord's shape).
//! * A model's own association writer calling `super` (ProductVariant's
//!   `product=`): the generated writer lives in GeneratedAttributeMethods.
//! * A model's own `before_create` runs after the one an ancestor class
//!   defines (ApplicationRecord's GenerateOid allocates the id), rather than
//!   replacing it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  primary_abstract_class

  before_create :parent_hook

  def parent_hook
    $marks << "parent"
  end
end
"#;

const ARTICLE: &str = r#"class Article < ApplicationRecord
  has_many :comments, dependent: :destroy

  validates :title, presence: true
  validates :body, presence: true, length: { minimum: 10 }

  before_create :child_hook

  def child_hook
    $marks << "child"
  end

  def self.insert!(attributes)
    super(attributes.merge(body: "defaulted body"))
  end
end
"#;

const COMMENT: &str = r#"class Comment < ApplicationRecord
  belongs_to :article

  validates :commenter, presence: true
  validates :body, presence: true

  def article=(article)
    super
    @writer_super_ran = !article.nil?
  end

  def writer_super_ran
    @writer_super_ran
  end
end
"#;

const SCRIPT: &str = r##"$marks = []
Article.create!(title: "chain", body: "abcdefghij")
puts "hooks: #{$marks.inspect}"

first = Article.create!(title: "dup", body: "abcdefghij")
found = Article.create_or_find_by(title: "dup") { |a| a.body = "should not apply" }
puts "found existing: #{found.id == first.id} #{found.body} #{Article.where(title: "dup").count}"
made = Article.create_or_find_by(title: "fresh") { |a| a.body = "block sets body" }
puts "made: #{made.persisted?} #{made.body}"
bang = Article.create_or_find_by!(title: "dup") { |a| a.body = "should not apply" }
puts "bang existing: #{bang.id == first.id}"
begin
  Article.create_or_find_by!(title: "bad") { |a| a.body = "short" }
rescue ActiveRecord::RecordInvalid => e
  puts "bang invalid: #{e.class}"
end

Article.insert!(title: "inserted", body: "ignored")
row = Article.find_by(title: "inserted")
puts "insert!: #{row.body} #{row.created_at.nil? ? "no stamp" : "stamped"}"
begin
  Article.insert_all!([{ title: "dup", body: "abcdefghij" }])
rescue ActiveRecord::RecordNotUnique => e
  puts "insert_all!: #{e.class}"
end
puts "insert_all skips: #{Article.insert_all([{ title: "dup", body: "abcdefghij" }, { title: "extra", body: "abcdefghij" }])} #{Article.where(title: "extra").count}"
begin
  Article.insert_all([{ title: "a", body: "abcdefghij" }, { title: "b" }])
rescue ArgumentError => e
  puts "mixed keys: #{e.message}"
end

comment = Comment.new
comment.article = first
puts "writer: #{comment.article_id == first.id} #{comment.writer_super_ran}"
comment.article = nil
puts "writer nil: #{comment.writer_super_ran}"
"##;

const EXPECTED: &[&str] = &[
    r#"hooks: ["parent", "child"]"#,
    "found existing: true abcdefghij 1",
    "made: true block sets body",
    "bang existing: true",
    "bang invalid: ActiveRecord::RecordInvalid",
    "insert!: defaulted body stamped",
    "insert_all!: ActiveRecord::RecordNotUnique",
    "insert_all skips: 1 1",
    "mixed keys: All objects being inserted must have the same keys",
    "writer: true true",
    "writer nil: false",
];

#[test]
fn model_surface_runs_as_on_rails() {
    let run = emit_and_run::real_blog()
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/article.rb", ARTICLE)
        .write("app/models/comment.rb", COMMENT)
        .edit(
            "db/schema.rb",
            "    t.string \"title\"\n    t.text \"body\"",
            "    t.string \"title\"\n    t.text \"body\"\n    t.index [\"title\"], unique: true, name: \"index_articles_on_title\"",
        )
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), EXPECTED, "{}", run.stderr);
}
