//! An `included do` block of a concern runs where the class body has its
//! `include`, so it reads the class state the body set above it. Core's
//! `GraphApi::Admin::SavedView` writes `self.model = -> { ::SavedView }`
//! and then `include SavedViews::AppliedIndexTableFiltersField`, whose
//! block declares `field …, preload: :saved_search`: a preload reads the
//! model. The include bucket wrote the include, and with it the carried
//! `field`, above the assignment, so every GraphQL schema build died with
//! "SavedView.model accessed without being set".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "app/models/object_type.rb",
            "class ObjectType\n  def self.field(name, preload: nil)\n    (@fields ||= []) << [name, preload && model_name_for(preload)]\n  end\n\n  def self.fields\n    @fields\n  end\n\n  def self.model_name_for(preload)\n    \"#{model.name}##{preload}\"\n  end\nend\n",
        )
        .write(
            "app/models/model_dsl.rb",
            "module ModelDsl\n  def model=(value)\n    @model = value\n  end\n\n  def model\n    raise \"#{name}.model accessed without being set\" if @model.nil?\n\n    @model.is_a?(Proc) ? @model.call : @model\n  end\nend\n",
        )
        .write("app/models/saved_record.rb", "class SavedRecord\nend\n")
        .write(
            "app/models/filters_field.rb",
            "module FiltersField\n  extend ActiveSupport::Concern\n\n  included do\n    field :applied_filters, preload: :saved_search\n  end\nend\n",
        )
        .write(
            "app/models/saved_view.rb",
            "class SavedView < ObjectType\n  extend ModelDsl\n\n  self.model = -> { SavedRecord }\n  include FiltersField\n  field :id\nend\n",
        )
}

#[test]
fn a_concerns_block_reads_the_model_the_body_set_above_its_include() {
    let run = app().run_ruby("p SavedView.fields\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[[:applied_filters, \"SavedRecord#saved_search\"], [:id, nil]]\n", "{}", run.stderr);
}
