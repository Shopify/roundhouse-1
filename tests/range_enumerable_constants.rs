//! Constants built from a Range's or `Integer#times`' elements.
//!
//! A Range is Enumerable, and blockless `n.times` is the Enumerator over
//! `0...n`, but only Array receivers answered the Enumerable builders.
//! So `(1..MAX).each_with_object({}) { ... }` and `MAX.times.map { ... }`
//! had no type, and every read of such a constant was refused as
//! `constant not supported`.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::ingest::ingest_app_from_tree;

const EWO: &str = r#"
module Ewo
  MAX = 3
  A = (1..MAX).each_with_object({}) { |n, h| h[n] = n }
  C = MAX.times.to_a
  D = MAX.times.map { |i| i * 2 }
  E = (1..MAX).map { |n| n.to_s }.freeze
  G = (0...MAX).select(&:even?)

  #: -> untyped
  def self.all = [A, C, D, E, G]
end
"#;

fn errors() -> Vec<String> {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/ewo.rb", EWO),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    diagnose(&app).into_iter().map(|d| d.to_string()).collect()
}

#[test]
fn range_and_times_builders_type_their_constants() {
    let errors = errors();
    assert!(!errors.iter().any(|e| e.contains("constant not supported")), "{errors:?}");
}
