//! A model's `database_documentation do |dd| ... end` (database_documentation
//! 3.2.0), against the gem itself on native Rails 8.2 and the same local
//! PostgreSQL database.
//!
//! Core's 230 models that `include
//! DatabaseDocumentation::ModelMetadata::Metadata` call the concern's
//! `database_documentation` macro in their body. It only records: the
//! block runs against an annotator, and the merged annotation lands in the
//! class attribute `annotation` (Core's privacy purge reads
//! `record.class.annotation.fields` and each field's `pii?`). The gem loads
//! through zeitwerk and ActiveSupport, so the PostgreSQL tree carries a
//! plain-Ruby port of it (runtime/spinel/database_documentation.rb,
//! `project::GEM_RUNTIME_PORTS`), loaded where the app's Bundler.require
//! would load the gem; the include resolves to the port and the macro call
//! is replayed in the class body through it
//! (`model_to_library::gem_port_macro_calls`).
//!
//! Before: every such include was refused as a Rails-dependent gem's
//! module, and every macro call as "model DSL call not lowered; the class
//! does not run it".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE customers (
    id bigserial NOT NULL,
    email character varying,
    address1 character varying,
    zip character varying,
    note character varying,
    token character varying,
    CONSTRAINT customers_pkey PRIMARY KEY (id)
);
CREATE TABLE notes (
    id bigserial NOT NULL,
    body character varying,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_database_documentation
"#;

/// The app's Bundler.require, which loads the gem (the port, emitted).
const APPLICATION: &str = r#"require_relative "boot"
require "rails/all"
Bundler.require(*Rails.groups)

module DdProbe
  class Application < Rails::Application
  end
end
"#;

const GEMFILE: &str = "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"database_documentation\", \"~> 3.0\"\n";

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const CUSTOMER: &str = r#"class Customer < ApplicationRecord
  include DatabaseDocumentation::ModelMetadata::Metadata

  database_documentation do |dd|
    dd.field(:address1, datatype: DatabaseDocumentation::Datatypes::StreetAddress)
    dd.field(:zip, datatype: DatabaseDocumentation::Datatypes::PostalCode, description: "Postal code.")
    dd.field(:note, is_pii: false)
    dd.field(
      :token,
      datatype: [DatabaseDocumentation::Datatypes::Credentials, DatabaseDocumentation::Datatypes::OtherData],
      is_pii: true,
    )
    dd.description("A customer.")
  end

  database_documentation do |dd|
    dd.field(:email, datatype: DatabaseDocumentation::Datatypes::EmailAddress, description: "Contact.")
    dd.enable_cdc_extraction
  end

  def pii_columns
    self.class.annotation.fields.filter_map { |name, field| name.to_s if field.pii? }
  end
end
"#;

/// An STI-free subclass inherits the class attribute.
const VIP_CUSTOMER: &str = "class VipCustomer < Customer\nend\n";

/// The macro `self.`-called, and a model that never documents.
const NOTE: &str = r#"class Note < ApplicationRecord
  include DatabaseDocumentation::ModelMetadata::Metadata

  self.database_documentation do |dd|
    dd.field(:body, datatype: DatabaseDocumentation::Datatypes::MessageContents)
  end
end
"#;

const PLAIN: &str = "class Plain < ApplicationRecord\n  self.table_name = \"notes\"\n  include DatabaseDocumentation::ModelMetadata::Metadata\nend\n";

const PROBE: &str = r##"class DocProbe
  def self.show(klass)
    a = klass.annotation
    return "#{klass.name}: nil" unless a

    fields = a.fields.map do |name, f|
      "#{name}=#{f.datatypes.map(&:name).inspect}/#{f.pii?}/#{f.description.inspect}"
    end
    "#{klass.name}: cdc=#{a.cdc_extraction_enabled?} desc=#{a.description.inspect} #{fields.join(",")}"
  end

  def self.lines
    out = [show(Customer), show(VipCustomer), show(Note), show(Plain)]
    ma = Customer.model_annotations
    out << "model_annotations: #{ma.table_name} #{ma.model_class} #{ma.fields.keys.inspect} #{ma.table_description.inspect}"
    out << "email: #{Array(ma.fields[:email][:datatype]).map(&:name).inspect} #{ma.fields[:email][:is_pii]} #{ma.fields[:email][:description].inspect}"
    out << "vip model_annotations: #{VipCustomer.model_annotations.inspect}"
    out << "pii: #{Customer.new.pii_columns.inspect}"
    out << "drop non-pii: #{DatabaseDocumentation.config.drop_non_pii_annotations}"
    begin
      Note.database_documentation { |dd| dd.field(:body, datatype: "text") }
    rescue ArgumentError => e
      out << "error: #{e.message}"
    end
    c = Customer.create!(email: "a@b.c", zip: "K1A")
    out << "row: #{Customer.find(c.id).zip}"
    out
  end
end
"##;

/// What database_documentation 3.2.0 answers natively (checked on the task
/// PostgreSQL), so two identical wrong answers still fail.
const EXPECTED: [&str; 11] = [
    r#"Customer: cdc=true desc="A customer." email=[:EMAIL_ADDRESS]/true/"Contact.",address1=[:STREET_ADDRESS]/true/nil,zip=[:POSTAL_CODE]/true/"Postal code.",note=[:OTHER_DATA]/false/nil,token=[:OTHER_IDENTIFIER]/true/nil"#,
    r#"VipCustomer: cdc=true desc="A customer." email=[:EMAIL_ADDRESS]/true/"Contact.",address1=[:STREET_ADDRESS]/true/nil,zip=[:POSTAL_CODE]/true/"Postal code.",note=[:OTHER_DATA]/false/nil,token=[:OTHER_IDENTIFIER]/true/nil"#,
    "Note: cdc=false desc=nil body=[:MESSAGE_CONTENTS]/true/nil",
    "Plain: nil",
    "model_annotations: customers Customer [:email] nil",
    r#"email: [:EMAIL_ADDRESS] true "Contact.""#,
    "vip model_annotations: nil",
    r#"pii: ["email", "address1", "zip", "token"]"#,
    "drop non-pii: false",
    "error: datatype must be an instance or array of Datatypes::Datatype",
    "row: K1A",
];

/// What native Rails requires, in order (the first only natively: the
/// gem itself, where the emitted tree loads the port).
const LOADS: [&str; 7] = [
    "native/database_documentation_gem.rb",
    "app/models/application_record.rb",
    "app/models/customer.rb",
    "app/models/vip_customer.rb",
    "app/models/note.rb",
    "app/models/plain.rb",
    "app/models/doc_probe.rb",
];

/// Native only: Core's database_documentation 3.2.0 from the pinned gem
/// home, required as Bundler.require would (its railtie needs railties).
/// Its lib goes on the load path unactivated: activating the gem resolves
/// its Dataplex publishing dependencies, which the gem home lacks and
/// the model metadata never loads.
const NATIVE_GEM: &str = r#"gem "railties"
gem "zeitwerk"
$LOAD_PATH.unshift(File.join(Gem::Specification.find_by_name("database_documentation", "3.2.0").gem_dir, "lib"))
require "rails"
require "database_documentation"
"#;

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/application.rb", APPLICATION),
    ("Gemfile", GEMFILE),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/customer.rb", CUSTOMER),
    ("app/models/vip_customer.rb", VIP_CUSTOMER),
    ("app/models/note.rb", NOTE),
    ("app/models/plain.rb", PLAIN),
    ("app/models/doc_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The include resolves to the port, the macro calls replay in body
/// order, and the tree loads the port where Bundler.require loads the gem.
#[test]
fn database_documentation_replays_through_the_gem_port() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let customer = std::fs::read_to_string(emitted.join("app/models/customer.rb")).unwrap();
    let include = customer.find("include DatabaseDocumentation::ModelMetadata::Metadata").expect(&customer);
    let first = customer.find("database_documentation do |dd|").expect(&customer);
    let second = customer.rfind("database_documentation do |dd|").unwrap();
    assert!(include < first && first < second, "{customer}");
    let note = std::fs::read_to_string(emitted.join("app/models/note.rb")).unwrap();
    assert!(note.contains("database_documentation do |dd|"), "{note}");
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("require_relative \"database_documentation\""), "{facades}");
    assert!(emitted.join("runtime/database_documentation.rb").exists());
    let feature = std::fs::read_to_string(emitted.join("runtime/features/database_documentation.rb")).unwrap();
    assert!(feature.contains("require_relative \"../database_documentation\""), "{feature}");
}

