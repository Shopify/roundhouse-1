//! `states.find { .. }` on an Array/relation of models is `Enumerable#find`
//! (an element or nil), not the finder `find(id)` that answers the model or
//! raises. Typing it as the bare model folded `state.present?` to `if true`
//! (Fulfillments::FulfillmentItemStateHandler#upsert_in_bulk then called
//! `quantity` on nil).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE item_states (
    id bigserial NOT NULL,
    line_id bigint,
    quantity bigint,
    CONSTRAINT item_states_pkey PRIMARY KEY (id)
);
"#;

const HANDLER: &str = r#"class Handler
  class << self
    #: (Hash[Integer, Array[ItemState]], Integer) -> Array[ItemState]
    def by_line(by_id, line)
      states = by_id[1] || []
      state = states.find { |s| s.line_id == line } || states.find { |s| s.line_id.nil? }
      out = []
      out << state if state.present?
      out
    end

    #: (Integer) -> ItemState
    def by_id(id)
      ItemState.find(id)
    end
  end
end
"#;

#[test]
fn a_block_find_over_models_stays_nilable_and_the_id_finder_does_not() {
    let (emitted, _errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        )
        .write("app/models/item_state.rb", "class ItemState < ApplicationRecord\nend\n")
        .write("app/models/handler.rb", HANDLER)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/handler.rb")).unwrap();
    assert!(!text.contains("if true"), "present? folded to a constant:\n{text}");
    assert!(
        text.contains("out << state if !(state.nil?)"),
        "the nil guard on the block find must stay:\n{text}"
    );
    assert!(text.contains("ItemState.find(id)"), "the id finder is untouched:\n{text}");
}
