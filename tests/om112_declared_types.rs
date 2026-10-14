//! Declared types Core writes that the analyzer refused as
//! `declared type not supported`:
//!
//! - Tapioca's per-model `Order::PrivateRelation` in a `#: as` cast or a
//!   `#:` ascription (Customers::StartCustomerRedactionTest, Pagination's
//!   SqlQueryOptimizerHelper): the relation over `Order`;
//! - a type alias read through a constant that holds a class
//!   (CheckoutAccountsCustomizations' `SchemaErrors = T.type_alias {
//!   BrandingSettingsSchemaBuilder::SchemaErrors }` with
//!   `BrandingSettingsSchemaBuilder = Checkouts::Core::BrandingSettingsSchemaBuilder`):
//!   the type `Checkouts::Core::BrandingSettingsSchemaBuilder` aliases.

use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!(
        "rh_om112_declared_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let base = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"orders\" do |t|\n    t.string \"name\"\n  end\nend\n",
        ),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        ("app/models/order.rb", "class Order < ApplicationRecord\nend\n"),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ];
    for (path, text) in base.iter().chain(files) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["check", "--continue"])
        .arg(&root)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn private_relation_is_the_relation_over_its_model() {
    let out = check(&[(
        "app/models/orders_report.rb",
        r#"# typed: strict
class OrdersReport
  #: (untyped) -> Integer
  def self.count_named(scope)
    orders = scope.where(name: "x") #: ::Order::PrivateRelation
    other = scope #: as Order::PrivateRelation
    orders.count + other.count
  end
end
"#,
    )]);
    assert!(!out.contains("declared type not supported"), "{out}");
    assert!(!out.contains("PrivateRelation"), "{out}");
}

#[test]
fn type_alias_through_a_constant_alias_is_the_aliased_type() {
    let out = check(&[
        (
            "app/models/checkouts/core/schema_builder.rb",
            r#"# typed: strict
module Checkouts
  module Core
    class SchemaBuilder
      SchemaErrors = T.type_alias { T::Hash[String, T::Array[String]] }
    end
  end
end
"#,
        ),
        (
            "app/models/input.rb",
            r#"# typed: strict
class Input
  SchemaBuilder = Checkouts::Core::SchemaBuilder

  SchemaErrors = T.type_alias { SchemaBuilder::SchemaErrors }

  #: -> void
  def initialize
    @errors = nil #: SchemaErrors?
  end
end
"#,
        ),
    ]);
    assert!(!out.contains("declared type not supported"), "{out}");
    assert!(!out.contains("SchemaErrors has no indexed"), "{out}");
}
