//! `result.is_a?(ActiveRecord::QueryMethods::WhereChain)` (Core's
//! Podding::Database::ScopeProxy#method_missing, run for every scope call
//! that answers a non-relation) was "constant not supported (all
//! targets)": the runtime's `where` answers the relation itself, so it had
//! no WhereChain class to name (e2e v2-r39, Sales::Subscriptions
//! DiscountIdentityTest).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PROXY: &str = r#"class ScopeProxy
  def initialize(scope)
    @scope = scope
  end

  def method_missing(method, *args, &block)
    result = @scope.send(method, *args, &block)
    if result.respond_to?(:where) || result.is_a?(ActiveRecord::QueryMethods::WhereChain)
      ScopeProxy.new(result)
    else
      result
    end
  end

  def respond_to_missing?(*) = true
end
"#;

const MODEL: &str = "class Widget < ApplicationRecord\nend\n";
const APPLICATION: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";
const SCHEMA: &str = "ActiveRecord::Schema[8.1].define(version: 2026_01_01_000000) do\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";

const SCRIPT: &str = r#"Widget.create!(name: "a")
proxy = ScopeProxy.new(Widget.all)
raise "count #{proxy.count.inspect}" unless proxy.count == 1
raise "where is not proxied" unless proxy.where(name: "a").is_a?(ScopeProxy)
raise "WhereChain is not a class" unless ActiveRecord::QueryMethods::WhereChain.is_a?(Class)
raise "a relation is not a chain" if Widget.all.is_a?(ActiveRecord::QueryMethods::WhereChain)
"#;

#[test]
fn where_chain_names_a_type_and_a_scope_proxy_passes_non_relations_through() {
    emit_and_run::empty_app()
        .write("app/models/application_record.rb", APPLICATION)
        .write("app/models/widget.rb", MODEL)
        .write("app/models/scope_proxy.rb", PROXY)
        .write("db/schema.rb", SCHEMA)
        .run_ruby(SCRIPT)
        .assert_passes();
}
