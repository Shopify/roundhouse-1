//! The by-NAME association rung (`scope_chain::resolve_assoc_read`) only
//! guesses for an owner that nothing types, and only in a model body.
//!
//! One model declaring `has_many :extensions` made every `x.extensions
//! .any?` in the app read as that association. Shopify Core's
//! `GraphApi::HasFieldOverride` writes `interface_field.extensions.any?`
//! on a `GraphQL::Schema::Field`; the seed rewrote it into
//! `Relation.new(Plus::Trial::Extension).where(commitment_id:
//! interface_field.id)`, and loading the `shops` fixtures died on
//! `undefined method 'id' for an instance of GraphApi::Field`. The
//! source spelling dispatches to whatever `extensions` really is.
//!
//! Outside a model body the guess is still taken for a call the has_many
//! reader's Array cannot answer (`exists?`, `joins`, `create!`): there the
//! source spelling fails on a record, so the seed is the only reading that
//! can work.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "commitments", force: :cascade do |t|
    t.string "name", null: false
  end
  create_table "extensions", force: :cascade do |t|
    t.integer "commitment_id", null: false
  end
  create_table "shops", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#;

fn emitted() -> Vec<(String, String)> {
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        (
            "app/models/commitment.rb",
            "class Commitment < ApplicationRecord\n  has_many :extensions\nend\n",
        ),
        (
            "app/models/extension.rb",
            "class Extension < ApplicationRecord\n  belongs_to :commitment\nend\n",
        ),
        (
            "app/models/shop.rb",
            r#"class Shop < ApplicationRecord
  def any_extensions?(c)
    return false if Commitment.where(name: name).none?
    c.extensions.any?
  end

  def field_extensions?
    return false if Commitment.where(name: name).none?
    f = FakeField.new([])
    f.extensions.any?
  end
end
"#,
        ),
        (
            "app/models/fake_field.rb",
            r#"class FakeField
  attr_reader :extensions

  def initialize(extensions)
    @extensions = extensions
  end
end
"#,
        ),
        (
            "app/models/field_override.rb",
            r#"module FieldOverride
  def self.copy(list)
    return [] if Commitment.where(name: "x").none?
    f = list.first
    if f.extensions.any?
      f.extensions.map { |e| e }
    else
      []
    end
  end

  def instance_scan(fields)
    return [] if Commitment.where(name: "x").none?
    fields.select { |field| field.extensions.any? }
  end

  def self.extended?(record)
    return false if Commitment.where(name: "x").none?
    record.extensions.exists?
  end
end
"#,
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .into_iter()
        .chain(ruby::emit_library(&app))
        .map(|f| (f.path.display().to_string(), f.content))
        .collect()
}

fn file(files: &[(String, String)], suffix: &str) -> String {
    files
        .iter()
        .find(|(p, _)| p.ends_with(suffix))
        .map(|(_, c)| c.clone())
        .unwrap_or_else(|| {
            panic!("no emitted {suffix}; got {:?}", files.iter().map(|(p, _)| p).collect::<Vec<_>>())
        })
}

/// Plain library code: `f` and the block's `field` are untyped, and the
/// module is no model's body, so both reads keep their source shape.
/// Each body opens a model chain, so the rewriter does visit it.
#[test]
fn untyped_owner_in_plain_library_code_keeps_the_dynamic_read() {
    let out = file(&emitted(), "app/models/field_override.rb");
    assert!(out.contains("if f.extensions.any?"), "kept `f.extensions.any?`:\n{out}");
    assert!(out.contains("field.extensions.any?"), "kept `field.extensions.any?`:\n{out}");
    assert!(
        !out.contains("commitment_id: f.id") && !out.contains("commitment_id: field.id"),
        "no guessed seed off `f` / `field`:\n{out}"
    );
}

/// The same plain module, a call the reader's Array has no answer for:
/// `record.extensions.exists?` would be a NoMethodError on the
/// folded reader, so the by-name guess still seeds it.
#[test]
fn untyped_owner_in_plain_library_code_seeds_a_relation_only_call() {
    let out = file(&emitted(), "app/models/field_override.rb");
    assert!(
        out.contains("ActiveRecord::Relation.new(Extension).where(commitment_id: record.id)"),
        "`exists?` seeds by name:\n{out}"
    );
}

/// Inside a model body the stamped `FakeField` owner is still not a
/// Commitment, while the untyped parameter keeps the by-name seed it
/// had before.
#[test]
fn model_body_seeds_the_untyped_owner_but_not_the_typed_non_model() {
    let out = file(&emitted(), "app/models/shop.rb");
    assert!(
        out.contains("ActiveRecord::Relation.new(Extension).where(commitment_id: c.id)"),
        "untyped owner in a model body still seeds by name:\n{out}"
    );
    assert!(out.contains("f.extensions.any?"), "a FakeField owner keeps its own reader:\n{out}");
    assert!(!out.contains("commitment_id: f.id"), "no seed off a FakeField:\n{out}");
}
