//! Call-site parameter types are keyed by (class, method, side): an
//! instance method and a class method of one name are two methods.
//!
//! Reduced from Shopify Core:
//!
//! - `CustomerFoundations::ActiveRecordHelper` has a class method
//!   `association_to_entity(object, association_name)` and an instance
//!   method `association_to_entity(association_name)` that calls it as
//!   `self.class.association_to_entity(self, association_name)`. Shared
//!   keying put the instance callers' Symbol into the class method's
//!   `object` slot, and `object.association(...)` failed on Symbol.
//! - `ShopVersioning` has `class << self` `increment_version_by_key(shop_id,
//!   version_key, visible_after:)` and an instance method
//!   `increment_version_by_key(version_key, visible_after:)` forwarding
//!   `self.id` cast to untyped. Shared keying typed the class method's
//!   `shop_id` with the instance callers' version-key String. The
//!   instance calls reach the concern's `def` through `include`, so their
//!   keywords are placed by the concern's shape, not left as one Hash.
use roundhouse::analyze::Analyzer;
use roundhouse::dialect::MethodReceiver;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::{App, ClassId, Symbol, Ty};
use std::path::PathBuf;

const SOURCES: &[(&str, &str)] = &[
    (
        "app/models/application_record.rb",
        "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
    ),
    (
        "db/schema.rb",
        r#"ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "shops", force: :cascade do |t|
    t.string "name"
  end
  create_table "shop_versions", force: :cascade do |t|
    t.integer "products_version"
  end
  create_table "customers", force: :cascade do |t|
    t.string "name"
  end
end
"#,
    ),
    (
        "app/models/concerns/shop_versioning.rb",
        r#"module ShopVersioning
  extend ActiveSupport::Concern

  class << self
    def increment_version_by_key(shop_id, version_key, visible_after: nil)
      visible_after ||= 5
      ShopVersions.increment(version_key, shop_id: shop_id, visible_after: visible_after)
    end
  end

  def increment_version_by_key(version_key, visible_after: nil)
    ShopVersioning.increment_version_by_key(
      self #: as untyped
        .id,
      version_key,
      visible_after: visible_after
    )
  end
end
"#,
    ),
    (
        "app/models/shop_versions.rb",
        r#"class ShopVersions < ApplicationRecord
  class << self
    def increment(key, shop_id:, visible_after: nil)
      [Array(shop_id), key, visible_after]
    end
  end
end
"#,
    ),
    ("app/models/shop.rb", "class Shop < ApplicationRecord\n  include ShopVersioning\nend\n"),
    (
        "app/models/concerns/entity_helper.rb",
        r#"module EntityHelper
  extend ActiveSupport::Concern

  class_methods do
    def association_to_entity(object, association_name)
      object.association(association_name)
    end
  end

  def association_to_entity(association_name)
    self.class.association_to_entity(self, association_name)
  end
end
"#,
    ),
    (
        "app/models/customer.rb",
        r#"class Customer < ApplicationRecord
  include EntityHelper

  def to_entity
    association_to_entity(:default_address)
  end
end
"#,
    ),
    (
        "app/models/region.rb",
        r#"class Region < ApplicationRecord
  def run
    Shop.find(1).increment_version_by_key("products_version", visible_after: 10)
  end
end
"#,
    ),
];

fn analyzed() -> App {
    let tree = SOURCES
        .iter()
        .map(|(path, src)| (PathBuf::from(path), src.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("fixture ingests");
    Analyzer::new(&app).analyze(&mut app);
    app
}

fn params<'a>(app: &'a App, class: &str, method: &str, side: MethodReceiver) -> &'a [Ty] {
    app.inferred_method_params
        .get(&(ClassId(Symbol::from(class)), Symbol::from(method), side))
        .unwrap_or_else(|| panic!("{class} {method} ({side:?}) has inferred params"))
}

fn admits(ty: &Ty, want: &Ty) -> bool {
    ty == want || matches!(ty, Ty::Union { variants } if variants.contains(want))
}

#[test]
fn a_class_method_keeps_the_receiver_its_instance_twin_passes() {
    let app = analyzed();
    // `self.class.association_to_entity(self, association_name)`.
    let class_side = params(&app, "EntityHelper", "association_to_entity", MethodReceiver::Class);
    assert!(
        matches!(&class_side[0], Ty::Class { id, .. } if id.0.as_str() != "Symbol"),
        "{class_side:?}"
    );
    assert!(!admits(&class_side[0], &Ty::Sym), "{class_side:?}");
    // `association_to_entity(:default_address)`.
    let instance_side = params(&app, "EntityHelper", "association_to_entity", MethodReceiver::Instance);
    assert_eq!(instance_side, &[Ty::Sym]);
}

#[test]
fn class_and_instance_twins_type_their_own_slots() {
    let app = analyzed();
    let class_side = params(&app, "ShopVersioning", "increment_version_by_key", MethodReceiver::Class);
    assert_eq!(class_side.len(), 3, "{class_side:?}");
    // `self #: as untyped .id` is authored untyped.
    assert_eq!(class_side[0], Ty::Untyped, "{class_side:?}");
    assert_eq!(class_side[1], Ty::Str, "{class_side:?}");
    assert!(admits(&class_side[2], &Ty::Int) && admits(&class_side[2], &Ty::Nil), "{class_side:?}");
    let instance_side =
        params(&app, "ShopVersioning", "increment_version_by_key", MethodReceiver::Instance);
    assert_eq!(instance_side, &[Ty::Str, Ty::Int]);
}

#[test]
fn an_includer_call_is_placed_by_the_module_def_shape() {
    let app = analyzed();
    // `shop.increment_version_by_key("…", visible_after: 10)` reaches
    // the concern's def; `visible_after:` binds its keyword, so no slot
    // downstream sees the kwargs Hash.
    let increment = params(&app, "ShopVersions", "increment", MethodReceiver::Class);
    assert_eq!(increment[0], Ty::Str, "{increment:?}");
    assert!(
        !increment.iter().any(|t| matches!(t, Ty::Hash { .. })
            || matches!(t, Ty::Union { variants } if variants.iter().any(|v| matches!(v, Ty::Hash { .. })))),
        "{increment:?}"
    );
}
