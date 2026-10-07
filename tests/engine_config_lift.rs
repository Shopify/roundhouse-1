//! An in-repo engine's `config.<key> = …` is the application's config.
//!
//! Core's components set their config object in the engine class body
//! (`config.platform_essentials = Essentials::Config.for(Rails.env).
//! new(ENV.to_hash)`) and read it back anywhere as
//! `Rails.configuration.<key>`. The value keeps the engine's lexical
//! scope (`Config` is `Widgets::Config` there), and `Config.for` picks
//! the per-environment subclass that a base class's `inherited` hook
//! autoloads from `config/<env>.rb` beside the file that defined it, so
//! the emitted tree keeps that file layout.
//!
//! The oracle for the config object is native Ruby on the same source
//! (no Rails needed for it); the emitted app must answer the same
//! through `Rails.configuration`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const KIT_CONFIG: &str = r#"module Kit
  class Config
    class << self
      def inherited(config)
        return super unless config.superclass == Config

        if (class_name = config.name) &&
            (location = Object.const_source_location(class_name)) &&
            (parent_dirname = File.dirname(location[0]))
          config_dirname = File.join(parent_dirname, "config")
          config.autoload(:Development, File.join(config_dirname, "development.rb"))
          config.autoload(:Test, File.join(config_dirname, "test.rb"))
        end

        super
      end

      def for(env)
        case env
        when "development", "test"
          const_get(env.capitalize)
        else
          raise "Unknown config for the environment: #{env}"
        end
      end
    end

    def initialize(env)
    end
  end
end
"#;

const WIDGETS_CONFIG: &str = r#"require "kit/config"

module Widgets
  class Config < ::Kit::Config
    def initialize(env)
      super
      @env = env
    end

    def name_from_env
      @env.fetch("WIDGET_NAME", "none")
    end
  end
end
"#;

const WIDGETS_CONFIG_TEST: &str = r#"module Widgets
  class Config < ::Kit::Config
    class Test < Config
      def greeting
        "test greeting for #{name_from_env}"
      end
    end
  end
end
"#;

const WIDGETS_CONFIG_DEVELOPMENT: &str = r#"module Widgets
  class Config < ::Kit::Config
    class Development < Config
      def greeting
        "development greeting for #{name_from_env}"
      end
    end
  end
end
"#;

const ENGINE: &str = r#"require "widgets/config"

module Widgets
  class Engine < ::Rails::Engine
    config.widgets = widgets_config = Config.for(Rails.env).new(ENV.to_hash)
  end
end
"#;

const REPORT_LIB: &str = r#"module WidgetReport
  def self.line
    Rails.configuration.widgets.greeting
  end
end
"#;

const EXPECTED: &str = "test greeting for gadget\nWidgets::Config::Test\n";

const FILES: &[(&str, &str)] = &[
    ("gems/kit/lib/kit/config.rb", KIT_CONFIG),
    ("engines/widgets/lib/widgets/config.rb", WIDGETS_CONFIG),
    ("engines/widgets/lib/widgets/config/test.rb", WIDGETS_CONFIG_TEST),
    ("engines/widgets/lib/widgets/config/development.rb", WIDGETS_CONFIG_DEVELOPMENT),
    ("engines/widgets/lib/widgets/engine.rb", ENGINE),
    ("app/lib/widget_report.rb", REPORT_LIB),
];

const PATH_GEMS: &str = "PATH\n  remote: engines/widgets\n  specs:\n    widgets (0.1.0)\n\n\
PATH\n  remote: gems/kit\n  specs:\n    kit (0.1.0)\n\n";

