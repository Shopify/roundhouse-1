//! A file that declares a nested module and then runs statements in its
//! namespace never requires itself.
//!
//! Zeitwerk loads `app/models/atlas/zones/registry.rb` for `Atlas::Zones::Registry`.
//! The file declares `Registry`, then runs namespace statements below it
//! that read a sibling file's module (`ZoneChecks`). The statements are
//! carried after the declaration inside the same file, so a require of
//! that file from them is a `require` of a file still loading: Ruby prints
//! "loading in progress, circular require considered harmful", which a
//! strict-warnings boot raises on.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn namespace_statements_below_a_declaration_do_not_require_the_declarations_file() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write(
            "app/models/atlas/zones/zone_checks.rb",
            "module Atlas\n  module Zones\n    module ZoneChecks\n      def self.verify = :checked\n    end\n  end\nend\n",
        )
        .write(
            "app/models/atlas/zones/registry.rb",
            "module Atlas\n  module Zones\n    module Registry\n      class Error < StandardError; end\n\n      class << self\n        def all\n          @all ||= []\n        end\n      end\n    end\n\n    ActiveSupport.run_load_hooks(:atlas_zones_registry, Atlas)\n\n    if true\n      ZoneChecks.verify\n    end\n  end\nend\n",
        )
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/registry_test.rb", r#"require "test_helper"
class RegistryTest < ActiveSupport::TestCase
  test "the namespace statements run as the declaration's file loads" do
    assert_equal [], Atlas::Zones::Registry.all
  end
end
"#)
        .run_test("test/models/registry_test.rb");
    run.assert_passes();
    let registry = std::fs::read_to_string(run.emitted.join("app/models/atlas/zones/registry.rb")).expect("emitted registry.rb");
    assert!(!registry.contains("require_relative \"registry\""), "registry.rb requires itself:\n{registry}");
}
