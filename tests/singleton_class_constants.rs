//! A constant written inside `class << self` is the singleton class's:
//! `X.singleton_class.constants` lists it, `X.constants` and `X::NAME`
//! do not see it, and the block's own methods and later constants read
//! it by bare name (lexical lookup through the singleton class). Ingest
//! registers it on the class; the Ruby family writes it back inside
//! `class << self` and reads it as `X.singleton_class::NAME`.
//!
//! CRuby running the authored source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"class SingletonTable
  LIMIT = 1
  class << self
    SINGLE = 7
    TABLE = { a: SINGLE, b: LIMIT }.freeze
    NOTE = <<~TEXT
      kept
    TEXT

    def single = SINGLE
    def table = TABLE
    def note = NOTE
  end

  def instance_sees_table = defined?(TABLE).inspect
end

class SingletonTableChild < SingletonTable
  def self.child_single = single
end

module SingletonGreeter
  class << self
    GREETING = "hi"
    def greet = GREETING
  end
end
"#;

const SCRIPT: &str = r#"p [SingletonTable.single, SingletonTable.table, SingletonTable.note, SingletonTable.new.instance_sees_table]
p [SingletonTable.constants.sort, SingletonTable.singleton_class.constants.sort, SingletonTable.const_defined?(:TABLE, false)]
p [(SingletonTable::TABLE rescue :name_error), SingletonTable.singleton_class::TABLE.frozen?]
p [SingletonTableChild.child_single, SingletonTableChild.constants.sort, SingletonTableChild.singleton_class.const_defined?(:SINGLE)]
p [SingletonGreeter.greet, SingletonGreeter.constants, SingletonGreeter.singleton_class.constants]
"#;

const EXPECTED: &str = "[7, {a: 7, b: 1}, \"kept\\n\", \"nil\"]\n\
[[:LIMIT], [:NOTE, :SINGLE, :TABLE], false]\n\
[:name_error, true]\n\
[7, [:LIMIT], true]\n\
[\"hi\", [], [:GREETING]]\n";

#[test]
fn singleton_body_constants_live_on_the_singleton_class_as_in_cruby() {
    let native = emit_and_run::ruby().arg("-e").arg(format!("{SOURCE}\n{SCRIPT}")).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED, "native control");

    let run = emit_and_run::real_blog().write("app/services/singleton_table.rb", SOURCE).run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}

/// A model's `class << self` (the model walk, `ingest::singleton_class`)
/// keeps its constant on the singleton class too.
const ARTICLE: &str = r#"class Article < ApplicationRecord
  has_many :comments, dependent: :destroy

  validates :title, presence: true
  validates :body, presence: true, length: { minimum: 10 }

  class << self
    DEFAULT_TITLE = "untitled"
    def default_title = DEFAULT_TITLE
  end
end
"#;

#[test]
fn a_model_singleton_body_constant_lives_on_the_singleton_class() {
    // The language rule the model shares with any class, run by CRuby.
    let control = "class Article\n  class << self\n    DEFAULT_TITLE = \"untitled\"\n    def default_title = DEFAULT_TITLE\n  end\nend\n";
    let script = "p [Article.default_title, Article.constants.include?(:DEFAULT_TITLE), Article.singleton_class.constants]\n";
    let native = emit_and_run::ruby().arg("-e").arg(format!("{control}{script}")).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected, "[\"untitled\", false, [:DEFAULT_TITLE]]\n", "native control");

    let run = emit_and_run::real_blog().write("app/models/article.rb", ARTICLE).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}
