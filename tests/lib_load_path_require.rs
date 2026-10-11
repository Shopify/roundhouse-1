//! A method body's `require "ns/thing"` of a carried file below the
//! application's or an engine's `lib` loads it at run time, as natively:
//! Rails puts each `lib` on `$LOAD_PATH` in every environment (railties
//! engine/configuration.rb `paths.add "lib", load_path: true`), and the
//! emitted tree has no such directory, so the feature is answered by
//! `RoundhouseAutoload::LIB_LOAD_PATH_FEATURES`. A feature two `lib`s
//! carry is left to Kernel's `require` (LoadError), as with the test
//! load path.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ENGINE: &str = "require \"rails\"\n\nmodule Widgets\n  class Engine < ::Rails::Engine\n  end\nend\n";

const LOCK: &str = "PATH\n  remote: components/widgets\n  specs:\n    widgets (0.1.0)\n\nDEPENDENCIES\n  widgets!\n";

const PROBER: &str = r#"class Prober
  def self.load_app_lib
    require "zz/thing"
    ZzThing.name
  end

  def self.load_engine_lib
    require "widgets/thing"
    Widgets::Thing.name
  end

  def self.load_ambiguous
    require "dup/thing"
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_lib_load_path\n")
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("Gemfile.lock", LOCK)
        .write("components/widgets/lib/widgets/engine.rb", ENGINE)
        .write("components/widgets/lib/widgets/thing.rb", "module Widgets\n  class Thing\n  end\nend\n")
        .write("components/widgets/lib/dup/thing.rb", "module DupFromEngine\nend\n")
        .write("lib/zz/thing.rb", "class ZzThing\nend\n")
        .write("lib/dup/thing.rb", "module DupFromApp\nend\n")
        .write("app/models/prober.rb", PROBER)
}

#[test]
fn a_carried_file_below_an_engine_or_application_lib_answers_its_require_at_run_time() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| !e.contains("database_adapter not supported")), "{}", errors.join("\n"));
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("models");
    let table = &models[models.find("LIB_LOAD_PATH_FEATURES = {").unwrap_or_else(|| panic!("no table in\n{models}"))..];
    let table = &table[..table.find("}.freeze").expect("table end")];
    assert!(table.contains("\"zz/thing\" =>"), "{table}");
    assert!(table.contains("\"widgets/thing\" =>"), "{table}");
    assert!(!table.contains("\"dup/thing\" =>"), "{table}");

    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             Rails.env_name = 'development'\n\
             require_relative 'runtime/rails_railtie'\n\
             require_relative 'runtime/rails_engine'\n\
             require_relative 'runtime/active_support_deprecation'\n\
             require_relative 'app/models'\n\
             puts Prober.load_app_lib\n\
             puts Prober.load_engine_lib\n\
             begin; Prober.load_ambiguous; rescue LoadError => e; puts \"LoadError\"; end\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "ZzThing\nWidgets::Thing\nLoadError\n", "{stderr}");
}
