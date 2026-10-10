//! A file whose first load raised is loaded again by the next require,
//! as Ruby does, and reruns what it already ran; Core's StrictWarning
//! raises that reload's "already initialized constant". Core's
//! line_item_test.rb requires order_node_access_enforcement_test_helper
//! and graphql_test_helper, which each require
//! graph_api_test_schema_helper.rb: a class body it loads raised, the
//! test file's file statement went on, and the next statement's reload
//! raised `already initialized constant
//! GraphApiTestSchemaHelper::BLOG_CONNECTION_EDGE_TYPE_VISIBILITY_CHANGES`,
//! which hid the error that stopped the first load (94 GraphQL ids). The
//! reload's redefinition is answered with the first load's error, named.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

// strict_warnings.rb: a redefinition warning raises.
const STRICT: &str = "module Strict\n  Offense = Class.new(StandardError)\n\n  module Raising\n    def warn(message, *)\n      raise Offense, message if message.include?(\"already initialized constant\")\n      super\n    end\n  end\nend\n\nWarning.singleton_class.prepend(Strict::Raising)\n";

// core_api_versioning/test_helper.rb.
const VERSIONING: &str = "module Versioning\n  def api_versioning_test_setup\n    @versions = []\n  end\n\n  def reset_api_versioning_test_setup\n    @versions = nil\n  end\nend\n";

// graph_api_test_schema_helper.rb: a constant, then a nested class whose
// body raises, then a statement after it.
const SCHEMA_HELPER: &str = "require \"support/helpers/versioning\"\n\nmodule SchemaHelper\n  extend Versioning\n\n  api_versioning_test_setup\n\n  BLOG_CHANGES = { initial: :hidden }.freeze\n\n  class Blog\n    LIMIT = Integer(ENV.fetch(\"RH_NEVER_SET\", \"unset\"))\n  end\n\n  reset_api_versioning_test_setup\nend\n";

const TEST: &str = "require \"test_helper\"\nrequire \"support/helpers/strict\"\nrequire \"support/helpers/first_helper\"\nrequire \"support/helpers/second_helper\"\n\nclass LineItemTest < ActiveSupport::TestCase\n  test \"runs\" do\n    assert true\n  end\nend\n";

#[test]
fn a_reload_after_a_failed_load_raises_the_first_loads_error_not_a_redefinition() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        // Lazy, as Core's local test boot is.
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n")
        .write("test/support/helpers/strict.rb", STRICT)
        .write("test/support/helpers/versioning.rb", VERSIONING)
        .write("test/support/helpers/schema_helper.rb", SCHEMA_HELPER)
        // order_node_access_enforcement_test_helper.rb and graphql_test_helper.rb.
        .write("test/support/helpers/first_helper.rb", "require \"support/helpers/schema_helper\"\n\nmodule FirstHelper\nend\n")
        .write("test/support/helpers/second_helper.rb", "require \"support/helpers/schema_helper\"\n\nmodule SecondHelper\nend\n")
        .write("test/models/line_item_test.rb", TEST)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).unwrap()).unwrap();
    let file = index["sources"][0]["classes"][0]["file"].as_str().unwrap().to_string();
    let out = emit_and_run::ruby()
        .args(["-Itest", "-e", "load ARGV.fetch(0)", &file])
        .current_dir(&emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", emitted.join("results.jsonl"))
        .env("BLOG_DB", ":memory:")
        .env("RAILS_ENV", "test")
        .output()
        .expect("emitted CRuby");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // Both statements name the error that stopped the first load.
    for line in [3, 4] {
        assert!(
            stderr.contains(&format!(
                "file-level statement at test/models/line_item_test.rb:{line} did not run: ArgumentError: invalid value for Integer(): \"unset\""
            )),
            "{stderr}"
        );
    }
    assert!(!stderr.contains("Strict::Offense:"), "{stderr}");
    assert!(
        stderr.contains("error[reload_after_failed_load]")
            && stderr.contains("schema_helper.rb loaded again after its first load raised ArgumentError: invalid value for Integer()")
            && stderr.contains("already initialized constant SchemaHelper::BLOG_CHANGES; raising the first load's error"),
        "{stderr}"
    );
}
