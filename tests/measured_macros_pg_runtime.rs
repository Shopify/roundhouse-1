//! measured 3.2.1's model macros (`measured_weight :weight`,
//! `measured_length :length, :width, :height, unit_field_name: :dimension_unit`)
//! and its `measured:` validator, run from the public gem's own source
//! (`measured/rails/active_record`, `measured/rails/validations`), against
//! native Rails 8.2 on the same local PostgreSQL database.
//!
//! The macros reach every model by including modules into
//! ActiveRecord::Base from the gem's `on_load(:active_record)` hook. An app
//! concern's `included do ... end` calls them, over `<field>_value` and
//! `<field>_unit` columns. Before: the concern's block was dropped, so the
//! model had no `weight`/`length` reader or writer.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE parcels (
    id bigserial NOT NULL,
    weight_value numeric(10,4),
    weight_unit character varying,
    length_value numeric(10,4),
    width_value numeric(10,4),
    height_value numeric(10,4),
    dimension_unit character varying,
    CONSTRAINT parcels_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_measured_macros
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n    measured (3.2.1)\n\nDEPENDENCIES\n  i18n\n  measured (~> 3.2.1)\n";

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// An app concern whose `included` block calls the gem's macros.
const CONCERN: &str = r#"module Dimensioned
  extend ActiveSupport::Concern

  included do
    measured_weight :weight
    measured_length :length, :width, :height, unit_field_name: :dimension_unit

    validates :weight, measured: { less_than: 1000, units: ["g", "kg"] }
    validates :length, :width, :height, measured: { greater_than_or_equal_to: 0, units: ["in", "cm"] }, allow_nil: true
  end
end
"#;

const PARCEL: &str = r#"class Parcel < ApplicationRecord
  include Dimensioned
end
"#;

const PROBE: &str = r##"class MeasuredProbe
  def self.lines
    out = []
    p = Parcel.new(weight: Measured::Weight.new(2.5, :kg), length: Measured::Length.new(10, :cm), width: Measured::Length.new(2, "cm"), height: Measured::Length.new(3, :cm))
    out << "write: #{p.weight_value.to_s} #{p.weight_unit} #{p.length_value.to_s} #{p.dimension_unit}"
    out << "reader: #{p.weight.class} #{p.weight.value.to_s} #{p.weight.unit} #{p.height.inspect}"
    out << "valid: #{p.valid?} #{p.errors.full_messages.inspect}"
    p.save!
    q = Parcel.find(p.id)
    out << "found: #{q.weight.inspect} #{q.length.convert_to(:in).value.round(3).to_s}"
    q.weight = nil
    out << "nil: #{q.weight.inspect} #{q.weight_value.inspect} #{q.weight_unit.inspect}"
    r = Parcel.new(weight_value: 5, weight_unit: "kilogram")
    out << "unit: #{r.weight_unit} #{r.weight.inspect}"
    r.weight_unit = "bogus"
    out << "bogus: #{r.weight.inspect} #{r.valid?} #{r.errors.full_messages.inspect}"
    big = Parcel.new(weight: Measured::Weight.new(5000, :g), length: Measured::Length.new(-1, :cm), dimension_unit: "cm")
    out << "bounds: #{big.valid?} #{big.errors.full_messages.sort.inspect}"
    out << "fields: #{Parcel.measured_fields.keys.inspect} #{Parcel.measured_fields[:length][:unit_field_name]}"
    begin
      Parcel.new.weight = Measured::Weight.new(123456789, :g)
    rescue => e
      out << "overflow: #{e.class} #{e.message[0, 40]}"
    end
    begin
      Parcel.measured_weight(:weight)
    rescue => e
      out << "dup: #{e.class} #{e.message}"
    end
    out
  end
end
"##;

/// Native only: Bundler.require's load of the gem, with its railtie.
const NATIVE_BOOT: &str = r#"railties = $LOAD_PATH.grep(%r{/activerecord/lib\z}).first.sub(%r{/activerecord/lib\z}, "/railties/lib")
$LOAD_PATH.unshift(railties)
require "rails/railtie"
require_relative "vendor/gems/measured-3.2.1/lib/measured"
"#;

