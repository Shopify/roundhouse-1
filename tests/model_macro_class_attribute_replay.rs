//! A model macro that declares `class_attribute` on the model and writes
//! it (Core's `Taggable::ClassMethods#has_tags`: `class_attribute
//! :tag_associate_name, :tag_options`, then `self.tag_options = options`)
//! is replayed on the PostgreSQL CRuby tree, whose boot loads
//! ActiveSupport's `Class#class_attribute`
//! (runtime/spinel/active_support_class_attribute.rb). Refusing it dropped
//! `has_tags :product_tags` from Product, so `Product.tag_options` was
//! undefined and `Taggable#tags` raised NoMethodError. Other trees do not
//! carry the port, and still refuse it by name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const STRUCTURE: &str = r#"CREATE TABLE products (
    id bigserial NOT NULL,
    tag_string character varying,
    CONSTRAINT products_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_class_attribute_replay\n";

const CONCERN: &str = r##"module Tagged
  extend ActiveSupport::Concern

  module ClassMethods
    def has_tags(tag_associate_name = nil, options = {})
      class_attribute :tag_associate_name, :tag_options
      self.tag_associate_name = tag_associate_name
      self.tag_options = options
    end
  end
end
"##;

const MODEL: &str = r##"class Product < ApplicationRecord
  include Tagged

  has_tags :product_tags, limit: 5
end
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("Gemfile", "source \"https://rubygems.org\"\n")
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("config/database.yml", DATABASE_YML)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", STRUCTURE)
        .write("app/models/concerns/tagged.rb", CONCERN)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n")
        .write("app/models/product.rb", MODEL)
}

/// The typer's "no known method `tag_associate_name=`" on the copied
/// macro (a `class_attribute` the macro itself declares) is a gap warning
/// either way; the refusal is the error this guards.
fn refusals(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("model macro call") && e.contains("not lowered")).collect()
}

#[test]
fn a_macro_declaring_class_attributes_is_replayed_on_postgresql() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(refusals(&errors).is_empty(), "{}", errors.join("\n"));
    let product = std::fs::read_to_string(dir.join("app/models/product.rb")).expect("product.rb");
    assert!(product.contains("has_tags :product_tags, limit: 5") || product.contains("has_tags(:product_tags"), "the macro call was dropped:\n{product}");
    assert!(dir.join("runtime/active_support_class_attribute.rb").exists());
}

#[test]
fn a_tree_without_class_attribute_still_refuses_the_macro_by_name() {
    let (_dir, errors) = app()
        .write("roundhouse.yml", "database_adapter: sqlite3\n")
        .write("config/database.yml", "test:\n  adapter: sqlite3\n  database: \":memory:\"\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\n  create_table \"products\" do |t|\n    t.string \"tag_string\"\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let expected = "model macro call on `Product` not lowered; `has_tags` calls `class_attribute`, which the lowered `Product` does not define";
    assert!(errors.iter().any(|e| e.contains(expected)), "{errors:#?}");
}

