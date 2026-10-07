//! `prepend Mod` in a library-class body puts `Mod` ahead of the class
//! in its ancestors, so `Mod`'s methods run around the class's own and
//! reach them through `super`. The Ruby emitter dropped the line (a
//! `lower_residue` warning only): the wrapper silently never ran.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const GUARD: &str = "module Guard\n  def store = \"guard(#{super})\"\nend\n";
const BASE: &str = "module BaseStore\n  def store = \"base\"\nend\n";
const VAULT: &str = "class Vault\n  include BaseStore\n  prepend Guard\n\n  def store = \"vault(#{super})\"\nend\n";
const ASSERTIONS: &str = r#"
raise "store: #{Vault.new.store.inspect}" unless Vault.new.store == "guard(vault(base))"
raise "ancestors: #{Vault.ancestors.take(3).inspect}" unless Vault.ancestors.take(3) == [Guard, Vault, BaseStore]
"#;

#[test]
fn prepended_module_wraps_the_class_method_like_native() {
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{GUARD}\n{BASE}\n{VAULT}\n{ASSERTIONS}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    emit_and_run::real_blog()
        .write("app/services/guard.rb", GUARD)
        .write("app/services/base_store.rb", BASE)
        .write("app/services/vault.rb", VAULT)
        .run_ruby(ASSERTIONS)
        .assert_passes();
}

/// A `self.prepended(base)` hook runs at the prepend; one that calls a
/// class method the class defines needs that def above it, as in the
/// source. Written ahead of the defs, the hook raised NoMethodError at
/// load.
const AUDITED: &str = "module Audited\n  def self.prepended(base)\n    base.register(:audited)\n  end\n\n  def store = \"audited(#{super})\"\nend\n";
const LEDGER: &str = "class Ledger\n  def self.register(name)\n    (@registered ||= []) << name\n  end\n\n  def self.registered = @registered\n\n  def store = \"ledger\"\n\n  prepend Audited\n\n  def other = 1\nend\n";
const HOOK_ASSERTIONS: &str = r#"
raise "store: #{Ledger.new.store.inspect}" unless Ledger.new.store == "audited(ledger)"
raise "registered: #{Ledger.registered.inspect}" unless Ledger.registered == [:audited]
raise "ancestors: #{Ledger.ancestors.take(2).inspect}" unless Ledger.ancestors.take(2) == [Audited, Ledger]
"#;

#[test]
fn prepended_hook_runs_after_the_def_it_calls_like_native() {
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{AUDITED}\n{LEDGER}\n{HOOK_ASSERTIONS}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    emit_and_run::real_blog()
        .write("app/services/audited.rb", AUDITED)
        .write("app/services/ledger.rb", LEDGER)
        .run_ruby(HOOK_ASSERTIONS)
        .assert_passes();
}

/// The same prepend in a model body.
const STAMPED: &str = "module Stamped\n  def headline = \"[#{super}]\"\nend\n";
const STAMPED_POST: &str = "class StampedPost < ApplicationRecord\n  self.table_name = \"articles\"\n  prepend Stamped\n\n  def headline = title.upcase\nend\n";
const MODEL_ASSERTIONS: &str = r#"
post = StampedPost.new(title: "hi", body: "long enough body")
raise "headline: #{post.headline.inspect}" unless post.headline == "[HI]"
raise "ancestors: #{StampedPost.ancestors.take(2).inspect}" unless StampedPost.ancestors.take(2) == [Stamped, StampedPost]
"#;

#[test]
fn prepended_module_wraps_a_model_method_like_native() {
    let native_model = "require \"active_record\"\nActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\nActiveRecord::Schema.verbose = false\nActiveRecord::Schema.define { create_table(:articles) { |t| t.string :title; t.text :body } }\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{native_model}\n{STAMPED}\n{STAMPED_POST}\n{MODEL_ASSERTIONS}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    emit_and_run::real_blog()
        .write("app/models/concerns/stamped.rb", STAMPED)
        .write("app/models/stamped_post.rb", STAMPED_POST)
        .run_ruby(MODEL_ASSERTIONS)
        .assert_passes();
}

/// A controller's mixins are flattened into it, its own method winning
/// a collision: backwards for a prepended module. Not performed yet, so
/// each collision is named instead of silently running the controller's
/// own method alone.
const CONTROLLER_GUARD: &str = "module ControllerGuard\n  def index = \"guard(#{super})\"\nend\n";
const STAMPED_CONTROLLER: &str = "class StampedController < ApplicationController\n  prepend ControllerGuard\n\n  def index = \"stamped\"\nend\n";

/// Every diagnostic (any severity) the analysis, lowering and `target`
/// emit report for the app at `root`.
fn diagnostics(root: &std::path::Path, target: roundhouse::project::BuildTarget) -> Vec<String> {
    let mut app = roundhouse::ingest::ingest_app(root).expect("ingest");
    let lower = roundhouse::session::analyze_and_lower(&mut app);
    let (_, emit) = roundhouse::emit::diagnostics::scope(|| roundhouse::project::target_files(&app, root, target));
    roundhouse::analyze::diagnose(&app)
        .into_iter()
        .chain(lower)
        .chain(emit)
        .map(|d| format!("{:?}: {}", d.severity, d.message))
        .collect()
}

fn prepend_app() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("roundhouse-prepend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("app/controllers/concerns/controller_guard.rb", CONTROLLER_GUARD),
        ("app/controllers/stamped_controller.rb", STAMPED_CONTROLLER),
        ("app/services/guard.rb", GUARD),
        ("app/services/base_store.rb", BASE),
        ("app/services/vault.rb", VAULT),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :stamped, only: :index\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
    ] {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    root
}

#[test]
fn controller_prepend_collision_is_named_and_strict_targets_refuse_prepend() {
    let root = prepend_app();
    let ruby = diagnostics(&root, roundhouse::project::BuildTarget::Ruby);
    let collision: Vec<&String> = ruby.iter().filter(|d| d.contains("prepend not supported (ruby)")).collect();
    assert_eq!(collision.len(), 1, "{ruby:#?}");
    assert!(
        collision[0].starts_with("Warning: ")
            && collision[0].contains("`prepend ControllerGuard` in `StampedController`")
            && collision[0].contains("its own `index` shadows `ControllerGuard#index`"),
        "{collision:?}"
    );
    assert!(!ruby.iter().any(|d| d.contains("Vault") && d.contains("prepend")), "{ruby:#?}");

    let typescript = diagnostics(&root, roundhouse::project::BuildTarget::Typescript);
    let refused: Vec<&String> = typescript.iter().filter(|d| d.contains("prepend not supported (typescript)")).collect();
    assert!(
        refused.iter().any(|d| d.starts_with("Error: ") && d.contains("`prepend Guard` in `Vault`: this target has no prepended-module lookup")),
        "{typescript:#?}"
    );
    assert!(refused.iter().any(|d| d.contains("`prepend ControllerGuard` in `StampedController`")), "{typescript:#?}");
    let _ = std::fs::remove_dir_all(&root);
}
