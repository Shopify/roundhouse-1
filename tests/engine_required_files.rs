//! An in-repo engine's class body loads files beside its source.
//!
//! Core's engines do it under `if Rails.env.test?`
//! (components/domains/lib/domains/engine.rb):
//! `require_relative "../../app/services/domains_dobby_service"` and
//! `require_relative "../../test/support/domains_dobby_service_mock"`,
//! a mock that reopens the service. The emitted engine sits elsewhere,
//! so the source's relative path misses (LoadError for
//! `app/app/services/…` and `app/test/support/…` in the release-10 boot
//! survey). The require now loads the emitted file of the source it
//! names. A `test/support` file no walk carries is carried as a file
//! program that defines its classes where the require runs, so the
//! mock's reopen is not merged into the service, and the files it
//! requires in turn are carried too.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ENGINE: &str = r#"require "rails"

module Widgets
  class Engine < Rails::Engine
    if Rails.env.test?
      paths.add("test/support", load_path: true)

      require_relative "../../app/services/widget_service"
      require_relative "../../test/support/widget_service_mock"
      require_relative "../../test/support/helpers/widget_helper"
    end
  end
end
"#;

const SERVICE: &str = r#"class WidgetService
  def self.price
    10
  end
end
"#;

const MOCK: &str = r#"class WidgetService
  class << self
    alias_method :price_from_service, :price

    def price
      price_from_service + 1
    end
  end
end
"#;

const HELPER: &str = r##"require_relative "../widget_constants"

module WidgetHelper
  def self.label
    "#{WIDGET_PREFIX}-#{WidgetService.price}"
  end
end
"##;

const CONSTANTS: &str = "WIDGET_PREFIX = \"w\"\n";

const FILES: &[(&str, &str)] = &[
    ("engines/widgets/lib/widgets/engine.rb", ENGINE),
    ("engines/widgets/app/services/widget_service.rb", SERVICE),
    ("engines/widgets/test/support/widget_service_mock.rb", MOCK),
    ("engines/widgets/test/support/helpers/widget_helper.rb", HELPER),
    ("engines/widgets/test/support/widget_constants.rb", CONSTANTS),
];

const PATH_GEMS: &str = "PATH\n  remote: engines/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  widgets!\n";

/// Boot the emitted tree as rails_engine_port.rs does (the full boot
/// needs the pg gem): Rails, the railtie and engine ports, then
/// app/models, which loads the engine.
fn boot(dir: &std::path::Path, env: &str) -> std::process::Output {
    emit_and_run::ruby()
        .current_dir(dir)
        .args([
            "-e",
            &format!(
                "require_relative 'runtime/active_support_lazy_load_hooks'\n\
                 $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
                 require_relative 'runtime/module_delegate'\n\
                 require_relative 'runtime/active_support_inflector'\n\
                 require_relative 'runtime/rails'\n\
                 Rails.env_name = {env:?}\n\
                 require_relative 'runtime/rails_railtie'\n\
                 require_relative 'runtime/rails_engine'\n\
                 require_relative 'runtime/active_support_deprecation'\n\
                 require_relative 'app/models'\n\
                 puts WidgetService.price\n\
                 puts defined?(WidgetHelper).inspect\n\
                 puts WidgetHelper.label if defined?(WidgetHelper)\n"
            ),
        ])
        .output()
        .expect("ruby")
}

#[test]
fn an_engine_body_loads_the_emitted_files_its_source_requires_relative() {
    let mut fixture = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_engine_required_files\n")
        .write("Gemfile.lock", PATH_GEMS);
    for (path, text) in FILES {
        fixture = fixture.write(path, text);
    }
    let (emitted, errors) = fixture.emit(BuildTarget::Ruby);
    // The fixture's only error is the PostgreSQL adapter this
    // fixture's emit does not compile in (rails_engine_port.rs boots
    // the same way); nothing about the engine's files.
    let own: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(own.is_empty(), "{own:?}");
    let engine = std::fs::read_to_string(emitted.join("app/models/widgets/engine.rb")).expect("the engine is carried");
    assert!(engine.contains("class Engine < Rails::Engine"), "{engine}");
    assert!(!engine.contains("require_relative \"../../"), "the source's relative paths are resolved:\n{engine}");
    let service = std::fs::read_to_string(emitted.join("app/models/widget_service.rb")).expect("the service");
    assert!(!service.contains("price_from_service"), "the mock is not merged into the service:\n{service}");

    let output = boot(&emitted, "test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "emitted program failed in {}:\n{stdout}\n{stderr}\n{engine}", emitted.display());
    assert!(!stderr.contains("LoadError") && !stderr.contains("roundhouse: error"), "{stderr}");
    assert_eq!(stdout, "11\n\"constant\"\nw-11\n", "{stderr}\n{engine}");
    // Outside the test environment the body requires nothing, and
    // neither the mock nor the helper is defined.
    let output = boot(&emitted, "development");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "emitted program failed in {}:\n{stderr}", emitted.display());
    assert_eq!(stdout, "10\nnil\n", "{stderr}");
}
