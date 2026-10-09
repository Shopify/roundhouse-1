//! Three Core load blockers, each a class body the emitted program
//! loaded differently from native Rails 8.2 on sorbet-runtime:
//!
//! * `X[Y]` on a class that `extend`s `T::Generic` (Core's
//!   `QuantityRules::HANDLE = CheckoutPlatform::Instructions::InstructionHandle[QuantityRules].new(NAME)`).
//!   sorbet-runtime's `T::Generic#[]` answers the class itself; the
//!   emitter erases the `extend T::Generic`, so the application is
//!   erased with it (src/ingest/sorbet_check.rs, erase_generic_applications)
//!   and `X[Y]` is `X`. A class that defines its own class-side `[]`
//!   keeps the call.
//! * A namespaced validator key (`validates :locale_code,
//!   "store_localization/locale_code": true` in Core's Shop). Rails
//!   const_gets `"#{key.to_s.camelize}Validator"`, and `camelize` turns
//!   each `/` into `::`: the validator is
//!   `StoreLocalization::LocaleCodeValidator`, not the division
//!   `StoreLocalization/localeCodeValidator` (src/ingest/validates.rs).
//! * `class << self; include ::Mixin` (Core's
//!   `BusinessCustomers::CompanyMetafieldOwner` includes
//!   `::Metafields::Owners::AbstractOwner` so). The rooted path named no
//!   app module once joined with its root, and the emitter dropped the
//!   `extend` it lowers to (src/emit/ruby/library.rs, resolve_app_module).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);

CREATE TABLE shipping_lines (
    id bigserial NOT NULL,
    province character varying,
    CONSTRAINT shipping_lines_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_generic_validator_singleton
"#;

/// The ActiveModel::Validations port translates through I18n, which the
/// lockfile names.
const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// CheckoutPlatform::Instructions::InstructionHandle's shape: a generic
/// class with a bounded type member, applied before `.new`.
const HANDLE: &str = r#"# typed: strict
module Platform
  class Handle
    extend(T::Generic)

    ValueType = type_member(:out) { { upper: Object } }

    #: (Symbol name) -> void
    def initialize(name)
      @name = name
    end

    #: Symbol
    attr_reader :name
  end
end
"#;

const SUB_HANDLE: &str = r#"module Platform
  class SubHandle < Handle
  end
end
"#;

/// A generic module; `[]` answers the module.
const GENERIC_MODULE: &str = r#"module GenericMod
  extend T::Generic

  Elem = type_member

  def self.label
    "mod"
  end
end
"#;

/// A generic class that defines its own class-side `[]`: the call is
/// that method's, not sorbet-runtime's.
const OWN_INDEX: &str = r#"class Registry
  extend T::Generic

  Elem = type_member

  def self.[](key)
    "entry #{key}"
  end
end
"#;

/// QuantityRules' shape: the handle applied to the class being defined,
/// in a constant, and applications inside methods (a rooted path, a
/// subclass, a module).
const RULES: &str = r#"# typed: strict
module Checkouts
  class QuantityRules
    extend T::Generic

    NAME = :quantity_rules

    HANDLE = Platform::Handle[QuantityRules].new(NAME) #: Platform::Handle[QuantityRules]

    ValueType = type_member { { fixed: Integer } }

    class << self
      #: -> Array[untyped]
      def report
        [
          HANDLE.name,
          HANDLE.class.name,
          Platform::Handle[Integer].equal?(Platform::Handle),
          ::Platform::Handle[Symbol].new(:rooted).name,
          Platform::SubHandle[String].new(:sub).class.name,
          GenericMod[Integer].label,
          Registry[:a],
        ]
      end
    end
  end
end
"#;

/// Shop's shape: a top-level model, a namespaced validator key.
const THING_VALIDATOR: &str = r#"module Ns
  class ThingValidator < ActiveModel::EachValidator
    def validate_each(record, attribute, value)
      record.errors.add(attribute, "is not a thing") unless value.to_s.start_with?("thing")
    end
  end
end
"#;

const WIDGET: &str = r#"class Widget < ApplicationRecord
  validates :name, "ns/thing": true
end
"#;

/// Shipping::CustomsFormLine's shape: a namespaced model and a key under
/// the same namespace.
const LINE_PROVINCE_VALIDATOR: &str = r#"module Shipping
  class LineProvinceValidator < ActiveModel::EachValidator
    def validate_each(record, attribute, value)
      record.errors.add(attribute, "is not a province") unless %w[ON QC].include?(value)
    end
  end
end
"#;

const LINE: &str = r#"module Shipping
  class Line < ApplicationRecord
    self.table_name = "shipping_lines"

    validates :province, "shipping/line_province": true, allow_nil: true
  end
end
"#;

const ABSTRACT_OWNER: &str = r##"module Metafields
  module Owners
    module AbstractOwner
      def owner_type
        "#{resource_name}_type"
      end

      def user_write_permission
        "write_#{resource_name}"
      end
    end
  end
end
"##;

/// CompanyMetafieldOwner's shape: a namespaced class whose singleton
/// class includes the module by its rooted path.
const COMPANY_OWNER: &str = r#"# typed: strict
module Biz
  class CompanyOwner
    class << self
      include ::Metafields::Owners::AbstractOwner

      #: -> String
      def resource_name
        "company"
      end
    end
  end
end
"#;

/// The same, as a module and written unrooted.
const MODULE_OWNER: &str = r#"module Biz
  module ModuleOwner
    class << self
      include Metafields::Owners::AbstractOwner

      def resource_name
        "module"
      end
    end
  end
end
"#;

const PROBE: &str = r##"class GvsProbe
  def self.describe(label, record)
    "#{label}: #{record.valid?} #{record.errors.full_messages.inspect}"
  end

  def self.lines
    out = []
    out << "rules: #{Checkouts::QuantityRules.report.inspect}"
    out << describe("thing", Widget.new(name: "thing one"))
    out << describe("not thing", Widget.new(name: "other"))
    widget = Widget.create(name: "thing two")
    out << "widget saved: #{widget.persisted?} #{Widget.count}"
    out << "widget rejected: #{Widget.create(name: "nope").persisted?} #{Widget.count}"
    out << describe("province", Shipping::Line.new(province: "ON"))
    out << describe("bad province", Shipping::Line.new(province: "XX"))
    out << describe("no province", Shipping::Line.new)
    out << "company: #{Biz::CompanyOwner.owner_type} #{Biz::CompanyOwner.user_write_permission}"
    out << "module: #{Biz::ModuleOwner.owner_type} #{Biz::ModuleOwner.user_write_permission}"
    out << "company is owner: #{Biz::CompanyOwner.singleton_class.include?(Metafields::Owners::AbstractOwner)}"
    out
  end
end
"##;

/// What native Rails 8.2 on sorbet-runtime 0.6.13502 answers (checked on
/// the task PostgreSQL), so two identical wrong answers still fail.
const EXPECTED: [&str; 11] = [
    r#"rules: [:quantity_rules, "Platform::Handle", true, :rooted, "Platform::SubHandle", "mod", "entry a"]"#,
    r#"thing: true []"#,
    r#"not thing: false ["Name is not a thing"]"#,
    r#"widget saved: true 1"#,
    r#"widget rejected: false 1"#,
    r#"province: true []"#,
    r#"bad province: false ["Province is not a province"]"#,
    r#"no province: true []"#,
    r#"company: company_type write_company"#,
    r#"module: module_type write_module"#,
    r#"company is owner: true"#,
];

const FILES: [(&str, &str); 18] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("Gemfile.lock", LOCK),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/platform/handle.rb", HANDLE),
    ("app/models/platform/sub_handle.rb", SUB_HANDLE),
    ("app/models/generic_mod.rb", GENERIC_MODULE),
    ("app/models/registry.rb", OWN_INDEX),
    ("app/models/checkouts/quantity_rules.rb", RULES),
    ("app/models/ns/thing_validator.rb", THING_VALIDATOR),
    ("app/models/widget.rb", WIDGET),
    ("app/models/shipping/line_province_validator.rb", LINE_PROVINCE_VALIDATOR),
    ("app/models/shipping/line.rb", LINE),
    ("app/models/metafields/owners/abstract_owner.rb", ABSTRACT_OWNER),
    ("app/models/biz/company_owner.rb", COMPANY_OWNER),
    ("app/models/biz/module_owner.rb", MODULE_OWNER),
];