fn gem_home() -> std::path::PathBuf {
    let home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    std::path::Path::new(&home).join("gems/measured-3.2.1")
}

/// The public gem's lib files, vendored as written.
fn gem_files() -> Vec<(String, String)> {
    let root = gem_home();
    let mut out = Vec::new();
    let mut stack = vec![root.join("lib")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rb") && !path.to_string_lossy().contains("/tapioca/") {
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
                out.push((format!("vendor/gems/measured-3.2.1/{rel}"), std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn files() -> Vec<(String, String)> {
    let own: [(&str, &str); 12] = [
        ("roundhouse.yml", "gem_sources:\n  measured: vendor/gems/measured-3.2.1\ndatabase_adapter: postgresql\n"),
        ("Gemfile", "source \"https://rubygems.org\"\n\ngem \"measured\", \"~> 3.2.1\"\n"),
        ("Gemfile.lock", LOCK),
        ("config/application.rb", "Bundler.require(*Rails.groups)\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/concerns/dimensioned.rb", CONCERN),
        ("app/models/parcel.rb", PARCEL),
        ("app/models/measured_probe.rb", PROBE),
        ("native_boot.rb", NATIVE_BOOT),
    ];
    own.iter().map(|(p, t)| (p.to_string(), t.to_string())).chain(gem_files()).collect()
}

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors
        .iter()
        .filter(|e| !e.contains("database_adapter not supported"))
        // The gem's `...` delegation and `new(**kw)` run as written on the CRuby tree.
        .filter(|e| !e.contains("full argument forwarding not supported") && !e.contains("constructor keyword arguments not supported"))
        .collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn measured_macros_match_native_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let all = files();
    let refs: Vec<(&str, &str)> = all.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "measured-macros",
        &refs,
        &emitted,
        "rh_e2e_measured_macros",
        "MeasuredProbe",
        &["native_boot", "app/models/application_record", "app/models/concerns/dimensioned", "app/models/parcel", "app/models/measured_probe"],
    );
    // What native Rails answers (checked on the task PostgreSQL), so two
    // identical wrong answers still fail.
    assert_eq!(native.len(), 11, "native: {native:#?}");
    assert_eq!(native[0], "write: 2.5 kg 10.0 cm");
    assert!(native[1].starts_with("reader: Measured::Weight 2.5 kg"), "{}", native[1]);
    assert_eq!(native[6], "bogus: nil false [\"Weight is not a valid unit\"]");
    assert_eq!(native[7], "bounds: false [\"Length -1 cm must be >= 0 cm\", \"Weight 5000 g must be < 1000 g\"]");
    assert_eq!(native[10], "dup: Measured::Rails::Error The field weight has already been measured");
    assert_eq!(emitted_lines, native, "emitted");
}

/// The macro calls are replayed in the model body, and the gem's Concern
/// modules keep their `extend ActiveSupport::Concern` (what extends
/// ActiveRecord::Base with their `ClassMethods` when the gem includes them).
#[test]
fn the_gems_macros_are_replayed_and_its_concerns_carried() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let parcel = std::fs::read_to_string(emitted.join("app/models/parcel.rb")).expect("parcel.rb");
    assert!(parcel.contains("measured_weight :weight") || parcel.contains("measured_weight(:weight)"), "{parcel}");
    let weight = std::fs::read_to_string(emitted.join("app/models/measured/rails/active_record/weight.rb")).expect("weight.rb");
    assert!(weight.contains("extend ActiveSupport::Concern"), "{weight}");
    // The railtie's reopens root their superclass: a bare `Rails` in
    // `module Measured` is the `Measured::Rails` being reopened.
    let class_methods =
        std::fs::read_to_string(emitted.join("app/models/measured/rails/active_record/class_methods.rb")).expect("class_methods.rb");
    assert!(class_methods.contains("class Rails < ::Rails::Railtie"), "{class_methods}");
}
