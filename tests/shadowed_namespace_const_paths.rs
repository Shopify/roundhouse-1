//! A resolved constant reference inside a namespace whose sibling
//! shares the top-level name keeps binding the constant it resolved to.
//!
//! Ruby looks the first segment of `A::B::C` up through the lexical
//! scopes before `Object` (Module.nesting), so inside `module Apps;
//! module Management; module Api` a bare `Apps::Management::Api::Ctx`
//! finds the sibling `Apps::Management::Apps` first and raises
//! `uninitialized constant Apps::Management::Apps::Management`. The
//! analysis writes a reference it resolved in full (`ContextState` ->
//! `Apps::Management::Api::ContextState`); emitted rooted (`::Apps::...`)
//! it reads what the source meant.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_resolved_reference_is_rooted_when_a_sibling_namespace_shadows_its_head() {
    // Native oracle (ruby -Ilib, same files): [:ctx, :create].
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write(
            "lib/apps/management/apps/create.rb",
            "module Apps\n  module Management\n    module Apps\n      class Create\n        def self.kind = :create\n      end\n    end\n  end\nend\n",
        )
        .write(
            "lib/apps/management/api.rb",
            "require \"apps/management/apps/create\"\n\nmodule Apps\n  module Management\n    module Api\n      module Ctx\n        def self.kind = :ctx\n      end\n\n      def self.state = Ctx.kind\n\n      def self.create = Apps::Create.kind\n    end\n  end\nend\n",
        )
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/api_test.rb", r#"require "test_helper"
require "apps/management/api"
class ApiTest < ActiveSupport::TestCase
  test "references resolved under a shadowing sibling namespace bind the top-level path" do
    assert_equal [:ctx, :create], [Apps::Management::Api.state, Apps::Management::Api.create]
  end
end
"#)
        .run_test("test/models/api_test.rb");
    run.assert_passes();
}
