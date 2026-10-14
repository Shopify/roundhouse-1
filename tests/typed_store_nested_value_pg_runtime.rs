//! A `typed_store` `any` field holding a nested Hash (Core's Shop
//! `s.any(:signup, default: {})`) keeps it through the PostgreSQL ruby
//! lane's runtime, as native Rails 8.2 with activerecord-typedstore on the
//! same local PostgreSQL database.
//!
//! The ruby tree's runtime/thread_state.rb reopens `TypedStore` with the
//! spinel flat-line parser, which loaded after runtime/typed_store.rb and
//! replaced its YAML `parse`/`write`: `signup:` over indented lines read
//! as nil (`settings[:signup].fetch(...)` raised `undefined method 'fetch'
//! for nil`), and a write dumped a Hash as its `to_s`. The overlay boot
//! now loads typed_store.rb after thread_state.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    settings text,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_typed_store_nested
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SHOP: &str = r#"class Shop < ApplicationRecord
  typed_store :settings do |s|
    s.string :timezone, default: "UTC"
    s.any :signup, default: {}
    s.boolean :open, default: false
  end

  def signup_types
    signup = settings[:signup] #: untyped
    signup.fetch("types", [])
  end
end
"#;

const PROBE: &str = r##"class NestedStoreProbe
  def self.lines
    out = []
    shop = Shop.create!(name: "a")
    out << "default: #{shop.settings[:signup].inspect} #{shop.signup_types.inspect}"
    shop.settings[:signup] = { "industry" => "restaurants", "types" => ["x", "y"] }
    shop.settings[:open] = true
    shop.save!
    found = Shop.find(shop.id)
    out << "loaded: #{found.settings[:signup].inspect} #{found.signup_types.inspect} #{found.settings[:open].inspect}"
    found.settings[:timezone] = "Paris"
    found.save!
    again = Shop.find(shop.id)
    out << "rewritten: #{again.settings[:signup].inspect} #{again.settings[:timezone]} #{again.settings[:open].inspect}"
    out << "dig: #{again.settings.dig(:signup, "industry").inspect}"
    out
  end
end
"##;

/// Native only: the typed_store gem, which the emitted tree's runtime
/// replaces.
const TYPED_STORE_GEM: &str = "gem \"activerecord-typedstore\"\nrequire \"active_record/typed_store\"\n";

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 4] = [
    r#"default: {} []"#,
    r#"loaded: {"industry" => "restaurants", "types" => ["x", "y"]} ["x", "y"] true"#,
    r#"rewritten: {"industry" => "restaurants", "types" => ["x", "y"]} Paris true"#,
    r#"dig: "restaurants""#,
];

const LOADS: [&str; 4] = [
    "native/typed_store_gem",
    "app/models/application_record",
    "app/models/shop",
    "app/models/nested_store_probe",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/nested_store_probe.rb", PROBE),
    ("native/typed_store_gem.rb", TYPED_STORE_GEM),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn a_nested_value_in_a_typed_store_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "typed-store-nested",
        &FILES,
        &emitted,
        "rh_e2e_typed_store_nested",
        "NestedStoreProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// The tree loads the YAML-backed typed_store after thread_state, so
/// its parse and write are the ones that run.
#[test]
fn the_yaml_typed_store_loads_after_thread_state() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let boot = std::fs::read_to_string(emitted.join("boot.rb")).expect("boot.rb");
    let thread_state = boot.find("require_relative \"runtime/thread_state\"").expect(&boot);
    let typed_store = boot.find("require_relative \"runtime/typed_store\"").expect(&boot);
    assert!(typed_store > thread_state, "typed_store before thread_state in boot.rb");
    assert_eq!(boot.matches("runtime/typed_store\"").count(), 1, "{boot}");
}
