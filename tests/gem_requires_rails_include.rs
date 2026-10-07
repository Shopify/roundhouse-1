//! An `include` of a module only a Rails-dependent gem declares is
//! refused by name (error[gem_requires_rails]), at check and at the
//! include: no emitted tree loads that gem, so the source's include
//! cannot run. Shopify Core's concerns `include ArTransactionChanges`
//! (ar_transaction_changes 1.1.11), which was emitted as a raw include
//! and failed boot with a bare NameError and no diagnostic.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    ar_transaction_changes (1.1.11)\n\nDEPENDENCIES\n  activesupport\n  ar_transaction_changes\n";

const RBI: &str = "# typed: true\nmodule ArTransactionChanges\n  def transaction_changed_attributes; end\nend\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("Gemfile.lock", LOCK)
        .write("sorbet/rbi/gems/ar_transaction_changes@1.1.11.rbi", RBI)
}

#[test]
fn a_rails_dependent_gem_s_module_include_is_refused_by_name() {
    let run = app()
        .write("lib/geolocates.rb", "module Geolocates\n  include ArTransactionChanges\n\n  def where\n    :here\n  end\nend\n")
        .run_ruby("puts :booted\n");
    let refusal = "Geolocates includes ArTransactionChanges, declared by the ar_transaction_changes gem, whose load closure requires Rails";
    assert!(run.errors.iter().any(|e| e.contains("gem_requires_rails") && e.contains(refusal)), "{:#?}", run.errors);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stderr.contains(&format!("roundhouse: error[gem_requires_rails]: {refusal}")), "{}", run.stderr);
    assert!(run.stderr.contains("NotImplementedError"), "{}", run.stderr);
    assert!(!run.stderr.contains("uninitialized constant"), "{}", run.stderr);
}

#[test]
fn a_model_s_include_of_one_is_refused_by_name() {
    let (emitted, errors) = app()
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/widget.rb", "class Widget < ApplicationRecord\n  include ArTransactionChanges\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema[8.1].define(version: 2026_01_01_000000) do\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let refusal = "Widget includes ArTransactionChanges, declared by the ar_transaction_changes gem, whose load closure requires Rails";
    assert!(errors.iter().any(|e| e.contains("gem_requires_rails") && e.contains(refusal)), "{errors:#?}");
    let widget = std::fs::read_to_string(emitted.join("app/models/widget.rb")).expect("widget.rb");
    assert!(widget.contains(&format!("::Kernel.raise(::NotImplementedError, 'roundhouse: error[gem_requires_rails]: {refusal}")), "{widget}");
    assert!(!widget.contains("include ArTransactionChanges"), "{widget}");
}

#[test]
fn a_module_the_app_nests_in_the_includer_s_scope_is_the_app_s() {
    let run = app()
        .write("lib/shop/ar_transaction_changes.rb", "module Shop\n  module ArTransactionChanges\n    def changed_in_transaction\n      :app\n    end\n  end\nend\n")
        .write("lib/shop/geolocates.rb", "module Shop\n  class Geolocates\n    include ArTransactionChanges\n  end\nend\n")
        .run_ruby("puts Shop::Geolocates.new.changed_in_transaction\n");
    assert!(!run.errors.iter().any(|e| e.contains("gem_requires_rails")), "{:#?}", run.errors);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "app\n");
}
