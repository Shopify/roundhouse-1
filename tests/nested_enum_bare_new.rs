//! A nested `T::Enum` whose members are bare `new` (Sorbet serializes the
//! lowercased constant name) feeds `enum(...)` like one with explicit
//! values. Core's `Sales::Subscriptions::Contract::ContractType` is spelled
//! `Ephemeral = new` / `Standard = new`; its `validates ..., if: :standard?`
//! died with `undefined method 'standard?'` when the enum expanded to nothing.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_bare_new_nested_enum_defines_its_predicates() {
    emit_and_run::real_blog()
        .edit(
            "db/schema.rb",
            "create_table \"articles\", force: :cascade do |t|",
            "create_table \"articles\", force: :cascade do |t|\n    t.string \"kind\", default: \"standard\", null: false",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            r#"class Article < ApplicationRecord
  class Kind < T::Enum
    enums do
      Ephemeral = new
      Standard = new
    end
  end
  enum(:kind, Kind.values.map(&:serialize).to_h { |s| [s.to_sym, s] })

  has_many :comments, dependent: :destroy"#,
        )
        .write(
            "test/models/article_bare_new_enum_test.rb",
            r#"require "test_helper"

class ArticleBareNewEnumTest < ActiveSupport::TestCase
  test "predicates come from the bare new members" do
    assert_equal({"ephemeral" => "ephemeral", "standard" => "standard"}, Article.kinds)
    article = articles(:one)
    assert article.standard?
    assert_not article.ephemeral?
  end
end
"#,
        )
        .run_test("test/models/article_bare_new_enum_test.rb")
        .assert_passes();
}

/// Core names `Sales::Subscriptions::Contract` all over the app as a value
/// (`T.nilable(Contract)`, `is_a?(Contract)`, `model: Contract`, returned by a
/// query base). That hands out the record class, not a namespace alias, so
/// its nested enum must keep folding.
#[test]
fn naming_the_model_class_as_a_value_keeps_its_nested_enum() {
    emit_and_run::real_blog()
        .edit(
            "db/schema.rb",
            "create_table \"articles\", force: :cascade do |t|",
            "create_table \"articles\", force: :cascade do |t|\n    t.string \"kind\", default: \"standard\", null: false",
        )
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n  has_many :comments, dependent: :destroy",
            r#"class Article < ApplicationRecord
  class Kind < T::Enum
    enums do
      Ephemeral = new
      Standard = new
    end
  end
  enum(:kind, Kind.values.map(&:serialize).to_h { |s| [s.to_sym, s] })

  has_many :comments, dependent: :destroy"#,
        )
        .write(
            "app/models/article_registry.rb",
            r#"class ArticleRegistry
  MODELS = { article: Article }.freeze

  attr_reader :klass

  def initialize
    @klass = Article
  end

  def article?(record)
    record.is_a?(Article)
  end

  def source
    Article
  end

  def find_one(id)
    Article.find_by(id: id)
  end
end
"#,
        )
        .write(
            "test/models/article_named_as_value_test.rb",
            r#"require "test_helper"

class ArticleNamedAsValueTest < ActiveSupport::TestCase
  test "predicates survive the class being used as a value" do
    article = articles(:one)
    assert ArticleRegistry.new.article?(article)
    assert article.standard?
    assert_not article.ephemeral?
  end
end
"#,
        )
        .run_test("test/models/article_named_as_value_test.rb")
        .assert_passes();
}
