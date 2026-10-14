//! `errors[:field]` read through a plain class's own `errors` reader (Core's
//! `Sales::NormalizeOrderErrors`: `attr_reader :errors`, handed a record's
//! errors by its caller). The reader is the class's own, so the read is
//! not the receiving object's `error_messages_for`, which the plain class
//! does not define: it asks the collection the reader holds.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const NORMALIZER: &str = r#"class Normalizer
  attr_reader :errors, :article

  def initialize(errors, article)
    @errors = errors
    @article = article
  end

  def title_errors
    errors[:title]
  end

  def title_flagged?
    !errors[:title].include?("is not wanted")
  end
end
"#;

#[test]
fn errors_index_through_a_plain_class_reader() {
    emit_and_run::real_blog()
        .write("app/models/normalizer.rb", NORMALIZER)
        .run_ruby(r#"
article = Article.new(title: "A title", body: "A long enough body")
raise "empty" unless Normalizer.new(article.errors, article).title_errors.empty?
raise "flagged" unless Normalizer.new(article.errors, article).title_flagged?
article.title = ""
article.valid?
found = Normalizer.new(article.errors, article).title_errors
raise found.inspect unless found == ["can't be blank"]
puts "passed"
"#)
        .assert_passes();
}
