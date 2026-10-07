//! A nested class header's superclass and its body's mixins, written
//! with a leading `::` in the source, keep binding the top-level
//! constant in the emitted tree.
//!
//! Core writes `class ListingAttributeValue < ::GraphApi::ObjectType`
//! inside `module Channels; module GraphApi`. The ingest records the
//! absolute name without its `::`, and emitted bare, `GraphApi` bound
//! `Channels::GraphApi`: `uninitialized constant
//! Channels::GraphApi::ObjectType` at load (116 boot-survey rows over
//! Merchandising, BusinessCustomers, ProductMapping, ...). A mixin
//! written relative (`include Helpers`) still binds the scope's.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_shadowed_top_level_superclass_and_mixin_stay_top_level() {
    // Native oracle (ruby -Ilib, same files): [:top, :top_shared, :channels_helper].
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/graph_api/object_type.rb", "module GraphApi\n  class ObjectType\n    def self.kind = :top\n  end\nend\n")
        .write("lib/graph_api/shared.rb", "module GraphApi\n  module Shared\n    def shared = :top_shared\n  end\nend\n")
        .write("lib/helpers.rb", "module Helpers\n  def helper = :top_helper\nend\n")
        .write(
            "lib/channels/graph_api/helpers.rb",
            "module Channels\n  module GraphApi\n    module Helpers\n      def helper = :channels_helper\n    end\n  end\nend\n",
        )
        .write(
            "lib/channels/graph_api/admin/value.rb",
            "require \"graph_api/object_type\"\nrequire \"graph_api/shared\"\nrequire \"helpers\"\nrequire \"channels/graph_api/helpers\"\n\nmodule Channels\n  module GraphApi\n    module Admin\n      class Value < ::GraphApi::ObjectType\n        include ::GraphApi::Shared\n        include Helpers\n      end\n    end\n  end\nend\n",
        )
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("test/value_test.rb", r#"require "test_helper"
require "channels/graph_api/admin/value"
class ValueTest < ActiveSupport::TestCase
  test "the header and mixins bind what the source names" do
    value = Channels::GraphApi::Admin::Value
    assert_equal [:top, :top_shared, :channels_helper], [value.kind, value.new.shared, value.new.helper]
  end
end
"#)
        .run_test("test/models/value_test.rb");
    run.assert_passes();
}
