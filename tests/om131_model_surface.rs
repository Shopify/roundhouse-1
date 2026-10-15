//! om131: model-surface gaps found by the unit-1000-v2 sample.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_om131\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n  include HasManyWithQueryConstraints\nend\n";

/// Core's components/platform/essentials/app/models/concerns/has_many_with_query_constraints.rb
/// (Sorbet lines and the patch require dropped).
const HAS_MANY_WITH_QUERY_CONSTRAINTS: &str = r##"module HasManyWithQueryConstraints
  extend ActiveSupport::Concern

  module ClassMethods
    def has_many_with_query_constraints(name, scope = nil, **options, &block)
      query_constraints = options.delete(:query_constraints)
      foreign_key = options.delete(:foreign_key)
      primary_key = options.delete(:primary_key)

      raise ArgumentError, "query_constraints is required" unless query_constraints
      raise ArgumentError, "foreign_key is required" unless foreign_key
      raise ArgumentError, "primary_key is required" unless primary_key

      query_constraints = Array(query_constraints)
      foreign_key = Array(foreign_key)
      primary_key = Array(primary_key)

      unless (foreign_key - query_constraints).empty?
        raise ArgumentError, "foreign_key #{foreign_key.inspect} must be present in query_constraints"
      end

      has_many(
        name,
        scope,
        foreign_key: query_constraints,
        primary_key: primary_key,
        **options,
        &block
      )

      nullify_columns = foreign_key.map(&:to_s)
      reflection = reflect_on_association(name)
      reflection.writable_foreign_key = foreign_key
      base_class = reflection.association_class

      patched = Class.new(base_class) do
        define_method(:nullified_owner_attributes) do
          attrs = nullify_columns.each_with_object({}) { |col, h| h[col] = nil }
          attrs[reflection.type] = nil if reflection.type.present?
          attrs
        end
      end

      reflection.define_singleton_method(:association_class) { patched }
    end
  end
end
"##;

const STRUCTURE: &str = r#"CREATE TABLE codes (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT codes_pkey PRIMARY KEY (id)
);

CREATE TABLE applications (
    id bigserial NOT NULL,
    code_id bigint,
    CONSTRAINT applications_pkey PRIMARY KEY (id)
);

CREATE TABLE fulfillment_orders (
    id bigserial NOT NULL,
    shop_id bigint,
    CONSTRAINT fulfillment_orders_pkey PRIMARY KEY (id)
);

CREATE TABLE fulfillment_order_line_items (
    id bigserial NOT NULL,
    shop_id bigint,
    fulfillment_order_id bigint,
    CONSTRAINT fulfillment_order_line_items_pkey PRIMARY KEY (id)
);
"#;

fn overlay(models: &[(&str, &str)]) -> emit_and_run::Overlay {
    let base = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("Gemfile.lock", LOCK)
        .write("config/database.yml", DATABASE_YML)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("app/models/has_many_with_query_constraints.rb", HAS_MANY_WITH_QUERY_CONSTRAINTS);
    models.iter().fold(base, |app, (path, text)| app.write(path, text))
}