/// The same models natively, on the gem, and emitted, on the port.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and gem home, and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn database_documentation_matches_the_gem_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let native_files: Vec<(&str, &str)> =
        FILES.iter().copied().chain([("native/database_documentation_gem.rb", NATIVE_GEM)]).collect();
    let (native, emitted) = pg_probe::native_and_emitted(
        "database-documentation",
        &native_files,
        &emitted,
        "rh_e2e_database_documentation",
        "DocProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}

/// An app that compiles the gem from source (roundhouse.yml
/// `gem_sources:`, as Core vendors database_documentation 3.2.0): the
/// gem's own files are the gem, so the tree carries no port for it.
/// Before, the port loaded with the gem facades and the compiled
/// `DatabaseDocumentation::Constants` then reassigned the
/// METADATA_PROJECT the port had defined: "already initialized
/// constant", which Core's StrictWarning turns into a boot failure.
const VENDORED_ENTRY: &str = "require \"database_documentation/constants\"\n\nmodule DatabaseDocumentation\n  VENDORED = true\nend\n";

const VENDORED_CONSTANTS: &str = "module DatabaseDocumentation\n  module Constants\n    MAX_APPLICATION_AND_SCHEMA_NAME_LENGTH = 57\n    METADATA_PROJECT = \"shopify-metadata\"\n    METADATA_LOCATION = \"us\"\n  end\nend\n";

/// Reads only what the vendored gem defines; the port would add its own
/// names (ModelMetadata, Datatypes, Configuration, ...) beside them.
const VENDORED_PROBE: &str = r##"class DocProbe
  def self.lines
    [
      "namespace: #{DatabaseDocumentation.constants.sort.inspect}",
      "constants: #{DatabaseDocumentation::Constants.constants.sort.inspect}",
      "project: #{DatabaseDocumentation::Constants::METADATA_PROJECT} #{DatabaseDocumentation::Constants::METADATA_LOCATION}",
    ]
  end
end
"##;

const VENDORED_EXPECTED: [&str; 3] = [
    "namespace: [:Constants, :VENDORED]",
    "constants: [:MAX_APPLICATION_AND_SCHEMA_NAME_LENGTH, :METADATA_LOCATION, :METADATA_PROJECT]",
    "project: shopify-metadata us",
];

/// Native only: Bundler.require of the vendored gem, from its directory.
const VENDORED_NATIVE_GEM: &str = "$LOAD_PATH.unshift(File.expand_path(\"../vendor/gems/database_documentation-3.2.0/lib\", __dir__))\nrequire \"database_documentation\"\n";

const VENDORED_FILES: [(&str, &str); 9] = [
    (
        "roundhouse.yml",
        "database_adapter: postgresql\ngem_sources:\n  database_documentation: vendor/gems/database_documentation-3.2.0\n",
    ),
    ("config/database.yml", DATABASE_YML),
    ("config/application.rb", APPLICATION),
    ("Gemfile", GEMFILE),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("vendor/gems/database_documentation-3.2.0/lib/database_documentation.rb", VENDORED_ENTRY),
    ("vendor/gems/database_documentation-3.2.0/lib/database_documentation/constants.rb", VENDORED_CONSTANTS),
    ("app/models/doc_probe.rb", VENDORED_PROBE),
];

fn vendored_overlay() -> emit_and_run::Overlay {
    VENDORED_FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
fn a_gem_compiled_from_source_carries_no_port() {
    let (emitted, errors) = vendored_overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let port = std::fs::read_to_string(emitted.join("runtime/database_documentation.rb"));
    assert!(port.is_err(), "the port is carried: {:.400}", port.unwrap_or_default());
    assert!(!emitted.join("runtime/features/database_documentation.rb").exists());
    let facades = std::fs::read_to_string(emitted.join("runtime/gem_facades.rb")).unwrap();
    assert!(!facades.contains("require_relative \"database_documentation\""), "{facades}");
    // The compiled gem answers `require "database_documentation"`.
    let models = std::fs::read_to_string(emitted.join("app/models.rb")).unwrap();
    assert!(
        models.contains("\"database_documentation\" => [\"models/roundhouse_files/vendor/gems/database_documentation-3.2.0/lib/database_documentation\"]"),
        "no compiled feature for the gem's entry"
    );
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and gem home, and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_gem_compiled_from_source_matches_the_gem_on_postgresql() {
    let (emitted, errors) = vendored_overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let native_files: Vec<(&str, &str)> =
        VENDORED_FILES.iter().copied().chain([("native/database_documentation_gem.rb", VENDORED_NATIVE_GEM)]).collect();
    let (native, emitted) = pg_probe::native_and_emitted(
        "database-documentation-vendored",
        &native_files,
        &emitted,
        "rh_e2e_database_documentation_vendored",
        "DocProbe",
        &["native/database_documentation_gem.rb", "app/models/doc_probe.rb"],
    );
    assert_eq!(native, VENDORED_EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}
