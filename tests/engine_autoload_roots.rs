//! An engine's `paths.add("app/liquid/drops", eager_load: true)` roots
//! that directory for autoloading, so `app/liquid/drops/line_item_drop.rb`
//! is `LineItemDrop` (not `Drops::LineItemDrop`, as under the `app/liquid`
//! root) and its constant loads the file on first reference, as Zeitwerk
//! does.
//!
//! Core's `LineItemDrop` has a statement in its class body the compiler
//! carries as a file program. With no root for its directory the program
//! was no constant's autoload: nothing required it, and natively
//! autoloaded `LineItemDrop::CurrentJsonSerializationMode.reset` (the
//! test helper's `process`) raised `NameError: uninitialized constant
//! ActionController::TestCase::Behavior::LineItemDrop` for every
//! controller test (v2-r45: 13 ids).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ENGINE: &str = r#"require "rails"

module Widgets
  class Engine < Rails::Engine
    paths.add("app/liquid/drops", eager_load: true)
    paths.add("app/liquid/lib", load_path: true)
  end
end
"#;

const DROP: &str = r#"puts "loading drop"

class WidgetDrop
  def self.name_for(id)
    "widget-#{id}"
  end
end

WidgetDrop.singleton_class.send(:define_method, :version) { 3 }
"#;

const PATH_GEMS: &str = "PATH\n  remote: engines/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  widgets!\n";

fn fixture() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_engine_autoload_roots\n")
        .write("Gemfile.lock", PATH_GEMS)
        .write("engines/widgets/lib/widgets/engine.rb", ENGINE)
        .write("engines/widgets/app/liquid/drops/widget_drop.rb", DROP)
}

#[test]
fn an_engines_eager_load_path_is_an_autoload_root_for_its_files_constants() {
    let (emitted, errors) = fixture().emit(BuildTarget::Ruby);
    let own: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(own.is_empty(), "{own:?}");
    let models = std::fs::read_to_string(emitted.join("app/models.rb")).expect("app/models.rb");
    assert!(
        models.contains("[:WidgetDrop, \"models/roundhouse_files/engines/widgets/app/liquid/drops/widget_drop\", false]")
            || models.contains("[:WidgetDrop, \"models/widget_drop\", false]"),
        "WidgetDrop is a top-level autoload, not nested under Drops:\n{}",
        models.lines().filter(|l| l.contains("WidgetDrop") || l.contains("UNREACHED")).collect::<Vec<_>>().join("\n")
    );
    assert!(!models.contains("[:Drops, "), "no `Drops` namespace under the app/liquid root");
}
