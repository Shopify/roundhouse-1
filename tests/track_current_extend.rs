//! A model extending a module whose `extended` hook records the model.
//!
//! Core's `Shop` (and `User`) is `extend TrackCurrent`: the module's
//! instance methods (`current`, `current=`, `with_current`) become class
//! methods of the model, and `TrackCurrent.extended` appends the model to
//! the module's `tracked_classes` (a `module_function`). Shop's emitted
//! class dropped the extend, so `Shop.current` was undefined, and the
//! hook's implicit-self `tracked_classes` read was refused as `constant
//! not supported`.

use roundhouse::det::HashMap;
use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;
use std::path::PathBuf;

const TRACK_CURRENT: &str = r#"# typed: true
module TrackCurrent
  THREAD_VAR_PREFIX = "TrackCurrent::"

  def tracked_classes
    @tracked_classes ||= []
  end
  module_function :tracked_classes
  private :tracked_classes

  class << self
    def extended(mod)
      tracked_classes << mod
      super
    end
  end

  def current
    Thread.current.thread_variable_get(track_current_thread_variable_key)
  end

  def current=(obj)
    Thread.current.thread_variable_set(track_current_thread_variable_key, obj)
  end

  def reset_all
    tracked_classes.each do |klass|
      klass.current = nil
    end
  end
  module_function :reset_all

  private

  def track_current_thread_variable_key
    @track_current_thread_variable_key ||= THREAD_VAR_PREFIX + self.name
  end
end
"#;

const SHOP: &str = r#"# typed: true
class Shop < ApplicationRecord
  self.primary_key = :id

  DEFAULT_TIMEZONE = "Eastern Time (US & Canada)"

  extend TrackCurrent

  #: -> untyped
  def self.current_name = current&.name
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table \"shops\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
        ("app/utils/track_current.rb", TRACK_CURRENT),
        ("app/models/shop.rb", SHOP),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn emitted(app: &roundhouse::App, suffix: &str) -> String {
    let files: Vec<_> = ruby::emit_lowered_models(app).into_iter().chain(ruby::emit_library(app)).collect();
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with(suffix))
        .map(|f| f.content.clone())
        .unwrap_or_else(|| {
            panic!("no {suffix}; got {:?}", files.iter().map(|f| f.path.display().to_string()).collect::<Vec<_>>())
        })
}

#[test]
fn the_model_keeps_its_extend() {
    let app = app();
    let shop = emitted(&app, "models/shop.rb");
    assert!(shop.contains("extend TrackCurrent"), "{shop}");
    let errors: Vec<String> = roundhouse::analyze::diagnose(&app).into_iter().map(|d| d.to_string()).collect();
    assert!(!errors.iter().any(|e| e.contains("TrackCurrent")), "{errors:?}");
}

#[test]
fn the_extended_hook_reads_the_module_function() {
    let app = app();
    let tc = emitted(&app, "track_current.rb");
    assert!(!tc.contains("constant not supported"), "{tc}");
    assert!(tc.contains("TrackCurrent.tracked_classes << mod"), "{tc}");
}

