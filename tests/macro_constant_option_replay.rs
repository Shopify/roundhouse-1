//! A class macro that retypes a column is replayed as written when its
//! keyword options are constants, not only literals: Core's `money_column
//! :total_price_usd, currency: USD_CURRENCY_CODE` (shopify-money) was
//! refused ("reaches `attribute`, which the lowered model declares
//! statically"), so `total_price_usd=` kept the generated writer and
//! stored the `Money` it was given, which `numericality` then read as
//! "isn't a number" (RefundTest `Order#update!`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTURE: &str = r#"CREATE TABLE orders (
    id bigserial NOT NULL,
    total_price_usd numeric,
    amount numeric,
    CONSTRAINT orders_pkey PRIMARY KEY (id)
);
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.priced(*columns, currency: nil, currency_column: nil)
    columns.flatten.each do |column|
      column_string = column.to_s.freeze
      attribute(column_string, :decimal)
    end
  end
end
"#;

const ORDER: &str = r#"class Order < ApplicationRecord
  USD_CURRENCY_CODE = "USD"

  priced :total_price_usd, currency: USD_CURRENCY_CODE
  priced :amount, currency_column: :currency
end
"#;

fn emitted_order() -> String {
    let app = [
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/order.rb", ORDER),
    ]
    .iter()
    .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, _) = app.emit(roundhouse::project::BuildTarget::Ruby);
    std::fs::read_to_string(emitted.join("app/models/order.rb")).expect("order.rb emitted")
}

#[test]
fn a_constant_option_does_not_stop_the_macro_replay() {
    let out = emitted_order();
    assert!(out.contains("priced :amount, currency_column: :currency"), "{out}");
    assert!(out.contains("priced :total_price_usd, currency: Order::USD_CURRENCY_CODE"), "{out}");
}
