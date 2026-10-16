//! A String `blank?` / `present?` in a method that also holds a relation
//! chain (`Model.where(id: attr)`) stays Rails' `blank?` on the CRuby tree.
//! `scope_chain::rewrite` writes a fresh send over a rewritten call and
//! dropped the `IrHint::StringBlank` that `lower::blank` put on the
//! `strip.empty?`, so the emitter left `(x || "").strip.empty?`, which raises
//! `undefined method 'strip' for an instance of Integer` on a uuid attribute
//! that holds an Integer (Core's `GiftCard#validate_gift_card_line_item`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CARD: &str = r#"class Card < ApplicationRecord
  def item_known?
    if item_ref.present?
      Card.where(id: item_ref).exists?
    end
  end

  def item_missing?
    item_ref.blank? || Card.where(id: item_ref).none?
  end
end
"#;

#[test]
fn a_string_blank_beside_a_relation_chain_is_rails_blank() {
    let (emitted, _errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_e2e_blank_chain\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write(
            "db/structure.sql",
            "CREATE TABLE cards (\n    id bigserial NOT NULL,\n    item_ref character varying(255),\n    CONSTRAINT cards_pkey PRIMARY KEY (id)\n);\n",
        )
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/card.rb", CARD)
        .emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/card.rb")).expect("card.rb");
    assert!(text.contains("(item_ref || \"\").blank?"), "{text}");
    assert!(!text.contains("strip.empty?"), "{text}");
}
