//! The ActiveRecord attribute surface Core's `ModelUuidColumns` reads
//! (activerecord 8.2.0.alpha): `abstract_class?` from the class body's
//! `self.abstract_class = true`, `self[name]` with a String name, and
//! `type_for_attribute` from the schema's column types. A class method
//! an ancestor defines (`uuid_attributes :shop_id`) runs in the model's
//! class body instead of being dropped.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "widgets", force: :cascade do |t|
    t.string "name", limit: 40
    t.integer "shop_id"
    t.text "notes"
    t.boolean "live"
    t.decimal "price"
  end
end
"#;

const CONCERN: &str = r#"module UuidColumns
  extend ActiveSupport::Concern

  class_methods do
    def uuid_attributes(*names)
      names.each { |name| UuidColumns.register(self, name.to_s) }
    end
  end

  def self.register(model, name)
    name
  end
end
"#;

fn emitted() -> Vec<(String, String)> {
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_e2e_attribute_api\n"),
        ("app/models/concerns/uuid_columns.rb", CONCERN),
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\n  include UuidColumns\nend\n",
        ),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\n  uuid_attributes :shop_id\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .into_iter()
        .map(|f| (f.path.to_string_lossy().to_string(), f.content))
        .collect()
}

fn file<'a>(files: &'a [(String, String)], suffix: &str) -> &'a str {
    files
        .iter()
        .find(|(p, _)| p.ends_with(suffix))
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| panic!("no {suffix} in {:?}", files.iter().map(|(p, _)| p).collect::<Vec<_>>()))
}

#[test]
fn the_abstract_class_write_and_an_inherited_dsl_call_stay_in_the_class_body() {
    let files = emitted();
    let base = file(&files, "application_record.rb");
    assert!(base.contains("  self.abstract_class = true\n"), "{base}");
    let widget = file(&files, "widget.rb");
    assert!(
        widget.contains("uuid_attributes(:shop_id)") || widget.contains("uuid_attributes :shop_id"),
        "{widget}"
    );
}

#[test]
fn the_schema_column_types_follow_the_postgresql_type_map() {
    let files = emitted();
    let widget = file(&files, "widget.rb");
    let start = widget.find("def self.schema_column_types").unwrap_or_else(|| panic!("{widget}"));
    let body = &widget[start..];
    let body = &body[..body.find("\n  end").unwrap_or(body.len())];
    for entry in [
        r#""id" => [:integer, 8]"#,
        r#""name" => [:string, 40]"#,
        r#""shop_id" => [:integer, 4]"#,
        r#""notes" => [:text, nil]"#,
        r#""live" => [:boolean, nil]"#,
        r#""price" => [:unported, "ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Decimal"]"#,
    ] {
        assert!(body.contains(entry), "{entry} missing from {body}");
    }
}

#[test]
fn the_record_reader_accepts_a_string_name() {
    let files = emitted();
    let widget = file(&files, "widget.rb");
    let start = widget.find("def [](name)").unwrap_or_else(|| panic!("{widget}"));
    assert!(widget[start..].contains("name = name.to_sym if name.is_a?(String)"), "{widget}");
    let start = widget.find("def []=(name, value)").unwrap_or_else(|| panic!("{widget}"));
    assert!(widget[start..].contains("name = name.to_sym if name.is_a?(String)"), "{widget}");
}
