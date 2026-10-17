//! Rails 8.2 `errors.include?(:attribute)` asks whether the attribute has an
//! error (`assert_includes errors, :publish_date`); a message String stays
//! Array's own question. A model's accumulator answered `false` for every
//! Symbol.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ARTICLE: &str = r#"class Article < ApplicationRecord
  has_many :comments, dependent: :destroy

  alias_attribute :heading, :title

  validates :title, presence: true
  validates :body, presence: true, length: { minimum: 10 }
end
"#;

#[test]
fn include_with_a_symbol_asks_for_an_error_on_the_attribute() {
    const RUN: &str = r#"
article = Article.new
raise "empty" if article.errors.include?(:title)
raise "invalid" if article.valid?
raise "attribute" unless article.errors.include?(:title)
raise "other attribute" if article.errors.include?(:id)
raise "message" unless article.errors.include?("Title can't be blank")
raise "other message" if article.errors.include?("Id can't be blank")
puts "include passed"
"#;
    emit_and_run::real_blog().write("app/models/article.rb", ARTICLE).run_ruby(RUN).assert_passes();
}