fn emitted_file(models: &[(&str, &str)], path: &str) -> String {
    let (emitted, _errors) = overlay(models).emit(roundhouse::project::BuildTarget::Ruby);
    std::fs::read_to_string(emitted.join("out").join(path))
        .or_else(|_| std::fs::read_to_string(emitted.join(path)))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `unsafe_self.alias_method(:code, :price_rule_code)` below a
/// `belongs_to`: Rails defines the association reader in a generated
/// module before the body continues, so the alias finds it.
#[test]
fn alias_of_an_association_reader_follows_the_reader() {
    let src = emitted_file(
        &[
            ("app/models/code.rb", "class Code < ApplicationRecord\nend\n"),
            ("app/models/base_application.rb", "class BaseApplication < ApplicationRecord\n  self.table_name = \"applications\"\nend\n"),
            ("app/models/scoped.rb", "module Scoped\n  extend ActiveSupport::Concern\nend\n"),
            (
                "app/models/sales/application.rb",
                "module Sales\n  class Application < BaseApplication\n    include Scoped\n\n    belongs_to :price_rule_code, -> { all }, class_name: \"::Code\", foreign_key: :code_id\n\n    unsafe_self = self #: as untyped\n    unsafe_self.alias_method(:code, :price_rule_code)\n    unsafe_self.alias_method(:code=, :price_rule_code=)\n\n    class << self\n      def sti_name\n        :code\n      end\n    end\n  end\nend\n",
            ),
        ],
        "app/models/sales/application.rb",
    );
    let reader = src.find("def price_rule_code\n").unwrap_or_else(|| panic!("{src}"));
    let alias = src.find("alias_method(:code, :price_rule_code)").unwrap_or_else(|| panic!("{src}"));
    assert!(reader < alias, "the alias follows the reader:\n{src}");
    let writer = src.find("def price_rule_code=").unwrap_or_else(|| panic!("{src}"));
    let alias = src.find("alias_method(:code=, :price_rule_code=)").unwrap_or_else(|| panic!("{src}"));
    assert!(writer < alias, "the alias follows the writer:\n{src}");
}

/// `repositories.extensions.create(...)` in a service whose bare
/// `repositories` is its own reader (a SmartProperties property holding
/// a module): one model declares `has_many :extensions`, but the by-name
/// guess that `repositories` is that model's record wrote
/// `commitment_id: repositories.id`, NoMethodError for the module.
#[test]
fn own_reader_owner_is_not_guessed_to_be_the_record_declaring_the_association() {
    let src = emitted_file(
        &[
            ("app/models/code.rb", "class Code < ApplicationRecord\n  has_many :things, foreign_key: :code_id\nend\n"),
            ("app/models/thing.rb", "class Thing < ApplicationRecord\n  belongs_to :code\nend\n"),
            (
                "app/models/repos.rb",
                "module Repos\n  class << self\n    def things\n      Thing\n    end\n  end\nend\n",
            ),
            (
                "app/models/creator.rb",
                "class Creator\n  def repositories\n    Repos\n  end\n\n  def perform\n    repositories.things.create(name: \"x\")\n  end\nend\n",
            ),
        ],
        "app/models/creator.rb",
    );
    assert!(src.contains("repositories.things.create"), "the call keeps its source shape:\n{src}");
    assert!(!src.contains("repositories.id"), "no `repositories.id` is read:\n{src}");
}

/// A `class_name: "FulfillmentOrderLineItem"` written in
/// `Fulfillments::FulfillmentOrder` names `Fulfillments::FulfillmentOrderLineItem`
/// as Rails' `compute_type` reads it, for a has_many declared through an
/// app macro too (emitted as `::FulfillmentOrderLineItem`: NameError).
#[test]
fn macro_declared_has_many_class_name_resolves_in_the_owners_namespace() {
    let src = emitted_file(
        &[
            (
                "app/models/fulfillments/fulfillment_order.rb",
                "module Fulfillments\n  class FulfillmentOrder < ApplicationRecord\n    has_many_with_query_constraints :line_items,\n      -> { order(:id) },\n      class_name: \"FulfillmentOrderLineItem\",\n      query_constraints: [:shop_id, :fulfillment_order_id],\n      primary_key: [:shop_id, :id],\n      foreign_key: :fulfillment_order_id\n  end\nend\n",
            ),
            (
                "app/models/fulfillments/fulfillment_order_line_item.rb",
                "module Fulfillments\n  class FulfillmentOrderLineItem < ApplicationRecord\n  end\nend\n",
            ),
        ],
        "app/models/fulfillments/fulfillment_order.rb",
    );
    assert!(src.contains(" Fulfillments::FulfillmentOrderLineItem.where"), "the reader queries the namespaced model:\n{src}");
    assert!(!src.contains(" ::FulfillmentOrderLineItem"), "no top-level constant is named:\n{src}");
}

#[test]
#[ignore]
fn scratch_real_condition() {
    let real = std::fs::read_to_string(std::env::var("OM131_FILE").unwrap()).unwrap();
    let src = emitted_file(&[("app/models/markets/platform/condition.rb", &real)], "app/models/markets/platform/condition.rb");
    std::fs::write("/tmp/claude-1000/om131/scratch.out.rb", &src).unwrap();
    assert!(src.contains("force_condition_uniqueness_check"), "missing");
}

/// `attribute :flag, :boolean, default: false` on a name with no column
/// is a virtual attribute: assignable at construction, reading its
/// default until assigned (Core's `Markets::Platform::Condition`,
/// `force_condition_uniqueness_check`: `unknown attribute` on `new`).
#[test]
fn virtual_attribute_with_a_literal_default_gets_a_reader_and_writer() {
    let src = emitted_file(
        &[(
            "app/models/code.rb",
            "class Code < ApplicationRecord\n  attribute :force_flag, :boolean, default: false\n  attribute :label, :string, default: \"none\"\n  attribute :name, :string, default: \"column wins\"\nend\n",
        )],
        "app/models/code.rb",
    );
    for needed in ["def force_flag\n", "def force_flag=", "def label\n", "def label="] {
        assert!(src.contains(needed), "{needed:?} missing:\n{src}");
    }
    assert!(src.contains("defined?(@force_flag)"), "the reader answers the default until assigned:\n{src}");
}

/// The untyped `attribute :movement_ids, default: -> { [] }` (Core's
/// `Inventory::AdjustmentGroup#emit_commit_log` reads `movement_ids`:
/// `undefined local variable or method`): a virtual attribute whose
/// collection default is stored on first read.
#[test]
fn untyped_virtual_attribute_with_a_lambda_default_gets_a_reader() {
    let src = emitted_file(
        &[(
            "app/models/code.rb",
            "class Code < ApplicationRecord\n  attribute :movement_ids, default: -> { [] }\nend\n",
        )],
        "app/models/code.rb",
    );
    assert!(src.contains("def movement_ids\n"), "reader missing:\n{src}");
    assert!(src.contains("def movement_ids="), "writer missing:\n{src}");
    assert!(src.contains("@movement_ids = []"), "the default is stored on first read:\n{src}");
}
