//! A constant a test class reads from an included module (Core's
//! `AccessTestHelper::DEFAULT_WRITE_DEFINITIONS_ACCESS_SCOPE`) is
//! qualified at its module even when a sibling include (`ApiChangeTestHelper`
//! includes `Mocha::API` and an app module) leaves the class's ancestors
//! partly unresolved. Before, the source index lost the module's constant
//! and the 26 reads in Core's `DefinitionsWithDeleteAccessTest` were
//! "constant not supported".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ACCESS: &str = r##"module AccessTestHelper
  DEFAULT_WRITE_DEFINITIONS_ACCESS_SCOPE = "write_metaobject_definitions"

  def set_scopes(token, scopes)
    [token, scopes]
  end
end
"##;

const CHANGE: &str = r##"module ApiChangeTestHelper
  include Minitest::Assertions
  include Mocha::API
end
"##;

const TEST: &str = r##"require "test_helper"

module Outer
  module Inner
    class AccessReadTest < ActiveSupport::TestCase
      include AccessTestHelper
      include ApiChangeTestHelper

      test "reads the included module's constant" do
        assert_equal ["t", ["write_metaobject_definitions"]], set_scopes("t", [DEFAULT_WRITE_DEFINITIONS_ACCESS_SCOPE])
      end
    end
  end
end
"##;

#[test]
fn included_module_constant_resolves_beside_an_unresolved_include() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/support/access_test_helper.rb", ACCESS)
        .write("test/support/api_change_test_helper.rb", CHANGE)
        .write("test/models/access_read_test.rb", TEST)
        .emit(roundhouse::project::BuildTarget::Ruby);
    // The sibling's unresolved includes are its own diagnostics; the
    // included module's constant is not one of them.
    assert!(errors.iter().all(|e| !e.contains("DEFAULT_WRITE_DEFINITIONS_ACCESS_SCOPE")), "{errors:?}");
    let text = std::fs::read_to_string(emitted.join("test/models/outer_inner_access_read_test.rb")).expect("emitted test");
    assert!(
        text.contains("AccessTestHelper::DEFAULT_WRITE_DEFINITIONS_ACCESS_SCOPE"),
        "the constant is read at its module:\n{text}"
    );
    assert!(!text.contains("constant not supported"), "{text}");
}
