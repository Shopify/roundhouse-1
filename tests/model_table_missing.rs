//! A concrete model whose table the schema does not declare is an error.
//!
//! 47 Core models read tables no schema has, most through table-name
//! rules ingest did not apply (nested-in-model, acronyms). Each emitted
//! query named a relation that does not exist, and check said nothing.
//! An abstract base, an STI subclass (its parent's table) and a
//! tableless ActiveModel class are not this. A model routed to another
//! configuration is checked against the same schema unless that
//! configuration names a separate schema file.

use roundhouse::det::HashMap;
use std::path::PathBuf;

#[test]
fn a_concrete_model_without_a_schema_table_is_reported() {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/structure.sql", "CREATE TABLE widgets (\n    id bigint NOT NULL,\n    type character varying\n);\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\nend\n"),
        ("app/models/gadget.rb", "class Gadget < Widget\nend\n"),
        ("app/models/gizmo.rb", "class Gizmo < ApplicationRecord\nend\n"),
        // Routed to another configuration, with no manifest naming a
        // separate schema file: Core points every configuration at one
        // database, so the same schema applies.
        ("app/models/analytics_record.rb", "class AnalyticsRecord < ApplicationRecord\n  self.abstract_class = true\n  connects_to database: { writing: :analytics }\nend\n"),
        ("app/models/report.rb", "class Report < AnalyticsRecord\nend\n"),
        ("app/models/search.rb", "class Search\n  include ActiveModel::Validations\n  validates :q, presence: true\nend\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .map(|d| d.to_string())
        .filter(|d| d.contains("model_table_not_in_schema"))
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("`Gizmo` reads table `gizmos`")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("`Report` reads table `reports`")), "{errors:?}");
}
