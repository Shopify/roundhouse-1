//! A controller's concern replay extends the includer with `X::ClassMethods`
//! only for a Concern that has one.
//!
//! Core's `LoginProtection` includes `DeviceEventsConcern`, whose body is
//! `included do around_action … end` and nothing else. The replay of
//! `include LoginProtection` wrote `extend DeviceEventsConcern::ClassMethods`
//! for it as for every dependency, and `class AdminAreaController` raised
//! `uninitialized constant DeviceEventsConcern::ClassMethods`.
use std::sync::{Mutex, MutexGuard};

static INGEST: Mutex<()> = Mutex::new(());

fn ingest_exclusive() -> MutexGuard<'static, ()> {
    INGEST.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

const AUDIT: &str = r#"module AuditTrail
  extend ActiveSupport::Concern

  included do
    around_action :record_audit
  end

  private

  def record_audit
    yield
  end
end
"#;

const GUARD: &str = r#"module Guarded
  extend ActiveSupport::Concern
  include AuditTrail

  included do
    guard_with :login
  end

  module ClassMethods
    def guard_with(name)
      (@guards ||= []) << name
    end
  end
end
"#;

#[test]
fn a_dependency_without_class_methods_is_not_extended_with_them() {
    let _ingest = ingest_exclusive();
    let tree = [
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("app/controllers/concerns/audit_trail.rb", AUDIT),
        ("app/controllers/concerns/guarded.rb", GUARD),
        ("app/controllers/area_controller.rb", "class AreaController < ApplicationController\n  include Guarded\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\n  get \"/area\", to: \"area#index\"\nend\n"),
        ("roundhouse.yml", "test_paths:\n  - test\n"),
    ]
    .iter()
    .map(|(path, text)| (std::path::PathBuf::from(path), text.as_bytes().to_vec()))
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let statements = app
        .included_block_statements
        .get(&roundhouse::ident::ClassId(roundhouse::ident::Symbol::from("AreaController")))
        .expect("AreaController carries the block's statements");
    let text: Vec<String> = statements
        .iter()
        .map(|(after, e)| format!("{} {}", after.0, roundhouse::emit::ruby::emit_expr(e)))
        .collect();
    assert_eq!(
        text,
        vec!["Guarded extend Guarded::ClassMethods".to_string(), "Guarded guard_with :login".to_string()],
        "AuditTrail has no ClassMethods to extend with"
    );
}
