//! Two emitted-constructor/parent shapes from Core's sweep-1 (round 4).
//!
//! 1. A tableless class that includes `ActiveModel::Model` and
//!    `ActiveModel::Attributes` and declares no `initialize`
//!    (`SmartGrid::Tile::TileModel`, built by `type.new(**attributes)`).
//!    The lowering drops the Model include, so the runtime's
//!    `Attributes#initialize(*)` forwarded the hash to Object#initialize:
//!    "wrong number of arguments (given 1, expected 0)". The class now
//!    carries the constructor Model supplies, assigning through
//!    `assign_attributes` after `super()` has seeded the attribute set.
//! 2. `class Card < self` nested in a model (`PaymentDetails::Card`), an
//!    STI subclass written beside its base. The superclass `self` had no
//!    constant path, so the class emitted bare (`class Card`, an Object
//!    subclass) and `find_sti_class` raised "Invalid single-table
//!    inheritance type: PaymentDetails::Card is not a subclass of
//!    PaymentDetails".
//! 3. `create!` / `create` given an Array of attribute hashes
//!    (persistence.rb) create one record each.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_model_init_sti
"#;

const STRUCTURE: &str = r#"CREATE TABLE payments (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    CONSTRAINT payments_pkey PRIMARY KEY (id)
);
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const TILE_MODEL: &str = r#"module Tiles
  class TileModel
    include ActiveModel::Model
    include ActiveModel::Attributes

    attribute :style, :string
  end
end
"#;

const FEE_TILE: &str = r#"module Tiles
  class FeeTile < TileModel
    attribute :amount, :integer
  end
end
"#;

const PAYMENT: &str = r#"class Payment < ApplicationRecord
  class Card < self
    def brand
      "visa"
    end
  end

  class Local < self
  end

  class Konbini < Local
  end
end
"#;

fn overlay(files: &[(&str, &str)]) -> emit_and_run::Overlay {
    [
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("Gemfile.lock", LOCK),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
        ("app/models/application_record.rb", APPLICATION_RECORD),
    ]
    .iter()
    .chain(files)
    .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn emit(files: &[(&str, &str)]) -> emit_and_run::Emitted {
    let (emitted, errors) = overlay(files).emit(roundhouse::project::BuildTarget::Ruby);
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{real:?}");
    emitted
}

#[test]
fn an_attributes_model_without_initialize_takes_the_attribute_hash() {
    let emitted = emit(&[("app/models/tiles/tile_model.rb", TILE_MODEL), ("app/models/tiles/fee_tile.rb", FEE_TILE)]);
    let src = std::fs::read_to_string(emitted.join("app/models/tiles/tile_model.rb")).unwrap();
    assert!(src.contains("include ActiveModel::Attributes"), "{src}");
    let init = src.split("def initialize(attributes = {})").nth(1).unwrap_or_else(|| panic!("no attribute-hash initialize:\n{src}"));
    let super_at = init.find("super()").unwrap_or_else(|| panic!("no super():\n{src}"));
    let assign_at = init.find("assign_attributes(attributes)").unwrap_or_else(|| panic!("no assign_attributes:\n{src}"));
    assert!(super_at < assign_at, "the attribute set is seeded before the assignment:\n{src}");
}

#[test]
fn a_nested_class_below_self_is_a_subclass_of_the_enclosing_model() {
    let emitted = emit(&[("app/models/payment.rb", PAYMENT)]);
    let mut all = std::fs::read_to_string(emitted.join("app/models/payment.rb")).unwrap_or_default();
    for entry in std::fs::read_dir(emitted.join("app/models/payment")).unwrap() {
        all.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    assert!(!all.is_empty(), "no emitted payment sources under {}", emitted.display());
    assert!(all.contains("class Card < Payment"), "Card is not below Payment:\n{all}");
    assert!(all.contains("class Local < Payment"), "Local is not below Payment:\n{all}");
    assert!(!all.contains("class Card\n"), "bare `class Card` (an Object subclass):\n{all}");
}

#[test]
fn class_create_with_an_array_creates_one_record_each() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/spinel/cruby/active_record_callbacks.rb"),
    )
    .unwrap();
    for method in ["create!", "create"] {
        let def = format!("def self.{method}(attrs = {{}})\n      return attrs.map {{ |each_attrs| {method}(each_attrs) }} if attrs.is_a?(Array)");
        assert!(src.contains(&def), "{method} lacks the Array form");
    }
}
