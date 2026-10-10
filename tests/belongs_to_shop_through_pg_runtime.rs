//! Core's `belongs_to_shop_through` (BelongsToShopThroughAssociation,
//! 120 calls) and `readonly_belongs_to_shop_through`: the macro runs
//! `accelerated_belongs_to(:shop, **kwargs.extract!(:primary_key,
//! :foreign_key, :inverse_of))` whatever association the call names,
//! prepends a `shop` reader that first fills `shop_id` from the named
//! associations (Denormalize#retrieve_denormalized_foreign_key), and
//! runs Denormalize's `define_denormalized_foreign_key`: a `shop_id`
//! presence validation, a before_validation that fills it, and a
//! `shop_id` reader falling back to the same lookup. Against native
//! Rails 8.2 on the same local PostgreSQL database. The modules are
//! Core's, without their Sorbet `requires_ancestor`/`mixes_in_class_methods` lines.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shops (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT shops_pkey PRIMARY KEY (id)
);

CREATE TABLE orders (
    id bigserial NOT NULL,
    shop_id bigint,
    CONSTRAINT orders_pkey PRIMARY KEY (id)
);

CREATE TABLE line_items (
    id bigserial NOT NULL,
    order_id bigint,
    shop_id bigint,
    title character varying,
    CONSTRAINT line_items_pkey PRIMARY KEY (id)
);

