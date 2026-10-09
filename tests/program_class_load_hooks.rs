//! A gem source's `ActiveSupport.run_load_hooks` runs where its file
//! runs it, behind the requires the file writes above it
//! (`project::apply_class_load_hooks`).
//!
//! - A class-body call ran from the class's declarations file. Another
//!   file program can load those declarations first: hedwig 1.68.0's
//!   configuration.rb requires configuration/command_table.rb, which
//!   reopens `class Configuration`, before its own `require_relative
//!   "configuration/dequeue_decorator"`. The hook ran there, and Core's
//!   `on_load(:hedwig)` `Hedwig.configure` raised NameError on
//!   `Hedwig::Configuration::DequeueDecorator` (unit-1000 r3). The hook
//!   now follows the file program's load of the declarations.
//! - A file-level call is a statement of the file's program already, and
//!   ran a second time from the class's declarations file
//!   (active_model_serializers 0.9.13's railtie.rb).
//!
//! The fixture has the same shape: a gem source `hookkit`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const HOOKKIT: &str = r#"require_relative "hookkit/version"

module Hookkit
  autoload :Configuration, "hookkit/configuration"

  # hedwig's shape: the hooks that loading Configuration runs configure
  # first.
  def self.configure
    _ = Configuration
    @configuration ||= Configuration.new
    yield @configuration
  end

  def self.configuration
    @configuration || raise("not configured")
  end

  def self.loaded
    @loaded ||= []
  end
end

ActiveSupport.run_load_hooks(:hookkit_loaded, Hookkit)
"#;
const VERSION: &str = "module Hookkit\n  VERSION = \"1.0.0\"\nend\n";
const CONFIGURATION: &str = r#"require_relative "configuration/command_table"
require_relative "configuration/dequeue_decorator"

module Hookkit
  class Configuration
    attr_accessor :decorator

    def tables
      CommandTable::BACKENDS
    end

    ActiveSupport.run_load_hooks(:hookkit, Hookkit)
  end
end
"#;
const COMMAND_TABLE: &str = r#"require_relative "command_table/memory"

module Hookkit
  class Configuration
    class CommandTable
      BACKENDS = [Memory].freeze
    end
  end
end
"#;
const MEMORY: &str = "module Hookkit\n  class Configuration\n    class CommandTable\n      class Memory\n      end\n    end\n  end\nend\n";
const DEQUEUE_DECORATOR: &str = "module Hookkit\n  class Configuration\n    class DequeueDecorator\n      def self.label = \"dequeue\"\n    end\n  end\nend\n";

/// Core's on_load(:hedwig) shape: configure with a class the hooked
/// file requires above its class body.
const INITIALIZER: &str = r#"ActiveSupport.on_load(:hookkit) do
  Hookkit.configure { |config| config.decorator = Hookkit::Configuration::DequeueDecorator }
end

ActiveSupport.on_load(:hookkit_loaded) do
  loaded << :hookkit
end
"#;

const SCRIPT: &str = "p Hookkit.loaded\nHookkit.configure { |config| p config.decorator.label }\np Hookkit.configuration.tables\n";
const EXPECTED: &str = "[:hookkit]\n\"dequeue\"\n[Hookkit::Configuration::CommandTable::Memory]\n";

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    hookkit (1.0.0)\n\nDEPENDENCIES\n  hookkit\n";
const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n\ndevelopment:\n  <<: *default\n  database: hook_probe\n\ntest:\n  <<: *default\n  database: hook_probe\n";

const FILES: [(&str, &str); 6] = [
    ("hookkit.rb", HOOKKIT),
    ("hookkit/version.rb", VERSION),
    ("hookkit/configuration.rb", CONFIGURATION),
    ("hookkit/configuration/command_table.rb", COMMAND_TABLE),
    ("hookkit/configuration/command_table/memory.rb", MEMORY),
    ("hookkit/configuration/dequeue_decorator.rb", DEQUEUE_DECORATOR),
];

const GEM: &str = "vendor/gems/hookkit-1.0.0/lib";

