//! `joins(:assoc)` on a relation whose model the compiler cannot see
//! (Core's `with_options(CustomerActiveRecord, options).joins(
//! :email_addresses).find_by(email_addresses: { ... })`), where the
//! association is named differently from its table, against native
//! Rails 8.2 on the same local PostgreSQL database: the where hash keyed
//! by the association name must find the joined table.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE customers (
    id bigserial NOT NULL,
    shop_id bigint NOT NULL,
    name character varying,
    CONSTRAINT customers_pkey PRIMARY KEY (id)
);

CREATE TABLE customer_email_addresses (
    id bigserial NOT NULL,
    customer_id bigint NOT NULL,
    shop_id bigint NOT NULL,
    email_address_normalized character varying,
    CONSTRAINT customer_email_addresses_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_joins_alias_where
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const CUSTOMER: &str = "class Customer < ApplicationRecord\n  has_many :email_addresses, class_name: \"CustomerEmailAddress\"\nend\n";
const EMAIL: &str = "class CustomerEmailAddress < ApplicationRecord\n  belongs_to :customer\nend\n";

const FINDER: &str = r#"class CustomerFinder
  def self.base(klass)
    klass.all
  end

  def self.find_one(shop_id, email)
    base(Customer)
      .joins(:email_addresses)
      .find_by(shop_id: shop_id, email_addresses: { shop_id: shop_id, email_address_normalized: email })
  end

  def self.where_first(shop_id, email)
    base(Customer)
      .where(email_addresses: { email_address_normalized: email })
      .joins(:email_addresses)
      .where(shop_id: shop_id)
      .to_a
  end

  def self.named_owner(shop_id)
    CustomerEmailAddress
      .where(shop_id: shop_id)
      .joins(:customer)
      .where.not(customer: { name: nil })
      .order(:id)
      .to_a
  end

  def self.find_many(shop_id, emails)
    base(Customer)
      .joins(:email_addresses)
      .where(shop_id: shop_id, email_addresses: { shop_id: shop_id, email_address_normalized: emails })
      .order(:id)
      .to_a
  end
end
"#;

const PROBE: &str = r##"class JoinsAliasWhereProbe
  def self.lines
    out = []
    ann = Customer.create!(shop_id: 1, name: "ann")
    bob = Customer.create!(shop_id: 1, name: "bob")
    CustomerEmailAddress.create!(customer_id: ann.id, shop_id: 1, email_address_normalized: "ann@x")
    CustomerEmailAddress.create!(customer_id: bob.id, shop_id: 1, email_address_normalized: "bob@x")
    out << "find_by: #{CustomerFinder.find_one(1, "ann@x")&.name.inspect} #{CustomerFinder.find_one(1, "nobody@x").inspect} #{CustomerFinder.find_one(2, "ann@x").inspect}"
    out << "where: #{CustomerFinder.find_many(1, ["bob@x", "ann@x"]).map(&:name).join(",")}"
    out << "where first: #{CustomerFinder.where_first(1, "bob@x").map(&:name).join(",")}"
    nameless = Customer.create!(shop_id: 1, name: nil)
    CustomerEmailAddress.create!(customer_id: nameless.id, shop_id: 1, email_address_normalized: "none@x")
    out << "belongs_to where.not: #{CustomerFinder.named_owner(1).map(&:email_address_normalized).join(",")}"
    out
  end
end
"##;

const EXPECTED: [&str; 4] = [
    "find_by: \"ann\" nil nil",
    "where: ann,bob",
    "where first: bob",
    "belongs_to where.not: ann@x,bob@x",
];

const LOADS: [&str; 5] = [
    "app/models/application_record",
    "app/models/customer",
    "app/models/customer_email_address",
    "app/models/customer_finder",
    "app/models/joins_alias_where_probe",
];

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/customer.rb", CUSTOMER),
        ("app/models/customer_email_address.rb", EMAIL),
        ("app/models/customer_finder.rb", FINDER),
        ("app/models/joins_alias_where_probe.rb", PROBE),
    ]
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn joins_then_a_where_keyed_by_the_association_matches_native_rails_on_postgresql() {
    let files = files();
    let overlay = files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "joins-alias-where",
        &files,
        &emitted,
        "rh_e2e_joins_alias_where",
        "JoinsAliasWhereProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
