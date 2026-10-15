//! An engine initializer's `on_load` hook that `prepend`s a Concern onto
//! the loaded class (Core's BillingComponent::Engine:
//! `klass.prepend(BillingApiMockConnection)`).
//!
//! ActiveSupport::Concern's `prepend_features` prepends the Concern's
//! `ClassMethods` to the class's singleton class. The tree carries no
//! Concern, so the carrier is written after the statement
//! (`program_concern_extends`); without it `BillingApi::Connection.
//! mock_request_engine` was undefined in every test teardown that
//! called it.
//!
//! CRuby with the real Concern is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const ENGINE: &str = r#"require "rails"

module Foo
  class Engine < Rails::Engine
    initializer "foo.connection" do
      Rails.autoloaders.main.on_load("Connection") do |klass, _abspath|
        klass.prepend(MockConnection)
      end
    end
  end
end
"#;

const CONNECTION: &str = r#"class Connection
  def self.use_application?
    false
  end

  def http
    "real"
  end
end
"#;

const MOCK: &str = r#"module MockConnection
  extend ActiveSupport::Concern

  class_methods do
    def mock_engine
      @mock_engine ||= "engine"
    end
  end

  def http
    return super if self.class.use_application?

    "mock #{self.class.mock_engine}"
  end
end
"#;

#[test]
fn a_prepended_concerns_class_methods_reach_the_class_as_in_cruby() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_engine_on_load_prepend\n")
        .write("lib/foo/engine.rb", ENGINE)
        .write("app/models/connection.rb", CONNECTION)
        .write("app/models/mock_connection.rb", MOCK)
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .emit(BuildTarget::Ruby);
    let engine = std::fs::read_to_string(dir.join("app/models/foo/engine.rb")).expect("the engine is carried");
    assert!(engine.contains("klass.singleton_class.prepend ::MockConnection::ClassMethods"), "{engine}");

    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args([
            "-e",
            "require_relative 'runtime/active_support_lazy_load_hooks'\n\
             $LOAD_PATH.unshift(File.expand_path('runtime/features'))\n\
             require_relative 'runtime/module_delegate'\n\
             require_relative 'runtime/active_support_inflector'\n\
             require_relative 'runtime/rails'\n\
             Rails.env_name = 'test'\n\
             require_relative 'runtime/rails_railtie'\n\
             require_relative 'runtime/rails_engine'\n\
             require_relative 'runtime/rails_autoloaders'\n\
             require_relative 'runtime/active_support_deprecation'\n\
             require_relative 'app/models'\n\
             Rails::Railtie.run_initializers_of_railties(Rails.application, :finish)\n\
             puts Connection.new.http\n\
             puts Connection.mock_engine\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout, "mock engine\nengine\n", "{stderr}");
}
