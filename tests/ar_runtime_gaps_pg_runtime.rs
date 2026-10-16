//! Four Active Record runtime gaps Core's unit tests hit, against Core's
//! pinned activerecord on the same local PostgreSQL database.
//!
//! * A `has_one :x, through:` is not saved with its owner through the
//!   owner's key: Rails' `save_has_one_association` leaves the through
//!   record's key alone (`unless reflection.through_reflection`). The
//!   emitted `after_create` asked the held Catalog for
//!   `product_publication_id`.
//! * `locking_enabled?` (Locking::Optimistic): `lock_optimistically` and a
//!   `lock_version` column. Core's Checkout#save asks it.
//! * `attribute :owner_uuid, :suid`: a type an app registers
//!   (`ActiveRecord::Type.register`) answers `type_for_attribute` instead
//!   of raising "the `:suid` attribute type is not ported".
//! * `serialize :col, type: Hash, coder: JSON` in a Concern's `included do`
//!   on an STI subclass declares the column's type on the subclass; the
//!   compiled accessors consulted none, and the call raised
//!   NotImplementedError as the class loaded.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

use roundhouse::project::BuildTarget;

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n";

const PUBLICATION: &str = "class Publication < ApplicationRecord\n  has_many :product_publications\n  has_one :catalog\nend\n";

const CATALOG: &str = "class Catalog < ApplicationRecord\n  belongs_to :publication\nend\n";

const PRODUCT_PUBLICATION: &str = r#"class ProductPublication < ApplicationRecord
  belongs_to :publication
  has_one :catalog, through: :publication
end
"#;

const CHECKOUT: &str = r#"class Checkout < ApplicationRecord
  def save(*args, **opts)
    self.note = "locked" if locking_enabled?
    super
  end
end
"#;

const PLAIN: &str = "class Plain < ApplicationRecord\nend\n";

const SUID_TYPE: &str = r#"class SuidType < ActiveRecord::Type::String
  def cast(value)
    value.nil? ? nil : "suid-#{value}"
  end
end
"#;

const SUID_INITIALIZER: &str = "ActiveSupport.on_load(:active_record) do\n  ActiveRecord::Type.register(:suid, SuidType, adapter: :postgresql)\nend\n";

const MANDATE: &str = "class Mandate < ApplicationRecord\n  attribute :owner_uuid, :suid\nend\n";

const TOKEN: &str = "class Token < ApplicationRecord\n  self.inheritance_column = \"kind\"\nend\n";

const TOKENIZED: &str = r#"module Tokenized
  extend ActiveSupport::Concern

  included do
    serialize :sensitive_data, type: Hash, coder: JSON
  end
end
"#;

const REMOTE_TOKEN: &str = "class RemoteToken < Token\n  include Tokenized\nend\n";

const PROBE: &str = r##"class ArGapsProbe
  def self.lines
    out = []
    pub = Publication.create!(name: "p")
    Catalog.create!(publication: pub, name: "c")
    pp = ProductPublication.create!(publication: pub)
    pp = ProductPublication.find(pp.id)
    out << "through reader: #{pp.catalog.name}"
    out << "through save: #{pp.update(note: "n")} #{pp.reload.note}"
    out << "locking: #{!!Checkout.locking_enabled?} #{!!Checkout.new.locking_enabled?} #{Checkout.locking_column}"
    out << "no column: #{!!Plain.locking_enabled?} #{!!Plain.new.locking_enabled?}"
    c = Checkout.new
    c.save
    out << "save: #{c.note}"
    t = Mandate.type_for_attribute("owner_uuid")
    out << "registered type: #{t.class} #{t.cast("x")}"
    r = RemoteToken.create!(sensitive_data: { "a" => 1 })
    r = RemoteToken.find(r.id)
    out << "serialized: #{r.sensitive_data.inspect} #{RemoteToken.type_for_attribute("sensitive_data").class.name.split("::").last}"
    out
  end
end
"##;

/// What Core's activerecord answers on PostgreSQL, so two identical wrong
/// answers still fail.
const EXPECTED: &[&str] = &[
    "through reader: c",
    "through save: true n",
    "locking: true true lock_version",
    "no column: false false",
    "save: locked",
    "registered type: SuidType suid-x",
    r#"serialized: {"a" => 1} Serialized"#,
];

const STRUCTURE: &str = r#"CREATE TABLE publications (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT publications_pkey PRIMARY KEY (id)
);
CREATE TABLE catalogs (
    id bigserial NOT NULL,
    publication_id bigint,
    name character varying,
    CONSTRAINT catalogs_pkey PRIMARY KEY (id)
);
CREATE TABLE product_publications (
    id bigserial NOT NULL,
    publication_id bigint,
    note character varying,
    CONSTRAINT product_publications_pkey PRIMARY KEY (id)
);
CREATE TABLE checkouts (
    id bigserial NOT NULL,
    note character varying,
    lock_version integer DEFAULT 0 NOT NULL,
    CONSTRAINT checkouts_pkey PRIMARY KEY (id)
);
CREATE TABLE plains (
    id bigserial NOT NULL,
    CONSTRAINT plains_pkey PRIMARY KEY (id)
);
CREATE TABLE mandates (
    id bigserial NOT NULL,
    owner_uuid character varying,
    CONSTRAINT mandates_pkey PRIMARY KEY (id)
);
CREATE TABLE tokens (
    id bigserial NOT NULL,
    kind character varying,
    sensitive_data character varying(65535),
    CONSTRAINT tokens_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str =
    "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_ar_runtime_gaps\n";

/// In load order, as native Rails requires them.
const MODELS: &[(&str, &str)] = &[
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/publication.rb", PUBLICATION),
    ("app/models/catalog.rb", CATALOG),
    ("app/models/product_publication.rb", PRODUCT_PUBLICATION),
    ("app/models/checkout.rb", CHECKOUT),
    ("app/models/plain.rb", PLAIN),
    ("app/models/suid_type.rb", SUID_TYPE),
    ("config/initializers/suid.rb", SUID_INITIALIZER),
    ("app/models/mandate.rb", MANDATE),
    ("app/models/token.rb", TOKEN),
    ("app/models/tokenized.rb", TOKENIZED),
    ("app/models/remote_token.rb", REMOTE_TOKEN),
    ("app/models/ar_gaps_probe.rb", PROBE),
];

fn files() -> Vec<(&'static str, &'static str)> {
    let mut files = MODELS.to_vec();
    files.extend([
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
    ]);
    files
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn ar_runtime_gaps_match_native_rails_on_postgresql() {
    let files = files();
    let (emitted, errors) =
        files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text)).emit(BuildTarget::Ruby);
    let real: Vec<_> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    let loads: Vec<&str> = MODELS.iter().map(|(path, _)| path.trim_end_matches(".rb")).collect();
    let (native, emitted) =
        pg_probe::native_and_emitted("ar-runtime-gaps", &files, &emitted, "rh_e2e_ar_runtime_gaps", "ArGapsProbe", &loads);
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
