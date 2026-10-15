//! Locals an `included do` block writes for the statements below it
//! (Core's Pagination::Admin::FulfillmentNotRequiredFilterable: `parent_query
//! = superclass`, then `override_filter_param(parent_definition, …)`) on a
//! plain includer. The block's calls were carried and its locals dropped, so
//! the call read `parent_definition` as a method and raised NameError when
//! the includer loaded.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn locals_of_an_included_block_are_written_where_the_carried_statements_read_them() {
    let run = emit_and_run::empty_app()
        .write(
            "app/models/base.rb",
            "class Base\n  def self.defs\n    @defs ||= []\n  end\n\n  def self.override_param(definition, values:)\n    defs << [definition, values]\n  end\nend\n",
        )
        .write("app/models/query.rb", "class Query < Base\n  defs << \"key\"\nend\n")
        .write(
            "app/models/filterable.rb",
            "module Filterable\n  extend ActiveSupport::Concern\n\n  included do\n    parent_query = superclass\n    parent_definition = parent_query.defs.first\n    override_param(parent_definition, values: [1, 2])\n  end\nend\n",
        )
        .write("app/models/list_query.rb", "class ListQuery < Query\n  include Filterable\nend\n")
        .run_ruby("p ListQuery.defs\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[[\"key\", [1, 2]]]\n", "{}", run.stderr);
}
