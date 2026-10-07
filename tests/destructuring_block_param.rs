//! A destructuring block parameter (`|(k, v), memo|`).
//!
//! Ingest kept only plain required parameters, so `(k, v)` was dropped:
//! `{ a: 1 }.each_with_object({}) { |(k, v), h| h[k] = v }` emitted as
//! `{ |h| h[k] = v }`, shifting `h` onto the pair and leaving `k` and
//! `v` unbound (`undefined local variable or method 'k'` at load).

use roundhouse::emit::ruby::emit_lowered_models;
use roundhouse::ingest::ingest_app_from_tree;

const TALLY: &str = r#"
class Tally < ApplicationRecord
  def self.flip(pairs)
    pairs.each_with_object({}) { |(k, v), h| h[v] = k }
  end

  def self.sums(pairs)
    pairs.map(&->((a, b)) { a + b })
  end
end
"#;

fn emitted() -> String {
    let tree = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\n  create_table :tallies do |t|\n  end\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
        ("app/models/tally.rb", TALLY),
    ]
    .into_iter()
    .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    emit_lowered_models(&app)
        .into_iter()
        .filter(|f| f.path.to_string_lossy().ends_with("tally.rb"))
        .map(|f| f.content)
        .collect()
}

#[test]
fn a_destructured_pair_binds_its_names_ahead_of_the_body() {
    let out = emitted();
    let flip = out.lines().skip_while(|l| !l.contains("def self.flip")).take(6).collect::<Vec<_>>().join("\n");
    assert!(flip.contains("|__param_") && flip.contains(", h|"), "{out}");
    assert!(flip.contains("k, v = __param_"), "{out}");
}

#[test]
fn a_lambda_destructures_its_parameter_too() {
    let out = emitted();
    assert!(out.contains("a, b = __param_"), "{out}");
}
