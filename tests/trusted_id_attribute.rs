//! Core's `trusted_id_attribute` (component gem) defines
//! `trusted_<attr>` and records the id class in the class attribute its
//! `extended` hook declares. It was a dropped model-DSL statement on 787
//! Core lines; `ingest::trusted_id_attribute` lowers what it defines.

use roundhouse::det::HashMap;
use std::path::PathBuf;

const TRUSTED_ID: &str = "module Component\n  class TrustedId\n    attr_reader :db_id\n\n    def initialize(db_id)\n      @db_id = db_id\n    end\n  end\nend\n";

// Core's module, trimmed to the hook and the macro.
const ATTRIBUTES: &str = r#"module Component
  module TrustedIdAttributes
    class << self
      def extended(model)
        raise TypeError unless model < ActiveRecord::Base

        model.class_attribute(:trusted_id_attributes, default: {}, instance_accessor: false)
        model.singleton_class.send(:public, :trusted_id_attributes)
        model.singleton_class.send(:private, :trusted_id_attributes=)
      end
    end

    def trusted_scope(id_column, ids, trusted_id_filters = nil)
      where(id_column => ids)
    end

    def trusted_id_attribute(attr_name, id_class = Component::TrustedId, attribute: attr_name)
      self.trusted_id_attributes = trusted_id_attributes.merge(attr_name => id_class)
      trusted_ivar_name = "@_trusted_#{attr_name}"
      class_eval(<<-RUBY, __FILE__, __LINE__ + 1)
        def trusted_#{attr_name}
          db_id = #{attribute}
          return nil unless db_id

          return #{trusted_ivar_name} if #{trusted_ivar_name}&.db_id == db_id
          #{trusted_ivar_name} = #{id_class}.new(db_id)
        end
      RUBY
    end
  end
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/structure.sql", "CREATE TABLE products (\n    id bigint NOT NULL,\n    shop_id bigint,\n    variant_id bigint\n);\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("lib/component/trusted_id.rb", TRUSTED_ID),
        ("lib/component/trusted_id_attributes.rb", ATTRIBUTES),
        ("lib/merch/variant_id.rb", "module Merch\n  class VariantId < Component::TrustedId\n  end\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n  extend Component::TrustedIdAttributes\nend\n"),
        (
            "app/models/merch/product.rb",
            "module Merch\n  class Product < ApplicationRecord\n    trusted_id_attribute :shop_id\n    trusted_id_attribute :variant, VariantId, attribute: :variant_id\n    trusted_id_attribute :id, VariantId\n\n    def variant_db_id\n      trusted_variant.db_id\n    end\n  end\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

#[test]
fn trusted_id_attribute_defines_its_reader_and_records_the_class() {
    let app = app();
    let diagnostics: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    let dropped: Vec<&String> = diagnostics.iter().filter(|d| d.contains("trusted_id_attribute")).collect();
    // The `attribute: :id` form too: its reader tests composite_primary_key? when called.
    assert!(dropped.is_empty(), "{dropped:?}");
    assert!(!diagnostics.iter().any(|d| d.contains("trusted_variant") || d.contains("`db_id`")), "{diagnostics:?}");

    let files = roundhouse::emit::ruby::with_native_keyword_splats(|| roundhouse::emit::ruby::emit_lowered_models(&app));
    let src = |suffix: &str| {
        files
            .iter()
            .find(|f| f.path.to_string_lossy().ends_with(suffix))
            .map(|f| f.content.clone())
            .unwrap_or_else(|| panic!("{suffix} emitted"))
    };
    let product = src("models/merch/product.rb");
    for want in [
        "def trusted_shop_id",
        "db_id = self.shop_id",
        "@_trusted_shop_id = ::Component::TrustedId.new(db_id)",
        "def trusted_variant",
        "db_id = self.variant_id",
        "@_trusted_variant = ::Merch::VariantId.new(db_id)",
        "self.trusted_id_attributes = trusted_id_attributes.merge(shop_id: ::Component::TrustedId)",
        "self.trusted_id_attributes = trusted_id_attributes.merge(variant: ::Merch::VariantId)",
        "def trusted_id",
        "if self.class.composite_primary_key?",
        "@_trusted_id = ::Merch::VariantId.new(db_id)",
        "self.trusted_id_attributes = trusted_id_attributes.merge(id: ::Merch::VariantId)",
    ] {
        assert!(product.contains(want), "missing `{want}`:\n{product}");
    }
    let base = src("models/application_record.rb");
    assert!(base.contains("def self.trusted_id_attributes"), "{base}");
    assert!(base.contains("@__class_attr_trusted_id_attributes = {}"), "{base}");
    // The module's own methods reach the class side, without its hook.
    assert!(base.contains("class << self\n    include Component::TrustedIdAttributes\n  end"), "{base}");
}
