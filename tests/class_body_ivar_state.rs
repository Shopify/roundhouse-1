//! A class or module body's own `@ivar = value` writes are class-side
//! state, not dropped statements.
//!
//! Core's `ModelUuidColumns` seeds `@pending_uuid_columns = []` in its
//! module body and `register_uuid_attribute_for_model` then does
//! `@pending_uuid_columns << [model, name]`. The library walk dropped the
//! write without a diagnostic, so the first registration met nil.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::dialect::LibraryClass;
use roundhouse::ingest::{ingest_app_from_tree, ingest_library_class, ingest_library_classes, IngestError};

fn module_named(source: &str, name: &str) -> Result<LibraryClass, IngestError> {
    ingest_library_classes(source.as_bytes(), "app/models/registry.rb")
        .map(|lcs| lcs.into_iter().find(|lc| lc.name.0.as_str() == name).expect(name))
}

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "rooms", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#;

fn emitted(path: &str, source: &str) -> String {
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms\nend\n"),
        (path, source),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    let file = path.rsplit('/').next().unwrap();
    ruby::emit_library(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with(file))
        .map(|f| f.content)
        .expect(path)
}

const REGISTRY: &str = r#"module Registry
  DEFAULT = "none"

  @items = [] #: Array[String]
  @index ||= {} #: Hash[String, Integer]

  class << self
    def add(item)
      @items << item
      @index[item] = @items.size
    end

    def items
      @items
    end
  end
end
"#;

#[test]
fn a_module_body_ivar_write_is_a_class_side_initializer() {
    let lc = module_named(REGISTRY, "Registry").expect("ingest");
    assert_eq!(lc.class_ivar_initializers.len(), 2, "{:?}", lc.class_ivar_initializers);
    let src = emitted("app/models/registry.rb", REGISTRY);
    let items = src.find("@items = []").unwrap_or_else(|| panic!("{src}"));
    assert!(src.contains("@index ||= {}"), "{src}");
    let default = src.find("DEFAULT = ").unwrap_or_else(|| panic!("{src}"));
    let add = src.find("def self.add").unwrap_or_else(|| panic!("{src}"));
    assert!(default < items && items < add, "{src}");
}

#[test]
fn a_class_body_ivar_write_is_a_class_side_initializer() {
    let source = "class Counter\n  @count = 0\n\n  def self.bump\n    @count += 1\n  end\nend\n";
    let lc = ingest_library_class(source.as_bytes(), "app/models/counter.rb")
        .expect("ingest")
        .expect("class");
    assert_eq!(lc.class_ivar_initializers.len(), 1);
    let src = emitted("app/models/counter.rb", source);
    assert!(src.contains("  @count = 0\n"), "{src}");
}

#[test]
fn a_later_constant_reading_the_ivar_is_ordered_or_refused_not_reordered() {
    // A body the emit can keep in source order writes the snapshot
    // after the state it reads.
    let source = "module Registry\n  @items = []\n  SNAPSHOT = @items.dup\n  def self.items = @items\nend\n";
    let lc = module_named(source, "Registry").expect("ingest");
    assert_eq!(lc.class_ivar_initializers.len(), 1);
    let src = emitted("app/models/registry.rb", source);
    let items = src.find("@items = []").unwrap_or_else(|| panic!("{src}"));
    let snapshot = src.find("SNAPSHOT = ").unwrap_or_else(|| panic!("{src}"));
    assert!(items < snapshot, "{src}");
    // A later constant that does not read class state keeps the module.
    let fine = "module Registry\n  @items = []\n  LIMIT = 3\n  def self.items = @items\nend\n";
    let lc = module_named(fine, "Registry").expect("ingest");
    assert_eq!(lc.class_ivar_initializers.len(), 1);
    // One whose replayed DSL keeps the constants first is refused by
    // name rather than emitted reading the state before its write.
    let replayed = "class Plugin < Gem::Base\n  configure :early\n  @hooks = []\n  SNAPSHOT = @hooks.dup\nend\n";
    let (src, diagnostics) = roundhouse::emit::diagnostics::scope(|| emitted("app/models/plugin.rb", replayed));
    assert!(
        diagnostics.iter().any(|d| d.severity == roundhouse::diagnostic::Severity::Error
            && d.message.contains("Plugin::SNAPSHOT")
            && d.message.contains("source-order initialization")),
        "{diagnostics:?}\n{src}"
    );
}

#[test]
fn a_replayed_class_body_call_keeps_its_place_around_the_ivar_write() {
    // `Gem::Base` is not modelled, so the class body's calls replay; the
    // ivar the second call relies on must be written between the two.
    let source = r#"class Plugin < Gem::Base
  configure :early
  @hooks = []
  register :late
end
"#;
    let src = emitted("app/models/plugin.rb", source);
    let early = src.find("configure(:early)").or_else(|| src.find("configure :early")).unwrap_or_else(|| panic!("{src}"));
    let hooks = src.find("@hooks = []").unwrap_or_else(|| panic!("{src}"));
    let late = src.find("register(:late)").or_else(|| src.find("register :late")).unwrap_or_else(|| panic!("{src}"));
    assert!(early < hooks && hooks < late, "{src}");
}

#[test]
fn a_later_initializer_reads_the_type_an_earlier_one_wrote() {
    let source = r#"module Sampler
  @sample_rate = 0.10
  @adjusted_sample_count = (1.0 / @sample_rate).to_i

  def self.count = @adjusted_sample_count
end
"#;
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms\nend\n"),
        ("app/models/sampler.rb", source),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    let lc = app.library_classes.iter().find(|lc| lc.name.0.as_str() == "Sampler").expect("Sampler");
    let roundhouse::expr::ExprNode::Assign { value, .. } = &*lc.class_ivar_initializers[1].node else {
        panic!("{:?}", lc.class_ivar_initializers[1]);
    };
    assert_eq!(value.ty, Some(roundhouse::ty::Ty::Int), "{value:?}");
}

#[test]
fn a_model_body_ivar_write_is_a_class_side_initializer() {
    // Core's `Event` seeds `@admin_timeline_subject_types = {}` in its
    // class body and reads it through a `class << self` accessor; each
    // `event_timeline_types` initializer's `register_admin_timeline_subject_type`
    // then does `admin_timeline_subject_types[type] = builder`. The model
    // walk dropped the write without a diagnostic, so the first
    // registration met nil.
    let source = r#"class Room < ApplicationRecord
  DEFAULT_AUTHOR = "Shopify"

  @subject_types = {}

  class << self
    attr_accessor :subject_types

    def register_subject_type(type, builder)
      subject_types[type] = builder
    end
  end
end
"#;
    let src = emitted_model(source);
    let seed = src.find("@subject_types = {}").unwrap_or_else(|| panic!("{src}"));
    assert!(src.contains("def self.subject_types\n"), "{src}");
    assert!(src.find("DEFAULT_AUTHOR = ").is_some_and(|c| c < seed), "{src}");
    // The seed runs in the class body, not inside a method.
    let line = src[..seed].rsplit('\n').next().unwrap();
    assert_eq!(line, "  ", "{src}");
}

fn emitted_model(source: &str) -> String {
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/room.rb", source),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms\nend\n"),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("room.rb"))
        .map(|f| f.content)
        .expect("room.rb")
}
