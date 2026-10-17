//! `config/environments/test.rb` registers Core's mock sorting-hat router on
//! every controller through a load hook that names a library module:
//!
//! ```ruby
//! ActiveSupport.on_load(:action_controller) do
//!   require "podding/sorting_hat/router/test_controller_patch"
//!   prepend Podding::SortingHat::Router::TestControllerPatch
//!   include Podding::SortingHat::Router::Helpers
//! end
//! ```
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const TEST_ENV: &str = r#"Rails.application.configure do
  config.eager_load = false

  ActiveSupport.on_load(:action_controller) do
    require "podding/router/test_controller_patch"

    #: self as (singleton(ActionController::Base) | singleton(ActionController::API))
    prepend Podding::Router::TestControllerPatch
    include Podding::Router::Helpers
  end
end
"#;

const PATCH: &str = r#"require "podding/router/helpers"

module Podding
  class Router
    module TestControllerPatch
      def process(*, &block)
        return super if skip_router?
        super
      end
    end
  end
end
"#;

const HELPERS: &str = r#"module Podding
  class Router
    module Helpers
      def skip_router?
        false
      end
    end
  end
end
"#;

const ROUTER: &str = r#"module Podding
  class Router
    def self.skip?
      false
    end
  end
end
"#;

#[test]
fn an_environment_load_hook_prepends_a_library_module() {
    let (dir, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_env_hook\n")
        .write("config/environments/test.rb", TEST_ENV)
        .write("lib/podding/router.rb", ROUTER)
        .write("lib/podding/router/helpers.rb", HELPERS)
        .write("lib/podding/router/test_controller_patch.rb", PATCH)
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .emit(BuildTarget::Ruby);
    let program = std::fs::read_to_string(dir.join("app/models/roundhouse_environment_test.rb")).unwrap();
    assert!(!program.contains("constant not supported"), "{program}");
    assert!(program.contains("prepend Podding::Router::TestControllerPatch"), "{program}");
    assert!(program.contains("include Podding::Router::Helpers"), "{program}");
    assert!(!errors.iter().any(|e| e.contains("constant not supported")), "{errors:#?}");
}
