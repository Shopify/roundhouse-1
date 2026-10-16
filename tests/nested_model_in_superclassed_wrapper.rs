//! Core's sales/app/models/shipping_line/applied_discount.rb wraps the
//! nested model in `class ShippingLine < GlobalDB::ShopRecord` — the file
//! restates the owner's superclass instead of reopening it bare. The nested
//! model must still be a model with the table Rails computes from the
//! owner (`shipping_line_applied_discounts`), not a class the tree has no
//! schema for.

use roundhouse::det::HashMap;
use std::path::PathBuf;

const SCHEMA: &str = r#"
ActiveRecord::Schema.define(version: 1) do
  create_table "shipping_lines", force: :cascade do |t|
    t.string "title"
  end
  create_table "shipping_line_applied_discounts", force: :cascade do |t|
    t.integer "shipping_line_id"
    t.string "code"
  end
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/global_db.rb", "module GlobalDB\nend\n"),
        (
            "app/models/global_db/shop_record.rb",
            "module GlobalDB\n  class ShopRecord < ApplicationRecord\n    self.abstract_class = true\n  end\nend\n",
        ),
        ("app/models/shipping_line.rb", "class ShippingLine < GlobalDB::ShopRecord\n  self.primary_key = :id\nend\n"),
        (
            "app/models/shipping_line/applied_discount.rb",
            "class ShippingLine < GlobalDB::ShopRecord\n  class AppliedDiscount < GlobalDB::ShopRecord\n    self.primary_key = :id\n  end\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree")
}

#[test]
fn a_model_nested_in_a_superclassed_wrapper_names_the_owner_prefixed_table() {
    let app = app();
    let model = app
        .models
        .iter()
        .find(|m| m.name.0.as_str() == "ShippingLine::AppliedDiscount")
        .expect("the nested class is a model");
    assert_eq!(model.table.0.as_str(), "shipping_line_applied_discounts");
    assert!(model.attributes.fields.contains_key(&roundhouse::Symbol::from("code")), "{:?}", model.attributes);
}

#[test]
fn a_model_that_only_nests_a_differing_class_stays_the_model() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", SCHEMA),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/shipping_line.rb", "class ShippingLine < ApplicationRecord\n  class Error < StandardError\n  end\nend\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    assert!(app.models.iter().any(|m| m.name.0.as_str() == "ShippingLine"));
}
