//! `config/environments/<env>.rb` as the program the emitted PostgreSQL
//! boot runs when `Rails.env` selects it (`ingest::environments`,
//! `project::environment_program_block`).
//!
//! Rails requires `config/environments/#{Rails.env}.rb` as the first of
//! Application#initialize!'s initializers (railties 8.2.0.alpha
//! engine.rb:584 `load_environment_config`): after config/application.rb,
//! before the railties' and the app's initializers. Its
//! `Rails.application.configure` block writes config (an app key replaces
//! application.rb's value; a framework key the runtime applies is applied;
//! an inert one is consumed; any other is refused by name), and the rest of
//! the file is app code, carried like an initializer's statements.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const APPLICATION: &str = r#"require_relative "boot"

module Probe
  class Application < Rails::Application
    config.x.greeting = "from application"
  end
end
"#;

const TEST_ENV: &str = r#"require "settings"
require "no_such_feature"

Rails.application.configure do
  config.eager_load = false
  config.cache_classes = true
  config.action_mailer.delivery_method = :test
  config.active_record.maintain_test_schema = false

  host = "test.example"
  config.x.greeting = "from test"
  config.app_host = host

  if ENV["NEVER_SET_IN_THIS_TEST"]
    config.cache_classes = false
  end

  config.to_prepare do
    Settings.uri = "https://#{Settings.host}"
  end
end

Settings.host = "test.example"
Settings.internal_uri = Settings.host
"#;

const DEVELOPMENT_ENV: &str = r#"require "settings"

Rails.application.configure do
  config.x.greeting = "from development"
end

Settings.host = "dev.example"
"#;

const SETTINGS: &str = r#"module Settings
  class << self
    attr_accessor :host, :uri, :internal_uri
  end
end
"#;

const REPORT: &str = r#"class Report
  def self.line
    [Settings.host, Settings.uri, Settings.internal_uri, Rails.application.config.x.greeting].inspect
  end

  def self.app_host
    Rails.application.config.app_host
  end
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_environment_programs\n")
        .write("config/application.rb", APPLICATION)
        .write("config/environments/test.rb", TEST_ENV)
        .write("config/environments/development.rb", DEVELOPMENT_ENV)
        .write("lib/settings.rb", SETTINGS)
        .write("lib/report.rb", REPORT)
}

/// The test program carries the app code and the app-key and applied
/// writes, consumes the inert and `eager_load` writes, and refuses the
/// framework write and the require nothing answers, each by name, at its
/// line. Boot selects by `Rails.env`, before the railties' initializers.
#[test]
fn environment_files_become_programs_selected_by_rails_env() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    let models = std::fs::read_to_string(dir.join("app/models.rb")).expect("app/models.rb");
    let case = models.find("case Rails.env.to_s\nwhen \"development\"").unwrap_or_else(|| panic!("{models}"));
    let test = case + models[case..].find("\nwhen \"test\"\n").unwrap_or_else(|| panic!("{models}"));
    let run = models
        .find("  RoundhouseEnvironmentTest.run_environment\n")
        .unwrap_or_else(|| panic!("{models}"));
    let railties = models
        .find("Rails::Railtie.run_initializers_of_railties(Rails.application, :boot)")
        .unwrap_or_else(|| panic!("{models}"));
    assert!(case < test && test < run && run < railties, "{models}");
    assert!(
        models.contains("  # config/environments/test.rb: 9 carried, 2 consumed, 2 not carried\n"),
        "{models}"
    );
    assert!(models.contains("but config/environments/#{Rails.env}.rb does not exist.\""), "{models}");
    let named: Vec<&str> = models.lines().filter(|l| l.starts_with("  warn \"roundhouse: error[environment_statement_not_carried]")).collect();
    assert_eq!(named.len(), 2, "{models}");
    assert!(
        named[0].contains("config/environments/test.rb:2 require \\\"no_such_feature\\\": `require \\\"no_such_feature\\\"`: not a runtime port"),
        "{}",
        named[0]
    );
    assert!(
        named[1].contains(
            "config/environments/test.rb:7 config.action_mailer.delivery_method = :test: `config.action_mailer.delivery_method` is framework configuration"
        ),
        "{}",
        named[1]
    );
    // The same two, as `check` reports them.
    let gaps: Vec<&String> = errors.iter().filter(|e| e.contains("initializer_unsupported") || e.contains("is framework configuration") || e.contains("no_such_feature")).collect();
    assert_eq!(gaps.len(), 2, "{errors:#?}");
    let program = std::fs::read_to_string(dir.join("app/models/roundhouse_environment_test.rb")).expect("holder");
    assert!(program.contains("Rails.application.x_greeting = \"from test\""), "{program}");
    assert!(program.contains("Rails.application.app_host = host"), "{program}");
    assert!(program.contains("Rails.application.active_record_maintain_test_schema = false"), "{program}");
    assert!(!program.contains("delivery_method"), "{program}");
    assert!(!program.contains("cache_classes"), "{program}");
    assert!(!program.contains("eager_load"), "{program}");
    let helper = std::fs::read_to_string(dir.join("test/test_helper.rb")).expect("test_helper.rb");
    let env = helper.find("ENV[\"RAILS_ENV\"] ||= \"test\"\nrequire_relative \"../boot\"").unwrap_or_else(|| panic!("{helper}"));
    assert!(env < helper.find("DatabaseConfiguration.configure!").unwrap(), "{helper}");
}

