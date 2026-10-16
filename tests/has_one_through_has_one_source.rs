//! `has_one :x, through: :link` whose source on the through model is itself
//! a `has_one` (Core's `Shop#shopify_payments_account`, through
//! `primary_shop_legal_entity`, source `ShopLegalEntity#shopify_payments_account`
//! with a hash scope). The reader must join the through model, keep the
//! through scope and the source scope, and answer what native activerecord
//! does — not the plain `Account.where(shop_id: id).first` fallback.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "shops", force: :cascade do |t|
    t.string "name"
  end
  create_table "legal_entities", force: :cascade do |t|
    t.integer "shop_id"
    t.boolean "is_primary", default: false
  end
  create_table "payment_accounts", force: :cascade do |t|
    t.integer "shop_id"
    t.integer "legal_entity_id"
    t.boolean "is_not_archived", default: true
    t.string "status"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "shop",
        "class Shop < ApplicationRecord\n  has_one :primary_legal_entity, -> { where(\"legal_entities.is_primary = true\") }, class_name: \"LegalEntity\", foreign_key: :shop_id\n  has_one :payment_account, through: :primary_legal_entity\nend\n",
    ),
    (
        "legal_entity",
        "class LegalEntity < ApplicationRecord\n  belongs_to :shop\n  has_one :payment_account, -> { where(is_not_archived: true, status: [\"enabled\", \"configuring\"]) }, class_name: \"Billing::PaymentAccount\", foreign_key: :legal_entity_id\nend\n",
    ),
    (
        "billing/payment_account",
        "module Billing\n  class PaymentAccount < ApplicationRecord\n    self.table_name = \"payment_accounts\"\n  end\nend\n",
    ),
];

const PROBE: &str = r##"module HasOneHasOneSourceProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    shop = Shop.create!(name: "a")
    other = Shop.create!(name: "b")
    nolegal = Shop.create!(name: "c")
    archived = Shop.create!(name: "d")
    primary = LegalEntity.create!(shop_id: shop.id, is_primary: true)
    secondary = LegalEntity.create!(shop_id: shop.id, is_primary: false)
    other_primary = LegalEntity.create!(shop_id: other.id, is_primary: true)
    archived_primary = LegalEntity.create!(shop_id: archived.id, is_primary: true)
    # Not on the primary entity: must not be answered.
    Billing::PaymentAccount.create!(shop_id: shop.id, legal_entity_id: secondary.id, status: "enabled")
    # Excluded by the source scope on the primary entity.
    Billing::PaymentAccount.create!(shop_id: shop.id, legal_entity_id: primary.id, status: "disabled")
    Billing::PaymentAccount.create!(shop_id: shop.id, legal_entity_id: primary.id, status: "enabled", is_not_archived: false)
    Billing::PaymentAccount.create!(shop_id: archived.id, legal_entity_id: archived_primary.id, status: "enabled", is_not_archived: false)
    show("only excluded rows") { Shop.find(shop.id).payment_account&.status }
    Billing::PaymentAccount.create!(shop_id: shop.id, legal_entity_id: primary.id, status: "configuring")
    Billing::PaymentAccount.create!(shop_id: other.id, legal_entity_id: other_primary.id, status: "enabled")
    show("source scope + through scope") { Shop.find(shop.id).payment_account&.status }
    show("other shop") { Shop.find(other.id).payment_account&.status }
    show("no legal entity") { Shop.find(nolegal.id).payment_account }
    show("archived only") { Shop.find(archived.id).payment_account }
    show("unsaved owner") { Shop.new.payment_account }
    show("class") { Shop.find(shop.id).payment_account.class.name }
    show("loaded?") do
      found = Shop.find(shop.id)
      found.payment_account
      found.association(:payment_account).loaded?
    end
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/has_one_has_one_source_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_has_one_has_one_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("has_one_has_one_source_probe.rb"), PROBE).unwrap();
    let mut requires = String::new();
    for (name, source) in MODELS {
        let path = dir.join(format!("{name}.rb"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
        requires.push_str(&format!("require_relative {name:?}\n"));
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activerecord\", \"activemodel\", \"activesupport\")\n\
         require \"active_record\"\n\
         ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
         ActiveRecord::Migration.verbose = false\n\
         ActiveRecord::Base.belongs_to_required_by_default = true\n\
         load File.expand_path(\"schema.rb\", __dir__)\n\
         {requires}require_relative \"has_one_has_one_source_probe\"\n\
         HasOneHasOneSourceProbe.run\n"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn a_has_one_through_a_has_one_source_keeps_both_scopes() {
    let run = app().run_ruby("HasOneHasOneSourceProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted has_one through has_one vs native activerecord");
}
