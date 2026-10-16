//! `has_one`/`has_many :x, through: :y` where the first hop `y` is a
//! `belongs_to` (Core's `BalanceTransaction#sp_order`, `has_one :sp_order,
//! through: :charge, source: :order`, where `Charge#order` is itself a
//! `has_one … through: :order_transaction`). The reader must join from
//! the through row the owner's foreign key names, not fall back to the
//! plain `Order.where(balance_transaction_id: id)` that names a column
//! the table does not have.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "orders", force: :cascade do |t|
    t.string "name"
  end
  create_table "order_transactions", force: :cascade do |t|
    t.integer "order_id"
  end
  create_table "charges", force: :cascade do |t|
    t.integer "order_transaction_id"
  end
  create_table "balance_transactions", force: :cascade do |t|
    t.integer "charge_id"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ("order", "class Order < ApplicationRecord\n  has_many :order_transactions\nend\n"),
    (
        "order_transaction",
        "class OrderTransaction < ApplicationRecord\n  belongs_to :order, optional: true\n  has_many :charges\nend\n",
    ),
    (
        "charge",
        "class Charge < ApplicationRecord\n  belongs_to :order_transaction, optional: true\n  has_one :order, through: :order_transaction\n  has_many :charges_of_transaction, through: :order_transaction, source: :charges\nend\n",
    ),
    (
        "balance_transaction",
        "class BalanceTransaction < ApplicationRecord\n  belongs_to :charge, optional: true\n  has_one :sp_order, through: :charge, source: :order\n  has_one :transaction_of_charge, through: :charge, source: :order_transaction\nend\n",
    ),
];

const PROBE: &str = r##"module ThroughBelongsToProbe
  def self.show(label)
    value = begin
      yield
    rescue StandardError => e
      [e.class.name, e.message[0, 120]]
    end
    puts "#{label}: #{value.inspect}"
  end

  def self.run
    order = Order.create!(name: "one")
    other = Order.create!(name: "two")
    tx = OrderTransaction.create!(order_id: order.id)
    other_tx = OrderTransaction.create!(order_id: other.id)
    orderless_tx = OrderTransaction.create!
    charge = Charge.create!(order_transaction_id: tx.id)
    sibling = Charge.create!(order_transaction_id: tx.id)
    other_charge = Charge.create!(order_transaction_id: other_tx.id)
    orderless = Charge.create!(order_transaction_id: orderless_tx.id)
    detached = Charge.create!
    bt = BalanceTransaction.create!(charge_id: charge.id)
    other_bt = BalanceTransaction.create!(charge_id: other_charge.id)
    orderless_bt = BalanceTransaction.create!(charge_id: orderless.id)
    detached_bt = BalanceTransaction.create!(charge_id: detached.id)
    no_charge_bt = BalanceTransaction.create!

    show("has_one through belongs_to") { Charge.find(charge.id).order&.name }
    show("other charge") { Charge.find(other_charge.id).order&.name }
    show("order-less transaction") { Charge.find(orderless.id).order }
    show("no transaction") { Charge.find(detached.id).order }
    show("nested through belongs_to") { BalanceTransaction.find(bt.id).sp_order&.name }
    show("nested other") { BalanceTransaction.find(other_bt.id).sp_order&.name }
    show("nested order-less") { BalanceTransaction.find(orderless_bt.id).sp_order }
    show("nested no transaction") { BalanceTransaction.find(detached_bt.id).sp_order }
    show("nested no charge") { BalanceTransaction.find(no_charge_bt.id).sp_order }
    show("belongs_to source row") { BalanceTransaction.find(bt.id).transaction_of_charge&.id == tx.id }
    show("has_many through belongs_to") { Charge.find(charge.id).charges_of_transaction.map(&:id).sort == [charge.id, sibling.id] }
    show("has_many other") { Charge.find(other_charge.id).charges_of_transaction.map(&:id) == [other_charge.id] }
    show("has_many none") { Charge.find(detached.id).charges_of_transaction.to_a }
    show("class") { BalanceTransaction.find(bt.id).sp_order.class.name }
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/through_belongs_to_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    app
}

fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_through_belongs_to_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.rb"), SCHEMA).unwrap();
    std::fs::write(dir.join("through_belongs_to_probe.rb"), PROBE).unwrap();
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
         {requires}require_relative \"through_belongs_to_probe\"\n\
         ThroughBelongsToProbe.run\n"
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
fn a_through_whose_first_hop_is_a_belongs_to_joins_from_the_foreign_key() {
    let run = app().run_ruby("ThroughBelongsToProbe.run");
    run.assert_passes();
    assert_eq!(run.stdout, native(), "emitted through-belongs_to readers vs native activerecord");
}
