//! A scope declared on an abstract base (through a concern included
//! there) that calls another scope bare runs that call on the RECEIVING
//! class, as Rails does with `self` the current relation's model. The
//! lowering named the owner instead (`ApplicationRecord.force_index`),
//! so a helper that refuses abstract classes (Core's `connection_ext`)
//! raised for every concrete model reaching the scope. Rails answers the
//! relation when a scope body ends in nil or false, so the call chains on.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCOPES: &str = r##"module HintScopes
  extend ActiveSupport::Concern

  included do
    scope :guarded, lambda { |*keys|
      raise NotImplementedError, "#{name}.guarded is not supported on abstract classes" if abstract_class?
      where("1 = 1")
    }

    scope :hinted, ->(*keys) { guarded(*keys) }

    # A scope whose condition does not hold is the relation as it stands.
    scope :maybe, lambda { |flag|
      where("1 = 0") if flag
    }
    scope :maybe_hinted, ->(flag) { maybe(flag) }
  end
end
"##;

#[test]
fn bare_scope_call_in_an_abstract_base_scope_dispatches_on_the_receiving_class() {
    emit_and_run::real_blog()
        .write("app/models/concerns/hint_scopes.rb", SCOPES)
        .edit(
            "app/models/application_record.rb",
            "  primary_abstract_class\n",
            "  primary_abstract_class\n  include HintScopes\n",
        )
        .run_ruby(r#"
Article.create!(title: "A title", body: "A long enough body")
raise "guarded" unless Article.guarded("k").to_a.size == 1
raise "hinted" unless Article.hinted("k").to_a.size == 1
raise "falsy scope" unless Article.maybe(false).order(:id).to_a.size == 1
raise "falsy scope, threaded" unless Article.maybe_hinted(false).order(:id).to_a.size == 1
raise "truthy scope" unless Article.maybe(true).to_a.empty?
raise "chained" unless Article.where(title: "A title").hinted("k").to_a.size == 1
puts "passed"
"#)
        .assert_passes();
}
