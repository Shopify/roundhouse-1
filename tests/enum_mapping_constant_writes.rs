//! An enum mapping that assigns its constants as it is built.
//!
//! Core's `markets_pro/flow_commerce/application.rb`:
//!
//! ```ruby
//! enum :status, { pending: PENDING = "pending", … }
//! ```
//!
//! The stored value was carried as the whole `PENDING = "pending"`
//! expression into the generated predicate, bang writer and scopes,
//! where a constant assignment does not parse ("dynamic constant
//! assignment"), and the constants were never defined in the class
//! body. The writes are now class-body statements ahead of the enum,
//! and each label stores the written value.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn enum_mapping_constant_writes_define_the_constants() {
    let run = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true",
            "  enum :title, {\n    one: ONE = \"one\",\n    two: TWO = \"two\",\n  }\n\n  def self.second\n    TWO\n  end\n\n  validates :title, presence: true",
        )
        .run_ruby(
            r#"
raise "ONE: #{Article::ONE.inspect}" unless Article::ONE == "one"
raise "second: #{Article.second.inspect}" unless Article.second == "two"
Article.delete_all
a = Article.create!(title: "one", body: "Long enough body")
raise "one?: #{a.title.inspect}" unless a.one? && !a.two?
raise "scope one: #{Article.one.count}" unless Article.one.count == 1 && Article.two.count == 0
a.two!
raise "two!: #{a.reload.title.inspect}" unless a.reload.title == "two" && Article.two.count == 1
puts "enum constants"
"#,
        );
    let source =
        std::fs::read_to_string(run.emitted.join("app/models/article.rb")).unwrap_or_default();
    let parse = ruby_prism::parse(source.as_bytes());
    let parse_errors: Vec<String> = parse.errors().map(|e| e.message().to_string()).collect();
    assert!(parse_errors.is_empty(), "{parse_errors:?} in:\n{source}");
    run.assert_passes();
    assert_eq!(run.stdout, "enum constants\n");
}