const PROBE_FILE: (&str, &str) = ("app/models/gvs_probe.rb", PROBE);

/// Native only: Core's sorbet-runtime (its gem home is the oracle's),
/// which defines `T::Generic` as the app's Bundler.require would.
const NATIVE_SORBET: (&str, &str) = ("native/sorbet.rb", "require \"sorbet-runtime\"\n");

/// What native Rails requires, in order (the first only natively).
const LOADS: [&str; 15] = [
    "native/sorbet.rb",
    "app/models/application_record.rb",
    "app/models/platform/handle.rb",
    "app/models/platform/sub_handle.rb",
    "app/models/generic_mod.rb",
    "app/models/registry.rb",
    "app/models/checkouts/quantity_rules.rb",
    "app/models/ns/thing_validator.rb",
    "app/models/widget.rb",
    "app/models/shipping/line_province_validator.rb",
    "app/models/shipping/line.rb",
    "app/models/metafields/owners/abstract_owner.rb",
    "app/models/biz/company_owner.rb",
    "app/models/biz/module_owner.rb",
    "app/models/gvs_probe.rb",
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().chain([&PROBE_FILE]).fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The emitted class bodies: no generic application left, the
/// validator as a constant path, the singleton include as an `extend`.
#[test]
fn generic_validator_and_singleton_include_emit_as_ruby_loads_them() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |path: &str| std::fs::read_to_string(emitted.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));

    let rules = read("app/models/checkouts/quantity_rules.rb");
    assert!(!rules.contains("Handle["), "the generic application is erased:\n{rules}");
    assert!(!rules.contains("SubHandle["), "{rules}");
    assert!(!rules.contains("GenericMod["), "{rules}");
    assert!(rules.contains("HANDLE = Platform::Handle.new("), "{rules}");
    assert!(rules.contains("Registry[:a]"), "a class's own `[]` is kept:\n{rules}");

    let widget = read("app/models/widget.rb");
    assert!(widget.contains("Ns::ThingValidator"), "{widget}");
    assert!(!widget.contains("Ns/"), "{widget}");
    let line = read("app/models/shipping/line.rb");
    assert!(line.contains("Shipping::LineProvinceValidator"), "{line}");
    assert!(!line.contains("Shipping/"), "{line}");

    let owner = read("app/models/biz/company_owner.rb");
    assert!(owner.contains("extend ::Metafields::Owners::AbstractOwner"), "{owner}");
    let module_owner = read("app/models/biz/module_owner.rb");
    assert!(module_owner.contains("extend Metafields::Owners::AbstractOwner"), "{module_owner}");
}

/// The same app natively, on Rails 8.2 and sorbet-runtime, and emitted.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and gem home, and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn generic_validator_and_singleton_include_match_native_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let native_files: Vec<(&str, &str)> = FILES.iter().copied().chain([PROBE_FILE, NATIVE_SORBET]).collect();
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "generic-validator-singleton",
        &native_files,
        &emitted,
        "rh_e2e_generic_validator_singleton",
        "GvsProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_lines, native, "emitted");
}
