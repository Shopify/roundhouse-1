//! Core's SerializationOptions applied to every record: its initializer
//! `ActiveSupport.on_load(:active_record) { include SerializationOptions }`
//! runs against ActiveRecord::Base after Base and its ActiveModel
//! ancestry load and before the app's models (Rails' base.rb:336
//! `run_load_hooks`), so its `serializable_hash` (an indifferent-access
//! result, Hash `methods:` entries), `serialization_options` (`fields:`,
//! the inheritance column excepted) and `as_json` (a `format:`
//! StringInquirer) override the record's, against native Rails 8.2 on
//! the same local PostgreSQL database.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    plan character varying,
    created_at timestamp(6) without time zone,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);
CREATE TABLE products (
    id bigserial NOT NULL,
    shop_id bigint,
    title character varying,
    price numeric(10,2),
    CONSTRAINT products_pkey PRIMARY KEY (id)
);
CREATE TABLE vehicles (
    id bigserial NOT NULL,
    type character varying,
    wheels integer,
    CONSTRAINT vehicles_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_serialization_options
"#;

/// Core's config/initializers/010_serialization_options.rb, verbatim.
const INITIALIZER: &str = r##"# typed: strict
# frozen_string_literal: true

require "serialization_options/serialization_options"
require "serialization_options/ext/multibyte_chars_ext"
require "serialization_options/ext/time_zone_ext"

ActiveSupport.on_load(:active_record) { include SerializationOptions }
"##;

/// Core's lib/serialization_options/serialization_options.rb without its
/// `Options = T.type_alias { ... }` line (native here has no
/// sorbet-runtime; the emit erases the alias).
const SERIALIZATION_OPTIONS: &str = r##"# typed: false
# frozen_string_literal: true

module SerializationOptions

  def serializable_hash(options = nil)
    options ||= {}

    attribute_names = attributes.keys

    if (only = options[:only])
      attribute_names &= Array.wrap(only).map(&:to_s)
    elsif (except = options[:except])
      attribute_names -= Array.wrap(except).map(&:to_s)
    end

    hash = HashWithIndifferentAccess.new

    method_names = Array.wrap(options[:methods])
    (attribute_names + method_names).each do |n|
      if n.respond_to?(:keys)
        n.each_key do |key|
          hash[key] = read_attribute_for_serialization(n[key])
        end
      else
        hash[n] = read_attribute_for_serialization(n)
      end
    end

    serializable_add_includes(options) do |association, records, opts|
      filtered_records = if association == :variants && opts[:variants_response_limit_for_rest]
        records.first(opts[:variants_response_limit_for_rest])
      else
        records
      end

      hash[association] = if filtered_records.respond_to?(:map)
        filtered_records.map { |a| a.serializable_hash(a.serialization_options(opts)) }
      elsif filtered_records.nil?
        nil
      else
        filtered_records.serializable_hash(filtered_records.serialization_options(opts))
      end
    end

    hash
  end

  def read_attribute_for_serialization(*args)
    value = super
    value.is_a?(ActiveSupport::StringInquirer) ? value.to_s : value
  end

  def serializable_add_includes(parent_options = {})
    super(parent_options) do |association, records, include_options|
      include_options.merge!(parent_options.slice(
        :api_features,
        :api_client_id,
        :format,
        :metafield_namespaces,
        :metafield_identifiers,
        :variants_response_limit_for_rest
      ))
      yield association, records, include_options
    end
  end

  def serialization_options(options)
    wants_default_attrs = options[:only].nil?

    if (fields = options[:fields]).present?
      fields = fields.map(&:to_s)
      [:include, :fetched_include].each do |options_key|
        next unless options[options_key].present?

        # :include is usually an array, but can sometimes be a hash
        options[options_key] = options[options_key].each_with_object({}) do |(k, v), hash|
          hash[k] = v || {} if fields.include?(k.to_s)
        end
      end

      options[:only] = Array(options[:only]).map(&:to_s) & fields
      options[:methods] = Array(options[:methods]).map do |key|
        if key.respond_to?(:keys)
          key = key.stringify_keys.slice(*fields)
          key = nil if key.blank?
        elsif !fields.include?(key.to_s)
          key = nil
        end
        key
      end.compact
    end

    options[:only] = Array.wrap(options[:only]).map(&:to_s)

    if !wants_default_attrs && options[:only].empty?
      options[:except] ||= []
      options[:except].concat(attribute_names)
    end

    options[:except] = Array.wrap(options[:except]).map(&:to_s)
    options[:except] |= Array.wrap(self.class.inheritance_column) if self.class.respond_to?(:inheritance_column)

    options
  end

  def to_xml(options = nil)
    options = mixin_format(options, "xml").except(:include, :fetched_include)
    options[:indent] ||= 0
    super(serialization_options(options))
  end

  def as_json(options = nil)
    super(serialization_options(mixin_format(options, "json")))
  end

  def cached_serialization_fields
    []
  end

  private

  def mixin_format(options, format)
    (options || {}).merge(format: ActiveSupport::StringInquirer.new(format))
  end
end
"##;

/// Core's lib/serialization_options/ext/multibyte_chars_ext.rb, verbatim.
const MULTIBYTE_CHARS_EXT: &str = r##"# typed: strict
# frozen_string_literal: true

module ActiveSupport
  module Multibyte
    class Chars
      delegate :as_json, to: :wrapped_string
      delegate :encode_json, to: :wrapped_string
    end
  end
end
"##;

/// Core's lib/serialization_options/ext/time_zone_ext.rb, verbatim.
const TIME_ZONE_EXT: &str = r##"# typed: true
# frozen_string_literal: true

module ActiveSupport
  class TimeZone
    def as_json(_options = nil)
      to_s
    end

    def to_str
      to_s
    end
  end
end
"##;

const APPLICATION_RECORD: &str = r##"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"##;

/// A record overriding `serialization_options` as Shop::Extension does
/// (`only ||=`, a Hash `methods:` entry, `super`), and its
/// `settings_json` in the shape of extension.rb:2571's
/// `as_json(only: [:id], methods: setting_methods).tap { ... }`.
const SHOP: &str = r##"class Shop < ApplicationRecord
  has_many :products

  def label
    "shop #{name}"
  end

  def paid?
    plan == "paid"
  end

  def contact_email
    "#{name}@example.com"
  end

  def plan_inquiry
    ActiveSupport::StringInquirer.new(plan.to_s)
  end

  def serialization_options(options = {})
    options[:only] ||= [:id, :name, :created_at]
    options[:methods] ||= [:label, { customer_email: :contact_email }]
    super(options)
  end

  def settings_json
    setting_methods = [
      :label,
      { pre_launch_enabled: :paid? },
    ]
    setting_methods << :plan_inquiry
    json_options = {
      only: [:id],
      methods: setting_methods,
    }
    as_json(json_options).tap do |json|
      json["extra"] = { "x" => 1 }
    end
  end
end
"##;

const PRODUCT: &str = r##"class Product < ApplicationRecord
  belongs_to :shop
end
"##;

const VEHICLE: &str = r##"class Vehicle < ApplicationRecord
end
"##;

const CAR: &str = r##"class Car < Vehicle
end
"##;

const PROBE: &str = r##"class SerializationOptionsProbe
  def self.lines
    out = []
    s = Shop.create!(name: "a", plan: "paid")
    s.products.create!(title: "t", price: 1.5)
    out << "default: #{s.as_json.except("created_at").inspect}"
    out << "settings: #{s.settings_json.inspect}"
    out << "fields: #{s.as_json(fields: ["name", "customer_email"]).inspect}"
    out << "only empty: #{s.as_json(only: [], methods: [:label]).inspect}"
    h = s.serializable_hash(only: [:name], methods: [{ mail: :contact_email }])
    out << "sh: #{h.class} #{h[:name].inspect} #{h["mail"].inspect} #{h.inspect}"
    out << "include: #{s.as_json(only: [:name], methods: [], include: { products: { only: [:title] } }).inspect}"
    out << "include fields: #{s.as_json(only: [:name], methods: [], include: [:products], fields: ["name"]).inspect}"
    p = Product.first
    out << "product: #{p.as_json(only: [:title], methods: [{ shop_name: :shop_id }]).inspect}"
    out << "product except: #{p.as_json(except: [:id, :shop_id]).inspect}"
    out << "options: #{s.serialization_options({ only: :name }).inspect}"
    c = Car.create!(wheels: 4)
    out << "sti: #{c.as_json(only: [:type, :wheels]).inspect} #{c.as_json(methods: [{ kind: :type }]).except("id").inspect}"
    out
  end
end
"##;

const EXPECTED: [&str; 11] = [
    r##"default: {"id" => 1, "name" => "a", "label" => "shop a", "customer_email" => "a@example.com"}"##,
    r##"settings: {"id" => 1, "label" => "shop a", "pre_launch_enabled" => true, "plan_inquiry" => "paid", "extra" => {"x" => 1}}"##,
    r##"fields: {"name" => "a", "customer_email" => "a@example.com"}"##,
    r##"only empty: {"label" => "shop a"}"##,
    r##"sh: ActiveSupport::HashWithIndifferentAccess "a" "a@example.com" {"name" => "a", "mail" => "a@example.com"}"##,
    r##"include: {"name" => "a", "products" => [{"title" => "t"}]}"##,
    r##"include fields: {"name" => "a"}"##,
    r##"product: {"title" => "t", "shop_name" => 1}"##,
    r##"product except: {}"##,
    r##"options: {only: ["name"], methods: [:label, {customer_email: :contact_email}], except: ["type"]}"##,
    r##"sti: {"type" => "Car", "wheels" => 4} {"kind" => "Car"}"##,
];

/// What native Rails requires, in order: the initializer first, as
/// Rails runs initializers before eager loading the models.
const LOADS: [&str; 7] = [
    "config/initializers/010_serialization_options",
    "app/models/application_record",
    "app/models/shop",
    "app/models/product",
    "app/models/vehicle",
    "app/models/car",
    "app/models/serialization_options_probe",
];

const FILES: [(&str, &str); 10] = [
    ("config/initializers/010_serialization_options.rb", INITIALIZER),
    ("lib/serialization_options/serialization_options.rb", SERIALIZATION_OPTIONS),
    ("lib/serialization_options/ext/multibyte_chars_ext.rb", MULTIBYTE_CHARS_EXT),
    ("lib/serialization_options/ext/time_zone_ext.rb", TIME_ZONE_EXT),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/product.rb", PRODUCT),
    ("app/models/vehicle.rb", VEHICLE),
    ("app/models/car.rb", CAR),
    ("app/models/serialization_options_probe.rb", PROBE),
];

fn overlay(files: &[(&str, &str)]) -> emit_and_run::Overlay {
    files.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn serialization_options_matches_native_rails_on_postgresql() {
    let mut files = vec![
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", STRUCTURE),
    ];
    files.extend(FILES);
    let (emitted, errors) = overlay(&files).emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "serialization-options",
        &files,
        &emitted,
        "rh_e2e_serialization_options",
        "SerializationOptionsProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    let real: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(real.is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}