CREATE TABLE client_details (
    id bigserial NOT NULL,
    order_id bigint,
    shop_id bigint,
    CONSTRAINT client_details_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_belongs_to_shop_through
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  include Component::ReadOnlyAssociations
  extend AcceleratedBelongsTo
  include Denormalize
  include BelongsToShopThroughAssociation
end
"#;

/// Core's gems/component/lib/component/read_only_associations.rb.
const READ_ONLY_ASSOCIATIONS: &str = r###"
module Component
  # Provides read-only versions of Active Record associations to use across components as an initial
  # step towards removing these associations entirely.
  #
  # To use, simply prefix the Active Record association macros `has_many`, `has_one` and `belongs_to`
  # with `readonly_` (e.g. `readonly_has_many`).
  #
  # The read only version of the associations do the following:
  # * All mutating methods on an association will now raise a `ReadOnlyError` when called.
  #   Specifically, these methods are
  #   * `build`
  #   * `create`
  #   * `create!`
  #   * `<<`
  #   * `push`
  #   * `concat`
  #   * `delete`
  #   * `delete_all`
  #   * `destroy`
  #   * `destroy_all`
  #   * `build_#{association}`
  #   * `create_#{association}`,
  #   * `create_#{association}!`
  #   * `#{association}=`
  #   * `#{association.to_s.singularize}_ids=`
  # * Any records returned by the association will be marked as read only. That is, calling `readonly?`
  #   on the record will return `true`.
  #
  # The point of using read only associations is to prevent the creation or mutation of records from
  # outside the component when these should instead be done through a component entrypoint. To find
  # existing areas where associations are mutated, you can change an association to read only and
  # then use CI to find the areas you need to migrate to an entrypoint. Once you've fully migrated,
  # the read only association now serves to guard against future regressions.
  #
  module ReadOnlyAssociations
    extend ActiveSupport::Concern

    ReadOnlyError = Class.new(StandardError)

    module ReadOnlyAssociationExtension
      private

      def raise_on_mutating_methods!(...)
        raise ReadOnlyError, "Mutating methods are disabled for read-only associations"
      end

      [:build, :create, :create!, :<<, :push, :concat, :delete, :delete_all, :destroy, :destroy_all].each do |method|
        alias_method(method, :raise_on_mutating_methods!)
        public method
      end
    end

    module ClassMethods
      def readonly_has_many(name, scope = nil, **options, &extension)
        readonly_scope = build_readonly_scope(scope)
        extend_option = [*options[:extend], ReadOnlyAssociationExtension]
        has_many(name, readonly_scope, **options.merge(extend: extend_option, autosave: false), &extension)

        mutating_methods = mutating_collection_methods(name)
        raise_on_mutating_methods!(mutating_methods)
      end

      def readonly_has_one(name, scope = nil, **options)
        has_one(name, scope, **options.merge(autosave: false))

        define_readonly_decorator(name)

        mutating_methods = mutating_singular_methods(name)
        raise_on_mutating_methods!(mutating_methods)
      end

      def readonly_belongs_to(name, scope = nil, **options)
        belongs_to(name, scope, **options.merge(autosave: false))

        define_readonly_decorator(name)

        mutating_methods = mutating_singular_methods(name, allow_assignment: true)
        raise_on_mutating_methods!(mutating_methods)
      end

      private

      def inherited(subclass)
        super
        subclass.class_eval do
          @generated_decorator = nil
        end
      end

      def define_readonly_decorator(name, clone: false)
        @generated_decorator ||= Module.new.tap(&method(:prepend))

        if clone
          # Clone and cache the record to prevent side effects
          @generated_decorator.module_eval(<<~RUBY, __FILE__, __LINE__ + 1)
            def #{name}
              @_readonly_#{name} ||= begin
                if (record = super()) && !record.readonly?
                  record = record.clone
                  record.readonly!
                end
                record
              end
            end
          RUBY
        else
          @generated_decorator.send(:define_method, name) do
            if (record = super())
              record.readonly!
            end
            record
          end
        end
      end

      def build_readonly_scope(scope)
        if scope && scope.arity > 0
          ->(*args) {
            #: self as ActiveRecord::Relation
            readonly.instance_exec(*args, &scope)
          }
        elsif scope
          -> {
            #: self as ActiveRecord::Relation
            readonly.instance_exec(&scope)
          }
        else
          -> {
            #: self as ActiveRecord::Relation
            readonly
          }
        end
      end

      def raise_on_mutating_methods!(mutating_methods)
        mutating_methods.each do |method|
          alias_method(method, :raise_on_mutating_methods!)
          public(method)
        end
      end

      def mutating_collection_methods(name)
        ["#{name}=", "#{name.to_s.singularize}_ids="]
      end

      def mutating_singular_methods(name, allow_assignment: false)
        methods = ["build_#{name}", "create_#{name}", "create_#{name}!"]
        methods << "#{name}=" unless allow_assignment
        methods
      end
    end

    private

    def raise_on_mutating_methods!(...)
      raise ReadOnlyError, "Mutating methods are disabled for read-only associations"
    end
  end
end
"###;

/// Core's components/platform/essentials/app/models/concerns/accelerated_belongs_to.rb.
const ACCELERATED: &str = r###"module AcceleratedBelongsTo

  def accelerated_belongs_to(record, *args, **kwargs)
    # let rails do its normal belongs to.
    belongs_to_class = self #: as untyped
    belongs_to_class.belongs_to(record, *args, **kwargs)

    reflection = reflect_on_association(record)

    # Not yet adding Array Foreign Key support for Polymorphic.
    # Since all Postgres/Yugabyte Foreign Keys are an array even when only 1 long
    # this may need to be added in the future.
    array_fk = reflection.foreign_key.is_a?(Array)
    uses_array_foreign_keys = array_fk && !kwargs[:polymorphic]

    current_match_condition = if uses_array_foreign_keys
      # Don't call reflection.association_primary_key here — it triggers eager class
      # resolution via compute_type, which fails for associations with string class_name
      # values when the target class hasn't been loaded yet. Instead, resolve PK columns
      # lazily at runtime via _pk_columns (set in current_snippet below).
      fk_columns = Array(reflection.foreign_key)
      fk_columns.map.with_index { |fk, i|
        "#{fk} == current.read_attribute(_pk_columns[#{i}])"
      }.join(" && ")
    else
      "#{reflection.foreign_key} == current.id"
    end

    current_snippet = if kwargs[:polymorphic]
      <<-EOS
        reflection = self.class.reflect_on_association(:#{record})

        klass = reflection.foreign_type&.then do |foreign_type|
          send(foreign_type)&.constantize
        end

        current = klass.current if klass.respond_to?(:current)
      EOS
    elsif uses_array_foreign_keys
      <<-EOS
        _reflection = self.class.reflect_on_association(:#{record})
        current = _reflection.klass.current
        _pk_columns = current ? Array(_reflection.association_primary_key) : nil
      EOS
    else
      <<-EOS
        current = self.class.reflect_on_association(:#{record}).klass.current
      EOS
    end

    class_eval(<<-EOS, -__FILE__, __LINE__ + 1)
      def #{record}(*)
        #{current_snippet}

        if current && #{current_match_condition}
          current
        else
          super
        end
      end
    EOS
  end

  def accelerated_readonly_belongs_to(name, scope = nil, **options)
    accelerated_belongs_to(name, scope, **options.merge(autosave: false))

    alias_method(:"mutable_#{name}", name)

    class_eval(<<~RUBY, __FILE__, __LINE__ + 1)
      def #{name}(...)
        @_readonly_#{name} ||= begin
          if (record = mutable_#{name}(...))
            record = record.clone
            record.readonly!
          end
          record
        end
      end
    RUBY

    mutating_methods = mutating_singular_methods(name, allow_assignment: true)
    raise_on_mutating_methods!(mutating_methods)
  end
end
"###;

/// Core's components/platform/essentials/app/models/concerns/denormalize.rb.
const DENORMALIZE: &str = r###"module Denormalize
  extend ActiveSupport::Concern

  included do
    include AcceleratedBelongsTo
  end

  module ClassMethods
    def belongs_to_model_through(name, *args, class_name:, **options)
      belongs_to(name, class_name: class_name) # rubocop:disable ShopifyCore/AssociationClassName

      denormalize_class = self #: as untyped
      denormalize_class.define_denormalized_foreign_key(name, *args, class_name: class_name, **options)
    end

    protected

    def define_denormalized_foreign_key(name, *associations, class_name:, **options)
      foreign_key = -+"#{name}_id"

      if options.fetch(:validates, true)
        self #: as untyped
          .validates(foreign_key.to_sym, presence: true)
      end

      self #: as untyped
        .before_validation do
        self #: as untyped
          .retrieve_denormalized_foreign_key(name, foreign_key, associations, class_name)
      end

      define_method(foreign_key) do
        super() || self #: as untyped
          .retrieve_denormalized_foreign_key(name, foreign_key, associations, class_name)
      end
    end
  end

  def retrieve_denormalized_foreign_key(model, foreign_key, associations, class_name)
    id = self[foreign_key] #: untyped

    return id if id

    model_class = class_name.constantize

    if model_class&.respond_to?(:current)
      id = model_class.current&.id

      if id
        self[foreign_key] = id unless destroyed?

        return id
      end
    end

    associations.each do |association|
      record = send(association)

      id = if model == record.class.name.demodulize.underscore.to_sym
        record.id
      else
        record.try!(foreign_key)
      end

      break if id
    end

    self[foreign_key] = id if id && !destroyed?
    id
  end
end
"###;

/// Core's components/shop_identity/app/models/concerns/belongs_to_shop_through_association.rb.
const SHOP_THROUGH: &str = r###"module BelongsToShopThroughAssociation
  extend ActiveSupport::Concern

  module ClassMethods
    def belongs_to_shop_through(*args, **kwargs)
      accelerated_belongs_to(:shop, **kwargs.extract!(:primary_key, :foreign_key, :inverse_of))

      @generated_shop_accessor ||= begin
        generated_shop_accessor = Module.new
        generated_shop_accessor.send(:define_method, :shop) do
          #: self as Denormalize
          retrieve_denormalized_foreign_key(:shop, "shop_id", args, "Shop")
          super()
        end

        prepend(generated_shop_accessor)
        generated_shop_accessor
      end

      define_denormalized_foreign_key(:shop, *args, class_name: "Shop", **kwargs)
    end

    def readonly_belongs_to_shop_through(name, *args, clone: false, **kwargs)
      belongs_to_shop_through(name, *args, **kwargs.merge(autosave: false))

      define_readonly_decorator("shop", clone:)

      mutating_methods = mutating_singular_methods("shop", allow_assignment: true)
      raise_on_mutating_methods!(mutating_methods)
    end
  end
end
"###;

const SHOP: &str = r#"class Shop < ApplicationRecord
  class << self
    attr_accessor :current
  end
end
"#;

const ORDER: &str = r#"class Order < ApplicationRecord
  accelerated_belongs_to :shop
  has_many :line_items
  has_one :client_detail
end
"#;

const LINE_ITEM: &str = r#"class LineItem < ApplicationRecord
  belongs_to :order
  readonly_belongs_to_shop_through :order, clone: true, primary_key: :id, foreign_key: :shop_id, inverse_of: nil
end
"#;

const CLIENT_DETAIL: &str = r#"class ClientDetail < ApplicationRecord
  belongs_to :order
  belongs_to_shop_through :order, primary_key: :id, foreign_key: :shop_id, inverse_of: false
end
"#;

const PROBE: &str = r##"class ShopThroughProbe
  def self.lines
    out = []
    s1 = Shop.create!(name: "one")
    s2 = Shop.create!(name: "two")
    o1 = Order.create!(shop: s1)
    r = ClientDetail.reflect_on_association(:shop)
    out << "reflection: #{r.macro} #{r.foreign_key} #{r.klass.name} #{ClientDetail.reflect_on_association(:order).macro}"
    c = ClientDetail.create!(order: o1, shop: s1)
    c = ClientDetail.find(c.id)
    out << "reader: #{c.shop.name} #{c.shop_id == s1.id} #{c.order.shop.name}"
    c.shop = s2
    c.save!
    out << "writer: #{ClientDetail.find(c.id).shop.name}"
    l = LineItem.create!(order: o1, shop_id: s2.id, title: "a")
    l = LineItem.find(l.id)
    out << "line item: #{l.shop.name} #{LineItem.reflect_on_association(:shop).foreign_key}"
    out
  end
end
"##;

/// Native Rails 8.2 on the same PostgreSQL, so two identical wrong
/// answers still fail.
const EXPECTED: [&str; 4] = [
    "reflection: belongs_to shop_id Shop belongs_to",
    "reader: one true one",
    "writer: two",
    "line item: two shop_id",
];

/// What does not run: Denormalize's `define_denormalized_foreign_key`
/// declares a `before_validation` callback at class-body run time, which
/// the compiled model has no place for, so the call is refused by name
/// (the prepended `shop` reader and the readonly decorator with it); the
/// `shop` association it declares is compiled from the call.
const REFUSED: [&str; 2] = [
    "model macro call on `ClientDetail` not lowered; `belongs_to_shop_through` → `define_denormalized_foreign_key` reaches `validates`",
    "model macro call on `LineItem` not lowered; `readonly_belongs_to_shop_through` → `belongs_to_shop_through` → `define_denormalized_foreign_key` reaches `validates`",
];

/// What native Rails requires, in order.
const LOADS: [&str; 10] = [
    "app/models/concerns/component/read_only_associations",
    "app/models/concerns/accelerated_belongs_to",
    "app/models/concerns/denormalize",
    "app/models/concerns/belongs_to_shop_through_association",
    "app/models/application_record",
    "app/models/shop",
    "app/models/order",
    "app/models/line_item",
    "app/models/client_detail",
    "app/models/shop_through_probe",
];

const FILES: [(&str, &str); 14] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/concerns/component/read_only_associations.rb", READ_ONLY_ASSOCIATIONS),
    ("app/models/concerns/accelerated_belongs_to.rb", ACCELERATED),
    ("app/models/concerns/denormalize.rb", DENORMALIZE),
    ("app/models/concerns/belongs_to_shop_through_association.rb", SHOP_THROUGH),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
    ("app/models/order.rb", ORDER),
    ("app/models/line_item.rb", LINE_ITEM),
    ("app/models/client_detail.rb", CLIENT_DETAIL),
    ("app/models/shop_through_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn belongs_to_shop_through_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "belongs-to-shop-through",
        &FILES,
        &emitted,
        "rh_e2e_belongs_to_shop_through",
        "ShopThroughProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    for refused in REFUSED {
        assert!(
            errors.iter().any(|e| e.contains(refused) && e.contains("its `shop` association is compiled from the call")),
            "{refused}\n{}",
            errors.join("\n")
        );
    }
    assert_eq!(emitted_lines, native, "emitted");
}


/// The `shop` association the macro's `accelerated_belongs_to(:shop,
/// **kwargs.extract!(...))` declares is compiled on both models, though
/// the calls name `:order`; without PostgreSQL.
#[test]
fn belongs_to_shop_through_compiles_its_shop_association() {
    let (emitted, _) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    for model in ["client_detail", "line_item"] {
        let text = std::fs::read_to_string(emitted.join(format!("app/models/{model}.rb"))).unwrap();
        assert!(text.contains("  def shop\n"), "{model}: no `shop` reader\n{text}");
        assert!(text.contains(r#"FROM \"shops\" WHERE \"id\" = $1"#), "{model}: `shop` does not read shops by id\n{text}");
    }
}
