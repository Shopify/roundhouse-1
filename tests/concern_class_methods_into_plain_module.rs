//! A plain module that includes a Concern with a `class_methods do` block,
//! Core's `ElasticSearch::TranslatableResourceProductIndexing`:
//!
//! ```ruby
//! module TranslatableResourceProductIndexing
//!   include StoreLocalization::ElasticSearch::TranslatableResourceIndexingCommons
//!   class << self; def index_name = "..."; end
//! end
//! ```
//!
//! ActiveSupport::Concern's `append_features` extends the includer with
//! `ClassMethods` whether it is a class or a module. The splice skipped
//! library modules, so the module answered none of the concern's class
//! methods (`undefined method 'should_index?' for module ...`, 10 ids).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r##"# typed: strong
module Indexing
  extend ActiveSupport::Concern

  class_methods do
    #: -> bool
    def should_index? = true

    #: -> String
    def label = "label:#{index_name}"
  end
end
"##;

const HOST: &str = r##"# typed: strong
module ProductIndexing
  include Indexing

  class << self
    #: -> String
    def index_name = "products"
  end
end
"##;

#[test]
fn a_plain_module_including_a_class_methods_concern_answers_its_class_methods() {
    let run = emit_and_run::empty_app()
        .write("app/models/concerns/indexing.rb", CONCERN)
        .write("app/models/product_indexing.rb", HOST)
        .run_ruby("p ProductIndexing.should_index?\np ProductIndexing.label\np ProductIndexing.respond_to?(:should_index?)\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "true\n\"label:products\"\ntrue", "{}", run.stderr);
}

/// Core's `Sales::AgreementBuilder#set_sale_associations_ids`:
/// `sale.association(:source).loaded?` over `Enumerable[Sale]`, where Sale's
/// `source` is a polymorphic belongs_to (no `source_loaded?`) and another
/// model's plain `source` has one. The call stayed on `association(...)`.
#[test]
fn association_loaded_on_a_polymorphic_belongs_to_is_not_flattened_for_an_untyped_receiver() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write(
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "sales", force: :cascade do |t|
    t.integer "source_id"
    t.string "source_type"
  end
  create_table "memberships", force: :cascade do |t|
    t.integer "source_id"
  end
  create_table "sources", force: :cascade do |t|
    t.string "name"
  end
end
"#,
        )
        .write("app/models/sale.rb", "class Sale < ApplicationRecord\n  belongs_to :source, polymorphic: true, optional: true\nend\n")
        .write("app/models/membership.rb", "class Membership < ApplicationRecord\n  belongs_to :source\nend\n")
        .write("app/models/source.rb", "class Source < ApplicationRecord\nend\n")
        .write(
            "app/models/source_linker.rb",
            r##"# typed: true
class SourceLinker
  #: (Enumerable[Sale] sales) -> void
  def link(sales)
    sales.each do |sale|
      sale.source_id = sale.source&.id if sale.association(:source).loaded?
    end
  end
end
"##,
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    let text = std::fs::read_to_string(emitted.join("app/models/source_linker.rb")).unwrap();
    assert!(text.contains("association(:source)"), "{text}");
    assert!(!text.contains("source_loaded?"), "{text}");
}
