//! A schema class a test file declares beside its test class, with a class
//! nested in it and a body call below that class:
//!
//! ```ruby
//! class TestSchema < ::GraphApi::Schema
//!   class Query < ::GraphApi::ObjectType ... end
//!   query(Query)
//! end
//! ```
//!
//! The emit wrote the schema's body calls first and the nested class in a
//! later reopening, so `query(Query)` read a bare `Query` that did not yet
//! exist (Core's `CartPlatformInputBuilderTestSchema`: "undefined method
//! 'graphql_name' for class Query", 4 unit-1000 ids).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
const SCHEMA_BASE: &str = "class ProbeSchema\n  def self.query(klass = nil)\n    @query = klass if klass\n    @query\n  end\nend\nclass ProbeObjectType\n  def self.field(name)\n    (@fields ||= []) << name\n  end\n  def self.fields\n    @fields\n  end\nend\n";
const TEST: &str = "require \"test_helper\"\n\nmodule Probe\n  class TestSchema < ::ProbeSchema\n    class Query < ::ProbeObjectType\n      field :a\n      def a\n        1\n      end\n    end\n\n    query(Query)\n  end\n\n  class ProbeTest < ActiveSupport::TestCase\n    test \"x\" do\n      assert_equal [:a], TestSchema.query.fields\n    end\n  end\nend\n";
#[test]
fn the_nested_class_precedes_the_body_call_that_names_it() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/probe_schema.rb", SCHEMA_BASE)
        .write("test/probe_test.rb", TEST)
        .run_test("test/models/probe_probe_test.rb");
    run.assert_passes();
    assert!(run.stdout.contains("1 tests passed"), "{}", run.stdout);
    let test = std::fs::read_to_string(run.emitted.join("test/models/probe_probe_test.rb")).unwrap();
    let nested = test.find("class Query < ::ProbeObjectType").expect("the nested class is written");
    let call = test.find("query(Query)").expect("the body call is written");
    assert!(nested < call, "the nested class precedes the call that names it:\n{test}");
}
