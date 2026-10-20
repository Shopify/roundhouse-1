//! `record.attributes` is keyed by column-name Strings, as in Rails.
//!
//! The catalog typed it `Hash[Symbol, String]`, so the analyzer judged
//! `record.attributes.symbolize_keys` the identity and `lower::symbolize_keys`
//! dropped the call. The emitted `attributes` answers String keys, so
//! Core's `Customers::RedactCustomerInput.from_redaction`
//! (`customer_redaction.attributes.symbolize_keys.slice(*schema_keys)`)
//! sliced Symbols out of a String-keyed Hash, got `{}`, and built the input
//! with every attribute nil: "Schema contains errors: {shop_id: [{error:
//! :presence}], …}".

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

#[test]
fn symbolize_keys_on_model_attributes_is_kept() {
    let files: &[(&str, &str)] = &[
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"redactions\", force: :cascade do |t|\n    t.integer \"shop_id\"\n    t.string \"email\"\n  end\nend\n",
        ),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/redaction.rb", "class Redaction < ApplicationRecord\nend\n"),
        (
            "app/models/redact_input.rb",
            "class RedactInput\n  def self.from_redaction(redaction)\n    redaction.attributes.symbolize_keys.slice(:shop_id, :email).compact\n  end\n\n  def self.build\n    from_redaction(Redaction.first)\n  end\nend\n",
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let out = ruby::emit_library(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("redact_input.rb"))
        .map(|f| f.content)
        .expect("redact_input.rb emitted");
    assert!(out.contains("symbolize_keys"), "symbolize_keys was dropped from a String-keyed receiver:\n{out}");
}
