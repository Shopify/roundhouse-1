//! An association names its class relative to its owner, as Rails'
//! `compute_type` reads it: `has_one :measurement` on `Shipping::Box`
//! is `Shipping::Box::Measurement` when that model exists, before
//! `Shipping::Measurement` and the top-level `Measurement`.
//!
//! The target was the bare camelized name, so every read through a
//! namespaced association dispatched on a class that doesn't exist
//! (`no known method … on Measurement?`), as Core's shipping boxes do.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::dialect::Association;
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"
ActiveRecord::Schema.define(version: 1) do
  create_table "shipping_boxes", force: :cascade do |t|
    t.string "name"
  end
  create_table "shipping_box_measurements", force: :cascade do |t|
    t.integer "shipping_box_id"
    t.string "dimension_unit"
  end
  create_table "measurements", force: :cascade do |t|
    t.integer "grams"
  end
end
"#;

const BOX: &str = r#"
module Shipping
  class Box < ApplicationRecord
    self.table_name = "shipping_boxes"
    has_one :measurement

    #: -> untyped
    def unit = measurement.dimension_unit.zzz
  end
end
"#;

const BOX_MEASUREMENT: &str = r#"
module Shipping
  class Box::Measurement < ApplicationRecord
    self.table_name = "shipping_box_measurements"
    belongs_to :shipping_box, class_name: "::Shipping::Box"
    belongs_to :measurement, class_name: "::Measurement"
  end
end
"#;

const WEIGHED: &str = r#"
module Weighed
  extend ActiveSupport::Concern

  included do
    has_one :weight, class_name: "::Measurement"
  end
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/measurement.rb", "class Measurement < ApplicationRecord\nend\n"),
        ("app/models/shipping/box.rb", BOX),
        ("app/models/shipping/box/measurement.rb", BOX_MEASUREMENT),
        ("app/models/concerns/weighed.rb", WEIGHED),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn targets(app: &roundhouse::App, model: &str) -> Vec<String> {
    app.models
        .iter()
        .find(|m| m.name.0.as_str() == model)
        .unwrap_or_else(|| panic!("no model {model}"))
        .associations()
        .map(|a| match a {
            Association::BelongsTo { target, .. }
            | Association::HasOne { target, .. }
            | Association::HasMany { target, .. }
            | Association::HasAndBelongsToMany { target, .. } => target.0.as_str().to_string(),
        })
        .collect()
}

/// The owner's own namespace wins over the top-level model of the
/// same name, and the read types through it.
#[test]
fn the_owner_namespace_is_searched_before_the_top_level() {
    let app = app();
    assert_eq!(targets(&app, "Shipping::Box"), ["Shipping::Box::Measurement"]);
    let errors: Vec<String> = diagnose(&app)
        .into_iter()
        .map(|d| d.to_string())
        .filter(|d| d.starts_with("error"))
        .collect();
    assert!(errors.iter().any(|e| e.contains("no known method `zzz` on String")), "{errors:?}");
}

/// A rooted `class_name:` names the top-level path as written.
#[test]
fn a_rooted_class_name_is_not_searched() {
    assert_eq!(targets(&app(), "Shipping::Box::Measurement"), ["Shipping::Box", "Measurement"]);
}

/// A concern's own declaration, which the analyzer reads per includer,
/// is unrooted too: a `::Measurement` receiver is no class.
#[test]
fn a_concern_rooted_class_name_is_unrooted() {
    let app = app();
    let items = app
        .concern_model_items
        .get(&roundhouse::ClassId(roundhouse::Symbol::from("Weighed")))
        .expect("concern items");
    let targets: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            roundhouse::dialect::ModelBodyItem::Association {
                assoc: Association::HasOne { target, .. }, ..
            } => Some(target.0.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(targets, ["Measurement"]);
}