/// Boot runs test.rb under `RAILS_ENV=test` and development.rb under
/// `RAILS_ENV=development`, never both: app code sets the module
/// attributes the app reads, the app key replaces application.rb's value,
/// the applied framework key reaches its reader, and `to_prepare` runs
/// in the Finisher. An environment with no file raises, as Rails 8.2
/// does. Boot loads the `pg` gem (it never connects): run with
/// `--ignored`, `GEM_PATH` and the jemalloc `LD_PRELOAD` that has it.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn boot_runs_the_selected_environment_program() {
    let (dir, _) = app().emit(BuildTarget::Ruby);
    let run = |env: &str| {
        let out = std::process::Command::new("ruby")
            .args([
                "-e",
                "require \"./boot\"\nputs Report.line\nputs Rails.application.active_record_maintain_test_schema.inspect\n\
                 begin\n  puts Report.app_host\nrescue NoMethodError => e\n  puts \"NoMethodError: #{e.message}\"\nend\n",
            ])
            .current_dir(&*dir)
            .env("RAILS_ENV", env)
            .env_remove("DATABASE_URL")
            .env_remove("ROUNDHOUSE_DATABASE_URL")
            .output()
            .expect("ruby");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (ok, stdout, stderr) = run("test");
    assert!(ok, "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "[\"test.example\", \"https://test.example\", \"test.example\", \"from test\"]",
            "false",
            "test.example",
        ],
        "stderr={stderr}"
    );
    let named: Vec<&str> = stderr.lines().filter(|l| l.contains("error[environment_statement_not_carried]")).collect();
    assert_eq!(named.len(), 2, "{stderr}");
    assert!(!stderr.contains("initializer_statement_not_run"), "{stderr}");

    let (ok, stdout, stderr) = run("development");
    assert!(ok, "stdout={stdout}\nstderr={stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "[\"dev.example\", nil, nil, \"from development\"]", "stderr={stderr}");
    assert_eq!(lines[1], "true", "stderr={stderr}");
    assert!(lines[2].starts_with("NoMethodError: undefined method 'app_host'"), "{stdout}");
    assert!(!stderr.contains("environment_statement_not_carried"), "{stderr}");

    let (ok, _, stderr) = run("staging");
    assert!(!ok, "{stderr}");
    assert!(
        stderr.contains("Rails environment has been set to staging but config/environments/staging.rb does not exist."),
        "{stderr}"
    );
}
