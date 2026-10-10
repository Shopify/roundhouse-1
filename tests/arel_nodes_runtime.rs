//! `Arel::Nodes` classes named outside Arel, as Core's pagination does:
//!
//! ```ruby
//! OPAQUE_CONDITIONS = [Arel::Nodes::BoundSqlLiteral, Arel::Nodes::Grouping].freeze
//! fields.map { it.is_a?(Arel::Nodes::Node) ? it.to_sql : it }
//! ```
//!
//! The runtime's Arel had no `Nodes`, so each read was "constant not
//! supported" and `Pagination::Paginator` raised at load. The runtime
//! (runtime/ruby/active_record/arel.rb) now declares activerecord's node
//! hierarchy; it composes SQL as strings and builds no node, so a string
//! fragment is no node, as in Rails.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const FIELDS: &str = r##"module SortFields
  OPAQUE_CONDITIONS = [Arel::Nodes::BoundSqlLiteral, Arel::Nodes::Grouping].freeze

  def self.resolve(fields)
    return fields unless fields

    if fields.is_a?(Array)
      fields.map { |it| it.is_a?(Arel::Nodes::Node) ? it.to_sql : it }
    elsif fields.is_a?(Arel::Nodes::Node)
      fields.to_sql
    else
      fields
    end
  end

  def self.opaque?(ast)
    OPAQUE_CONDITIONS.any? { |type| ast.is_a?(type) }
  end
end
"##;

#[test]
fn node_classes_resolve_and_strings_are_no_nodes() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("app/models/sort_fields.rb", FIELDS)
        .run_ruby(
            "p SortFields.resolve([\"title\", Arel::Nodes::Grouping.new(\"a = 1\")])\n\
             p SortFields.resolve(\"id\")\n\
             p SortFields.opaque?(Arel::Nodes::Grouping.new(Arel.sql(\"b\")))\n\
             p SortFields.opaque?(\"b\")\n\
             p Arel::Nodes::Grouping.ancestors.take_while { |c| c != Object }.map(&:name)\n",
        );
    let constants: Vec<_> = run.errors.iter().filter(|e| e.contains("constant not supported")).collect();
    assert!(constants.is_empty(), "{constants:#?}");
    run.assert_passes();
    assert_eq!(
        run.stdout.lines().collect::<Vec<_>>(),
        [
            r#"["title", "(a = 1)"]"#,
            r#""id""#,
            "true",
            "false",
            r#"["Arel::Nodes::Grouping", "Arel::Nodes::Unary", "Arel::Nodes::NodeExpression", "Arel::Nodes::Node"]"#,
        ],
        "{}",
        run.stderr
    );
}
