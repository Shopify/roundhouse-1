//! A model's column reader answers nil until the column is assigned, NOT
//! NULL or not, so `expires_at.blank?` is not statically `false` just
//! because the column types `Time`.
//!
//! Core's refresh token sets `self.expires_at = ... if expires_at.blank?`
//! in `after_initialize`; `lower::blank` folded the condition to `false`,
//! the default never ran, and every token validated as expired.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

#[test]
fn a_not_null_time_column_blank_check_keeps_its_nil_arm() {
    let files: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema[7.1].define(version: 1) do\n  create_table \"tokens\", force: :cascade do |t|\n    t.datetime \"expires_at\", null: false\n  end\nend\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        (
            "app/models/token.rb",
            "class Token < ApplicationRecord\n  after_initialize :set_attributes\n\n  private\n\n  def set_attributes\n    self.expires_at = Time.current + 90 * 86_400 if expires_at.blank?\n  end\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(files).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let src = ruby::emit_lowered_models(&app)
        .iter()
        .find(|f| f.path.ends_with("token.rb"))
        .expect("model emitted")
        .content
        .clone();
    assert!(!src.contains("if false"), "the default must not fold away:\n{src}");
    assert!(src.contains("expires_at.nil?"), "blank? on the column reads its nil arm:\n{src}");
}
