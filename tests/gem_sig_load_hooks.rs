//! A gem source configured from an app initializer's `on_load`, as
//! hedwig 1.68.0 is from Core's background_queue.rb (unit-1000 r7c: 806 of
//! 935 setups raised `Hedwig::NotConfiguredError`).
//!
//! - Nothing names `Hedwig::Configuration` at boot but the `sig` of
//!   `Hedwig.configuration`. sorbet-runtime runs that block on the first
//!   call, which autoloads configuration.rb, whose `run_load_hooks(:hedwig)`
//!   runs the hook that configures (`ingest::sig_autoloads`).
//! - The initializer's `require "hedwig/process"` names a gem file that no
//!   autoload reaches; it stays a require, which loads the compiled program,
//!   so `Hedwig::Process` resolves.
//! - A hook registered before the load runs from it; one registered after
//!   runs at once (`ActiveSupport.on_load` replays `@loaded`).
//!
//! The fixture has the same shape: a gem source `sigkit`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SIGKIT: &str = r#"require "sorbet-runtime"

module Sigkit
  autoload :Configuration, "sigkit/configuration"

  class NotConfiguredError < StandardError; end

  class << self
    extend T::Sig

    sig { void }
    def configure
      _ = Configuration
      @configuration ||= Configuration.new
      yield @configuration
    end

    sig { returns(Sigkit::Configuration) }
    def configuration
      @configuration || Kernel.raise(NotConfiguredError)
    end

    def hooks
      @hooks ||= []
    end
  end
end
"#;
const CONFIGURATION: &str = r#"module Sigkit
  class Configuration
    attr_accessor :queues
  end
end

ActiveSupport.run_load_hooks(:sigkit, Sigkit)
"#;
const PROCESS: &str = r#"module Sigkit
  class Process
    def self.registered
      @registered ||= []
    end

    def self.register(name)
      registered << name
    end
  end
end
"#;

const BEFORE: &str = r#"require "sigkit/process"

ActiveSupport.on_load(:sigkit) do
  Sigkit.hooks << :before
  Sigkit.configure { |config| config.queues = %w[default low] }
end

Sigkit::Process.register(:core)
"#;
/// A class body that reads the configuration at load, as Core's
/// background_queue/job.rb does (`Hedwig.configuration.queues`).
const QUEUES: &str = "class QueueNames\n  ALL = Sigkit.configuration.queues.map(&:to_sym).freeze\nend\n";
const AFTER: &str = "_ = QueueNames::ALL\n\nActiveSupport.on_load(:sigkit) do\n  Sigkit.hooks << :after\nend\n";

const SCRIPT: &str = "p Sigkit.hooks\np QueueNames::ALL\np Sigkit::Process.registered\n";
const EXPECTED: &str = "[:before, :after]\n[:default, :low]\n[:core]\n";

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    sigkit (1.0.0)\n\nDEPENDENCIES\n  sigkit\n";
const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n\ndevelopment:\n  <<: *default\n  database: sig_probe\n\ntest:\n  <<: *default\n  database: sig_probe\n";

const FILES: [(&str, &str); 3] =
    [("sigkit.rb", SIGKIT), ("sigkit/configuration.rb", CONFIGURATION), ("sigkit/process.rb", PROCESS)];

const GEM: &str = "vendor/gems/sigkit-1.0.0/lib";

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\ngem_sources:\n  sigkit: vendor/gems/sigkit-1.0.0\n")
        .write("config/database.yml", DATABASE_YML)
        .write("Gemfile.lock", LOCK)
        .write("Gemfile", "source \"https://rubygems.org\"\n\ngem \"sigkit\"\n")
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("config/initializers/sigkit.rb", BEFORE)
        .write("config/initializers/zz_queues.rb", AFTER)
        .write("app/lib/queue_names.rb", QUEUES)
        .write("config/environments/development.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n");
    for (path, text) in FILES {
        app = app.write(&format!("{GEM}/{path}"), text);
    }
    app
}

/// CRuby with sorbet-runtime and Active Support from `GEM_PATH`.
#[test]
#[ignore = "requires ruby with sorbet-runtime and activesupport"]
fn the_native_control_configures_from_the_sig() {
    let root = std::env::temp_dir().join(format!("roundhouse-gem-sig-load-hooks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in FILES.iter().copied().chain([("queue_names.rb", QUEUES)]) {
        let file = root.join("lib").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let after = AFTER.replace("_ = QueueNames::ALL", "require \"queue_names\"\n_ = QueueNames::ALL");
    let script = format!(
        "require \"active_support\"\nrequire \"active_support/lazy_load_hooks\"\nrequire \"sigkit\"\n{BEFORE}{after}{SCRIPT}"
    );
    let native = emit_and_run::ruby().arg("-I").arg(root.join("lib")).args(["-e", &script]).output().expect("CRuby control");
    let _ = std::fs::remove_dir_all(&root);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);
}

#[test]
fn the_sig_reads_the_constant_it_names_and_the_require_stays() {
    let (out, _errors) = app().emit(roundhouse::project::BuildTarget::Ruby);
    let mut configuration = None;
    let mut stack = vec![out.join("app")];
    let mut initializer = None;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(at) = text.find("def configuration\n").or_else(|| text.find("def self.configuration\n")) {
                    configuration = Some(text[at..].lines().take(3).collect::<Vec<_>>().join("\n"));
                }
                if text.contains("Process.register(:core)") {
                    initializer = Some(text);
                }
            }
        }
    }
    let configuration = configuration.expect("an emitted Sigkit.configuration");
    assert!(configuration.contains("_ = Sigkit::Configuration"), "{configuration}");
    let initializer = initializer.expect("the emitted initializer");
    assert!(
        initializer.contains("require \"sigkit/process\"")
            || initializer.contains("RoundhouseAutoload.require_source(\"vendor/gems/sigkit-1.0.0/lib/sigkit/process.rb\")"),
        "{initializer}"
    );
}

/// The emitted boot. It loads the `pg` gem (it never connects).
#[test]
#[ignore = "requires ruby with the pg gem"]
fn the_emitted_boot_configures_from_the_sig_and_resolves_the_required_class() {
    let run = app().run_ruby(SCRIPT);
    assert!(run.success, "{}\n=== stdout ===\n{}\n=== stderr ===\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}
