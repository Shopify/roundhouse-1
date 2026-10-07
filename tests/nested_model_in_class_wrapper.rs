//! `module Shipping; class Box; class Measurement < ApplicationRecord`
//! — a model nested in a class its file reopens only as a namespace.
//!
//! Core's shipping/box/measurement.rb is written so. Model ingest and
//! the file classification took the first class in the file, `Box`
//! (no superclass): the file became a library class, `Measurement` a
//! plain nested class with no table, and every column read on it
//! failed, including through its owner's `has_one :measurement`.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::emit::ruby;
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

const MEASUREMENT: &str = r#"
module Shipping
  class Box
    class Measurement < ApplicationRecord
      self.table_name = "shipping_box_measurements"
      belongs_to :shipping_box, class_name: "::Shipping::Box"

      #: -> untyped
      def unit = dimension_unit.yyy
    end
  end
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/shipping/box.rb", BOX),
        ("app/models/shipping/box/measurement.rb", MEASUREMENT),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn errors(app: &roundhouse::App) -> Vec<String> {
    diagnose(app)
        .into_iter()
        .map(|d| d.to_string())
        .filter(|d| d.starts_with("error"))
        .collect()
}

/// The nested class is the model: it binds its table, so a column
/// reads its type, and it emits a row class.
#[test]
fn the_nested_class_is_the_model_and_binds_its_table() {
    let app = app();
    assert!(app.models.iter().any(|m| m.name.0.as_str() == "Shipping::Box::Measurement"));
    let errors = errors(&app);
    assert!(errors.iter().any(|e| e.contains("no known method `yyy` on String")), "{errors:?}");
    assert!(!errors.iter().any(|e| e.contains("`dimension_unit`")), "{errors:?}");
    let files = ruby::emit_spinel(&app);
    assert!(
        files.iter().any(|f| f.path.to_string_lossy().ends_with("shipping/box/measurement_row.rb")),
        "no row class for the nested model"
    );
}

/// The owner's `has_one :measurement` reaches the nested model.
#[test]
fn the_owner_association_reads_through_the_nested_model() {
    let errors = errors(&app());
    assert!(errors.iter().any(|e| e.contains("no known method `zzz` on String")), "{errors:?}");
    assert!(!errors.iter().any(|e| e.contains("on Measurement") || e.contains("on Shipping::Box::Measurement")), "{errors:?}");
}
