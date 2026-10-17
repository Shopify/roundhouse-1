//! `ActiveRecord::Associations::Preloader.new(records:, associations:).call`
//! checks clean, and runs on the Ruby-family runtime's own Preloader port
//! (`runtime/spinel/active_record_associations.rb`), which leaves a loaded
//! association's target and a collection's unsaved records alone as Rails
//! does. Upstream lowers an admitted call to the model's
//! `preload_associations` and refuses the class where no target ships it;
//! `lower::preloader::LOWERS_TO_MODEL_LOADER` keeps that rewrite off here.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;

fn constant_errors(body: &str) -> Vec<String> {
    constant_errors_with(body, &[])
}

fn constant_errors_with(body: &str, extra: &[(&str, &str)]) -> Vec<String> {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    for (path, src) in extra {
        tree.insert(PathBuf::from(path), src.as_bytes().to_vec());
    }
    tree.insert(
        PathBuf::from("db/schema.rb"),
        b"ActiveRecord::Schema.define do\n  create_table \"articles\" do |t|\n    t.string \"title\"\n  end\n  create_table \"comments\" do |t|\n    t.integer \"article_id\"\n  end\nend\n".to_vec(),
    );
    tree.insert(PathBuf::from("app/models/application_record.rb"), b"class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/article.rb"), b"class Article < ApplicationRecord\n  has_many :comments\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/comment.rb"), b"class Comment < ApplicationRecord\n  belongs_to :article\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/digest.rb"), format!("class Digest\n  def self.run\n{body}  end\nend\n").into_bytes());
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == roundhouse::diagnostic::Severity::Error)
        .map(|d| d.message)
        .filter(|m| m.contains("Preloader"))
        .collect()
}

#[test]
fn one_models_records_check_clean() {
    let errors = constant_errors(
        "    ActiveRecord::Associations::Preloader.new(records: Comment.all.to_a, associations: :article).call\n    ActiveRecord::Associations::Preloader.new(records: Article.all, associations: [:comments]).call\n    nil\n",
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

/// Rails' `call` answers the loaders; `preload_associations` answers nil,
/// so a call whose value is used is not lowered (`a_refused_call_is_not_rewritten`),
/// and runs on the runtime's Preloader port instead of being refused.
#[test]
fn a_used_value_runs_on_the_runtime_preloader() {
    let errors = constant_errors(
        "    if ActiveRecord::Associations::Preloader.new(records: Comment.all.to_a, associations: :article).call\n      1\n    end\n    ActiveRecord::Associations::Preloader.new(records: Comment.all.to_a, associations: :article).call\n",
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

/// Several models' records, or records of a type analysis does not know, are grouped by class at run time (`ActiveRecord::Preloading.records`), as Rails' preloader groups them.
#[test]
fn mixed_records_run_on_the_runtime_preloader() {
    let errors = constant_errors(
        "    ActiveRecord::Associations::Preloader.new(records: [Comment.first, Article.first], associations: :article).call\n    nil\n",
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

/// A `tap` or `each` block's value and an `if` branch whose `if` is a dropped statement are dropped too, so a call there checks clean; a method's tail is the method's value and keeps the refusal.
#[test]
fn a_dropped_block_or_branch_value_checks_clean() {
    let errors = constant_errors(
        "    Comment.all.to_a.tap { |rs| ActiveRecord::Associations::Preloader.new(records: rs, associations: :article).call }\n    if Comment.count > 0\n      ActiveRecord::Associations::Preloader.new(records: Comment.all.to_a, associations: :article).call\n    end\n    nil\n",
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

/// An application `tap` may answer its block's value, so the call stays a value,
/// which the runtime's Preloader port answers (upstream refuses it, where no
/// target ships the class).
#[test]
fn an_app_defined_tap_runs_on_the_runtime_preloader() {
    let errors = constant_errors_with(
        "    Basket.new.tap { ActiveRecord::Associations::Preloader.new(records: Comment.all.to_a, associations: :article).call }\n    nil\n",
        &[("app/models/basket.rb", "class Basket\n  def tap\n    yield\n  end\nend\n")],
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

/// Any option beside `records:` and `associations:` (`scope:`) is the port's own
/// keyword (`Preloader#initialize(records:, associations:, scope: nil, ...)`);
/// upstream refuses it, as its model-loader rewrite cannot carry the scope.
#[test]
fn a_scope_option_runs_on_the_runtime_preloader() {
    let errors = constant_errors(
        "    ActiveRecord::Associations::Preloader.new(records: Comment.all.to_a, associations: :article, scope: Article.where(id: 1)).call\n    nil\n",
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

fn lowered_sends(body: &str) -> Vec<String> {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    tree.insert(PathBuf::from("db/schema.rb"), b"ActiveRecord::Schema.define do\n  create_table \"comments\" do |t|\n    t.integer \"article_id\"\n  end\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/application_record.rb"), b"class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/comment.rb"), b"class Comment < ApplicationRecord\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/note.rb"), b"class Note\n  def article = nil\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/digest.rb"), format!("class Digest\n  def self.run\n{body}  end\nend\n").into_bytes());
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let digest = app.library_classes.iter().find(|c| c.name.0.as_str() == "Digest").expect("Digest");
    let mut out = Vec::new();
    fn walk(e: &roundhouse::Expr, out: &mut Vec<String>) {
        if let roundhouse::expr::ExprNode::Send { method, .. } = &*e.node {
            out.push(method.as_str().to_string());
        }
        e.node.for_each_child(&mut |c| walk(c, out));
    }
    for m in &digest.methods {
        walk(&m.body, &mut out);
    }
    out
}

/// The runtime ships a Preloader port that keeps loaded targets and unsaved
/// records, as Rails does, so no call is rewritten into the model's
/// `preload_associations` (which reloads): `LOWERS_TO_MODEL_LOADER`.
#[test]
fn a_preloader_call_is_not_rewritten_to_the_batch_loader() {
    for model in ["Comment.all.to_a", "[Note.new]"] {
        let body = format!("    ActiveRecord::Associations::Preloader.new(records: {model}, associations: :article).call\n    nil\n");
        let sends = lowered_sends(&body);
        assert!(!sends.iter().any(|m| m == "preload_associations"), "{sends:?}");
    }
}
