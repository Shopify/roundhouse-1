//! `self.table_name = "remote_domains"` binds a model to that table.
//!
//! Shopify's `DomainSubscription` stores in `remote_domains`. Ingest
//! derived the conventional `domain_subscriptions`, found no such table
//! and gave the model NO columns, so `subscription.shop_id` (a column
//! of `remote_domains`) read as "no known method `shop_id` on
//! DomainSubscription?" -- 38 errors on core, and 783 models declare
//! their table this way.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::{diagnose, Analyzer};
use roundhouse::ingest::ingest_app_from_tree;

fn failures(model_body: &str) -> Vec<String> {
    let files: Vec<(&str, String)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"remote_domains\", force: :cascade do |t|\n    t.integer \"shop_id\"\n  end\nend\n".into(),
        ),
        (
            "app/models/domain_subscription.rb",
            format!("class DomainSubscription < ApplicationRecord\n{model_body}\n  def self.owner\n    find_by(id: 1).shop_id\n  end\nend\n"),
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.into_bytes())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    diagnose(&app)
        .iter()
        .filter(|d| d.code() == "send_dispatch_failed")
        .map(|d| format!("{d:?}"))
        .collect()
}

#[test]
fn a_renamed_table_supplies_the_models_columns() {
    let f = failures("  self.table_name = \"remote_domains\"");
    assert!(f.is_empty(), "unexpected dispatch failures: {f:?}");
}

#[test]
fn without_the_declaration_the_conventional_table_is_used() {
    let f = failures("");
    assert_eq!(f.len(), 1, "no domain_subscriptions table, so no shop_id: {f:?}");
}

/// `self.table_name_prefix = "three_d_secure_"` in the class body (32
/// core models, e.g. `ThreeDSecure::Authentication`) prefixes the
/// derived name the way a namespace module's `table_name_prefix` does.
#[test]
fn a_class_level_table_name_prefix_is_applied() {
    let files: Vec<(&str, String)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"three_d_secure_authentications\", force: :cascade do |t|\n    t.string \"status\"\n  end\nend\n".into(),
        ),
        (
            "app/models/three_d_secure/authentication.rb",
            "module ThreeDSecure\n  class Authentication < ApplicationRecord\n    self.table_name_prefix = \"three_d_secure_\"\n\n    def self.st\n      find_by(id: 1).status\n    end\n  end\nend\n".into(),
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.into_bytes())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    let f: Vec<_> = diagnose(&app)
        .iter()
        .filter(|d| d.code() == "send_dispatch_failed")
        .map(|d| format!("{d:?}"))
        .collect();
    assert!(f.is_empty(), "unexpected dispatch failures: {f:?}");
}

fn dispatch_failures(files: Vec<(&str, String)>) -> Vec<String> {
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.into_bytes())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    diagnose(&app)
        .iter()
        .filter(|d| d.code() == "send_dispatch_failed")
        .map(|d| format!("{d:?}"))
        .collect()
}

/// `module RelationshipGraph; class << self; def table_name_prefix; …`
/// (the Vault app) prefixes `RelationshipGraph::Edge`'s table exactly
/// as `def self.table_name_prefix` does; the module's other singleton
/// methods beside it do not hide it.
#[test]
fn a_namespace_prefix_declared_in_class_self_is_applied() {
    let f = dispatch_failures(vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"relationship_graph_edges\", force: :cascade do |t|\n    t.string \"kind\"\n  end\nend\n".into(),
        ),
        (
            "app/models/relationship_graph.rb",
            "module RelationshipGraph\n  class << self\n    def table_name_prefix\n      \"relationship_graph_\"\n    end\n\n    def config\n      @config ||= {}\n    end\n  end\nend\n".into(),
        ),
        (
            "app/models/relationship_graph/edge.rb",
            "module RelationshipGraph\n  class Edge < ApplicationRecord\n    def self.kind_of_first\n      find_by(id: 1).kind\n    end\n  end\nend\n".into(),
        ),
    ]);
    assert!(f.is_empty(), "unexpected dispatch failures: {f:?}");
}

/// `self.table_name = "vault_vitals_responses"` in a class nested in a
/// module (`VaultVitals::Response`, whose conventional table would be
/// `responses`) binds the written table.
#[test]
fn a_table_name_written_in_a_namespaced_model_is_applied() {
    let f = dispatch_failures(vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"vault_vitals_responses\", force: :cascade do |t|\n    t.string \"answer\"\n  end\nend\n".into(),
        ),
        (
            "app/models/vault_vitals/response.rb",
            "module VaultVitals\n  class Response < ApplicationRecord\n    self.table_name = \"vault_vitals_responses\"\n\n    def self.first_answer\n      find_by(id: 1).answer\n    end\n  end\nend\n".into(),
        ),
    ]);
    assert!(f.is_empty(), "unexpected dispatch failures: {f:?}");
}

/// A `private` inside a model's `concerning` block belongs to the
/// concern's module. It used to refuse the whole model ("visibility
/// declarations inside a `concerning do … end` block are not
/// modeled"), dropping the class and with it its `self.table_name`
/// (Vault's `Ad`, `advertisements`).
#[test]
fn a_private_marker_in_a_concerning_block_keeps_the_model_and_its_table() {
    let f = dispatch_failures(vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"advertisements\", force: :cascade do |t|\n    t.string \"title\"\n  end\nend\n".into(),
        ),
        (
            "app/models/ad.rb",
            "class Ad < ApplicationRecord\n  self.table_name = \"advertisements\"\n\n  def self.first_title\n    find_by(id: 1).title\n  end\n\n  concerning :Approvable do\n    def approved?\n      submitter.present?\n    end\n\n    private\n\n    def submitter\n      title\n    end\n  end\n\n  private\n\n  def cost\n    1\n  end\nend\n".into(),
        ),
    ]);
    assert!(f.is_empty(), "unexpected dispatch failures: {f:?}");
}
