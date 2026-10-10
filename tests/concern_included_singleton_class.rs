//! `included do class << self … end end` in an `ActiveSupport::Concern`.
//!
//! Core's `MerchantSubscriptions::Concerns::Serializable` writes its class
//! side this way: Rails class_evals the block on each includer, so the
//! `class << self` there opens the includer's singleton class and its defs
//! (`load`, `from_attributes`, `dump`) are the includer's class methods.
//! The block has no direct `def`, so it was not read as a definition
//! carrier at all: the emitted concern was an empty module and
//! `Capabilities.from_attributes` raised NoMethodError (10 unit-1000 ids).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r##"module Serial
  extend ActiveSupport::Concern

  included do
    class << self
      def load(text)
        return if text.nil?

        from_attributes(text.split(","))
      end

      def from_attributes(parts)
        new(parts.first)
      end

      def tag = "#{name}!"
    end

    def self.direct = :direct
  end

  def initialize(first = nil)
    @first = first
  end

  def first = @first
end
"##;

const PLAIN: &str = r##"class Plain
  include Serial

  def initialize(first = nil)
    super
  end
end
"##;

const SCRIPT: &str = "p Plain.load(\"a,b\").first\np Plain.load(nil)\np Plain.tag\np Plain.direct\n";

#[test]
fn a_singleton_class_in_an_included_block_defines_the_plain_includers_class_methods() {
    let run = emit_and_run::empty_app()
        .write("app/models/concerns/serial.rb", CONCERN)
        .write("app/models/plain.rb", PLAIN)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "\"a\"\nnil\n\"Plain!\"\n:direct", "{}", run.stderr);
}

const AR_CONCERN: &str = r##"module Labeled
  extend ActiveSupport::Concern

  included do
    class << self
      def labeled(title)
        new(title: title)
      end

      def label_for(title) = "#{name}:#{labeled(title).title}"
    end
  end
end
"##;

#[test]
fn a_singleton_class_in_an_included_block_defines_the_models_class_methods() {
    let run = emit_and_run::real_blog()
        .write("app/models/concerns/labeled.rb", AR_CONCERN)
        .edit("app/models/article.rb", "class Article < ApplicationRecord\n", "class Article < ApplicationRecord\n  include Labeled\n")
        .run_ruby("p Article.labeled(\"x\").title\np Article.label_for(\"y\")\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "\"x\"\n\"Article:y\"", "{}", run.stderr);
}
