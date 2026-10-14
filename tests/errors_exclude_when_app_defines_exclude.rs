//! `errors.exclude?(:attribute)` on a record's errors when the app defines
//! an `exclude?` of its own (Core's merchandising validators ask
//! `product_variant.errors.exclude?(:quantity_unit)`), where the
//! `exclude_predicate` lowering stands off and the call reaches
//! `ActiveModel::ErrorAttributes` — which answered no `exclude?`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CHECKER: &str = r#"class Checker
  #: (untyped) -> bool
  def exclude?(value)
    !value
  end

  #: (Article) -> bool
  def title_clean?(article)
    article.errors.exclude?(:title) && article.errors.exclude?(:body)
  end
end
"#;

#[test]
fn errors_exclude_answers_when_the_app_defines_exclude() {
    emit_and_run::real_blog()
        .write("app/models/checker.rb", CHECKER)
        .run_ruby(r#"
checker = Checker.new
bad = Article.new(title: "", body: "A long enough body")
bad.valid?
raise "bad" if checker.title_clean?(bad)
good = Article.new(title: "A title", body: "A long enough body")
good.valid?
raise "good" unless checker.title_clean?(good)
puts "passed"
"#)
        .assert_passes();
}