#[test]
fn native_ruby_builds_the_environment_config() {
    let root = std::env::temp_dir().join(format!("roundhouse-engine-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in FILES {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, text).unwrap();
    }
    let output = std::process::Command::new("ruby")
        .arg("-I")
        .arg(root.join("gems/kit/lib"))
        .arg("-I")
        .arg(root.join("engines/widgets/lib"))
        .arg("-e")
        .arg("require \"widgets/config\"\nc = Widgets::Config.for(\"test\").new(ENV.to_hash)\nputs c.greeting\nputs c.class.name\n")
        .env("WIDGET_NAME", "gadget")
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&root);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_ruby_reads_the_engine_config_through_rails_configuration() {
    let lock = std::fs::read_to_string(roundhouse::fixtures::real_blog().join("Gemfile.lock")).unwrap();
    let mut fixture = emit_and_run::real_blog().write("Gemfile.lock", &format!("{PATH_GEMS}{lock}"));
    for (path, text) in FILES {
        fixture = fixture.write(path, text);
    }
    let (emitted, errors) = fixture.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(
        emitted.join("app/models/widgets/config/test.rb").is_file(),
        "Config::Test keeps its own file beside config.rb, where the inherited hook autoloads it"
    );
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require {main:?}\nMain.configure_default_adapter!\nputs WidgetReport.line\nputs Rails.configuration.widgets.class.name\n"
        ))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .env("WIDGET_NAME", "gadget")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

/// Core's per-environment configs are empty: `config/test.rb` is
/// `class Config; class Test < Config; end; end`. With no body there is
/// no span to tell the file apart from `config.rb`, so `Config::Test`
/// was spliced into `config.rb`; there `class Test` triggered the
/// autoload the `inherited` hook had just registered, for a
/// `config/test.rb` the tree did not have (LoadError at
/// checkouts_component/config.rb:8 and nine more components in the
/// release-10 boot survey). The file that declares it says where it was
/// written.
const WIDGETS_CONFIG_TEST_EMPTY: &str = r#"module Widgets
  class Config < ::Kit::Config
    class Test < Config; end
  end
end
"#;

const REPORT_LIB_EMPTY: &str = r#"module WidgetReport
  def self.line
    Rails.configuration.widgets.name_from_env
  end
end
"#;

const EXPECTED_EMPTY: &str = "gadget\nWidgets::Config::Test\n";

fn empty_test_files() -> Vec<(&'static str, &'static str)> {
    FILES
        .iter()
        .map(|&(path, text)| match path {
            "engines/widgets/lib/widgets/config/test.rb" => (path, WIDGETS_CONFIG_TEST_EMPTY),
            "app/lib/widget_report.rb" => (path, REPORT_LIB_EMPTY),
            _ => (path, text),
        })
        .collect()
}

#[test]
fn an_empty_environment_config_keeps_its_own_file() {
    let files = empty_test_files();
    // Native Ruby on the same source is the oracle.
    let root = std::env::temp_dir().join(format!("roundhouse-engine-config-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in &files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, text).unwrap();
    }
    let native = std::process::Command::new("ruby")
        .arg("-I")
        .arg(root.join("gems/kit/lib"))
        .arg("-I")
        .arg(root.join("engines/widgets/lib"))
        .arg("-e")
        .arg("require \"widgets/config\"\nc = Widgets::Config.for(\"test\").new(ENV.to_hash)\nputs c.name_from_env\nputs c.class.name\n")
        .env("WIDGET_NAME", "gadget")
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&root);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED_EMPTY);

    let lock = std::fs::read_to_string(roundhouse::fixtures::real_blog().join("Gemfile.lock")).unwrap();
    let mut fixture = emit_and_run::real_blog().write("Gemfile.lock", &format!("{PATH_GEMS}{lock}"));
    for (path, text) in &files {
        fixture = fixture.write(path, text);
    }
    let (emitted, errors) = fixture.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(
        emitted.join("app/models/widgets/config/test.rb").is_file(),
        "the empty Config::Test keeps its own file beside config.rb"
    );
    let config = std::fs::read_to_string(emitted.join("app/models/widgets/config.rb")).unwrap();
    assert!(!config.contains("class Test"), "config.rb does not define Test itself:\n{config}");
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require {main:?}\nMain.configure_default_adapter!\nputs WidgetReport.line\nputs Rails.configuration.widgets.class.name\n"
        ))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .env("WIDGET_NAME", "gadget")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED_EMPTY);
}