/// A PostgreSQL tree (its boot runs the initializers' `on_load` and the
/// gem files' `run_load_hooks`) that Bundler-requires the gem.
fn app(eager: bool) -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\ngem_sources:\n  hookkit: vendor/gems/hookkit-1.0.0\n")
        .write("config/database.yml", DATABASE_YML)
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"hookkit\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("config/initializers/hookkit.rb", INITIALIZER)
        .write(
            "config/environments/development.rb",
            &format!("Rails.application.configure do\n  config.eager_load = {eager}\nend\n"),
        );
    for (path, text) in FILES {
        app = app.write(&format!("{GEM}/{path}"), text);
    }
    app
}

#[test]
fn the_native_control_configures_with_the_required_decorator() {
    let root = std::env::temp_dir().join(format!("roundhouse-program-class-load-hooks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in FILES {
        let file = root.join("lib").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let script = format!("require \"active_support\"\nrequire \"active_support/lazy_load_hooks\"\n{INITIALIZER}require \"hookkit\"\n{SCRIPT}");
    let native = emit_and_run::ruby().arg("-I").arg(root.join("lib")).args(["-e", &script]).output().expect("CRuby control");
    let _ = std::fs::remove_dir_all(&root);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);
}

#[test]
fn a_class_body_hook_runs_after_its_files_requires_and_a_file_level_hook_once() {
    let (out, errors) = app(false).emit(BuildTarget::Ruby);
    let errors: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter")).collect();
    assert!(errors.iter().all(|e| !e.contains("run_load_hooks")), "{errors:#?}");
    let read = |rel: &str| std::fs::read_to_string(out.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));

    let program = read(&format!("app/models/roundhouse_files/{GEM}/hookkit/configuration.rb"));
    let line = |needle: &str| {
        program
            .lines()
            .position(|line| line.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("no `{needle}` in the program:\n{program}"))
    };
    let hook = line("ActiveSupport.run_load_hooks(:hookkit, Hookkit)");
    assert!(line("RoundhouseAutoload.require_gem_file(\"models/hookkit/configuration/dequeue_decorator\")") < hook, "{program}");
    let declarations = program
        .lines()
        .position(|line| {
            let line = line.trim_start();
            (line.starts_with("RoundhouseAutoload.require_declaration(") || line.starts_with("require RoundhouseAutoload.path("))
                && line.ends_with("\"models/hookkit/configuration\")")
        })
        .unwrap_or_else(|| panic!("the program loads no declarations of Hookkit::Configuration:\n{program}"));
    assert!(declarations < hook, "{program}");
    assert_eq!(program.matches("run_load_hooks(:hookkit,").count(), 1, "{program}");

    // Neither hook runs from a declarations file, nor twice.
    let mut fired = Vec::new();
    let mut stack = vec![out.join("app")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                for hook in ["run_load_hooks(:hookkit,", "run_load_hooks(:hookkit_loaded,"] {
                    for _ in text.matches(hook) {
                        fired.push((hook, path.strip_prefix(&*out).unwrap().display().to_string()));
                    }
                }
            }
        }
    }
    fired.sort();
    assert_eq!(
        fired,
        [
            ("run_load_hooks(:hookkit,", format!("app/models/roundhouse_files/{GEM}/hookkit/configuration.rb")),
            ("run_load_hooks(:hookkit_loaded,", format!("app/models/roundhouse_files/{GEM}/hookkit.rb")),
        ]
    );
}

/// The emitted boot. It loads the `pg` gem (it never connects): run with
/// `--ignored` and a `GEM_PATH` that has it (plus `LD_PRELOAD` when its
/// extension needs one). The native control above needs Active Support
/// from the same `GEM_PATH`.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn the_emitted_boot_configures_with_the_required_decorator() {
    let run = app(false).run_ruby(SCRIPT);
    assert!(run.success, "{}\n=== stdout ===\n{}\n=== stderr ===\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}

#[test]
#[ignore = "requires ruby with the pg gem"]
fn the_emitted_eager_boot_configures_with_the_required_decorator() {
    let run = app(true).run_ruby(SCRIPT);
    assert!(run.success, "{}\n=== stdout ===\n{}\n=== stderr ===\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}
