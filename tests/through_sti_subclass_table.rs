//! `has_one :through` onto an STI subclass joins the BASE table. Core's
//! `Shop has_one :express_gateway, through: :primary_shop_legal_entity`
//! targets `ExpressGateway < PaymentGateway`, whose rows live in
//! `payment_gateways`. The subclass is a library class, not a model, so
//! the join resolver fell back to the class-name table and wrote
//! `ON shop_legal_entities.id = express_gateways.shop_legal_entity_id`:
//! PostgreSQL answered `missing FROM-clause entry for table
//! "express_gateways"` and `EnqueuePaypalMerchantNotification` failed.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect()
}

fn app() -> roundhouse::App {
    ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "shops", force: :cascade do |t|
    t.string "name", null: false
  end
  create_table "shop_legal_entities", force: :cascade do |t|
    t.integer "shop_id", null: false
    t.boolean "primary", null: false
  end
  create_table "payment_gateways", force: :cascade do |t|
    t.integer "shop_legal_entity_id", null: false
    t.string "type", null: false
  end
end
"#,
        ),
        (
            "app/models/shop.rb",
            r#"class Shop < ApplicationRecord
  has_many :shop_legal_entities
  has_one :primary_shop_legal_entity, -> { where(primary: true) }, class_name: "ShopLegalEntity"
  has_one :express_gateway, through: :primary_shop_legal_entity
end
"#,
        ),
        (
            "app/models/shop_legal_entity.rb",
            r#"class ShopLegalEntity < ApplicationRecord
  belongs_to :shop
  has_one :express_gateway, foreign_key: :shop_legal_entity_id
end
"#,
        ),
        (
            "app/models/payment_gateway.rb",
            r#"class PaymentGateway < ApplicationRecord
  belongs_to :shop_legal_entity
end
"#,
        ),
        ("app/models/express_gateway.rb", "class ExpressGateway < PaymentGateway\nend\n"),
    ]))
    .expect("ingest sti through app")
}

#[test]
fn a_through_onto_an_sti_subclass_joins_the_base_table() {
    let files = ruby::emit_lowered_models(&app());
    let shop = files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("shop.rb"))
        .map(|f| f.content.clone())
        .expect("shop.rb emitted");
    assert!(
        shop.contains("shop_legal_entities.id = payment_gateways.shop_legal_entity_id"),
        "the through join must key the base table:\n{shop}"
    );
    assert!(
        !shop.contains("express_gateways."),
        "no join against the class-name table of the STI subclass:\n{shop}"
    );
}
