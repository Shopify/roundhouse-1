//! A model's compiled column accessors live in its own
//! GeneratedAttributeMethods module on the Ruby family, as Rails defines
//! them (activerecord 8.2.0.alpha core.rb `initialize_generated_modules`),
//! so a method the class defines over one supers into it: the model's
//! own `def name=(value) super(value.strip) end`, and a writer a gem's
//! class macro defines with `define_method` (shopify-money 4.1.1's
//! `clear_cache_on_currency_change`: `define_method("#{currency_column}=")
//! { |value| clear_money_column_cache; super(value) }`). Against native
//! Rails 8.2 on the same local PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    code character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_generated_attribute_methods
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    tracing (1.0.0)\n\nDEPENDENCIES\n  tracing\n";

const GEM_MAIN: &str = r#"require_relative "tracing/hooks"
require_relative "tracing/railtie"
"#;

const GEM_RAILTIE: &str = r#"require "rails/railtie"

module Tracing
  class Railtie < Rails::Railtie
    ActiveSupport.on_load(:active_record) do
      ActiveRecord::Base.send(:include, Tracing::Hooks)
    end
  end
end
"#;

/// MoneyColumn::ActiveRecordHooks' shape: a per-record cache set up in
/// `init_internals`, and a macro whose writer clears it and supers into
/// the column writer.
const GEM_HOOKS: &str = r##"module Tracing
  module Hooks
    def self.included(base)
      base.extend(ClassMethods)
    end

    def reload(*)
      @trace_cache = {}
      super
    end

    def trace
      @trace ||= []
    end

    def remember(column)
      @trace_cache[column] = public_send(column)
    end

    def cached(column)
      @trace_cache[column]
    end

    private

    def init_internals
      @trace_cache = {}
      super
    end

    module ClassMethods
      def clears_cache_on(column)
        define_method("#{column}=") do |value|
          @trace_cache = {}
          trace << "#{column}=#{value}"
          super(value)
        end
      end
    end
  end
end
"##;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SHOP: &str = r##"class Shop < ApplicationRecord
  clears_cache_on :code

  def name=(value)
    super(value.to_s.strip)
  end

  def code?
    super && code != "none"
  end
end
"##;

const PROBE: &str = r##"class GeneratedAttributesProbe
  def self.lines
    out = []
    # Construction and loading call no writer in Rails; the compiled
    # initialize and hydration factories still do (hook 2's probe).
    s = Shop.new
    s.trace.clear
    s.name = "  a  "
    s.code = "x"
    out << "writers: #{s.name.inspect} #{s.code.inspect} #{s.trace.inspect}"
    s.remember(:code)
    s.code = "y"
    out << "macro writer clears: #{s.cached(:code).inspect} #{s.code}"
    s.save!
    t = Shop.find(s.id)
    t.trace.clear
    out << "persisted: #{t.name.inspect} #{t.code.inspect} #{t.trace.inspect}"
    t.code = "none"
    out << "predicate: #{t.code?} #{Shop.new.code?}"
    out << "owners: #{Shop.instance_method(:name=).owner} #{Shop.instance_method(:name=).super_method.owner} #{Shop.instance_method(:code=).owner}"
    at = Shop.ancestors.index(Shop.instance_method(:name=).super_method.owner)
    out << "ancestor: #{at > 0 && at < Shop.ancestors.index(ApplicationRecord)}"
    out
  end
end
"##;

const NATIVE_BOOT: &str = r#"railties = $LOAD_PATH.grep(%r{/activerecord/lib\z}).first.sub(%r{/activerecord/lib\z}, "/railties/lib")
$LOAD_PATH.unshift(railties)
require "rails/railtie"
require_relative "vendor/gems/tracing-1.0.0/lib/tracing"
"#;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 6] = [
    r#"writers: "a" "x" ["code=x"]"#,
    r#"macro writer clears: nil y"#,
    r#"persisted: "a" "y" []"#,
    r#"predicate: false false"#,
    r#"owners: Shop Shop::GeneratedAttributeMethods Shop"#,
    r#"ancestor: true"#,
];

const FILES: [(&str, &str); 14] = [
    ("roundhouse.yml", "gem_sources:\n  tracing: vendor/gems/tracing-1.0.0\ndatabase_adapter: postgresql\n"),
    ("Gemfile", "source \"https://rubygems.org\"\n\ngem \"tracing\"\n"),
    ("Gemfile.lock", LOCK),
    ("config/application.rb", "Bundler.require(*Rails.groups)\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("vendor/gems/tracing-1.0.0/lib/tracing.rb", GEM_MAIN),
    ("vendor/gems/tracing-1.0.0/lib/tracing/railtie.rb", GEM_RAILTIE),
    ("vendor/gems/tracing-1.0.0/lib/tracing/hooks.rb", GEM_HOOKS),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/generated_attributes_probe.rb", PROBE),
    ("native_boot.rb", NATIVE_BOOT),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn overrides_of_column_accessors_super_into_them_as_natively_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "generated-attribute-methods",
        &FILES,
        &emitted,
        "rh_e2e_generated_attribute_methods",
        "GeneratedAttributesProbe",
        &["native_boot", "app/models/application_record", "app/models/shop", "app/models/generated_attributes_probe"],
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

/// The column accessors are written in the module, included first; the
/// model's own writer stays in the class.
#[test]
fn column_accessors_are_written_in_the_generated_module() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let shop = std::fs::read_to_string(emitted.join("app/models/shop.rb")).expect("shop.rb");
    let module = shop.find("  module GeneratedAttributeMethods\n").expect(&shop);
    let include = shop.find("  include GeneratedAttributeMethods\n").expect(&shop);
    let module_body = &shop[module..include];
    assert!(module_body.contains("    def code=("), "{shop}");
    assert!(module_body.contains("    def name"), "{shop}");
    assert!(!module_body.contains("strip"), "{shop}");
    assert!(shop[include..].contains("super(value.to_s.strip)"), "{shop}");
    assert!(shop[include..].contains("clears_cache_on"), "{shop}");
}
