//! `as_json` on an Enumerable in a lambda in a class body (`property
//! :scopes, converts: ->(scopes) { scopes.as_json }`) on a library class.
//!
//! Core's Apps::Installations::EnsureInstalled converts a PrivacyRegistry::
//! ScopeSet (a Set) with `scopes.as_json`, and every request of
//! `GraphApi::Admin::*` that installs an app died with `undefined method
//! 'as_json' for an instance of Access::ScopeSet`. Active Support answers
//! it from Enumerable#as_json (`to_a.as_json(options)`). A class-body
//! lambda is not an analyzed body, so the send is not routed to
//! `ActiveSupport::JSON::Encoding.as_json`: the runtime's core_ext port
//! defines the method, and Hash, Range, Struct and IO (all Enumerable)
//! keep the answers Rails gives them.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BASE: &str = r#"class BusinessProcess
  def self.property(name, **opts)
    (@properties ||= {})[name] = opts
  end

  def self.properties
    @properties
  end
end
"#;

const INSTALLER: &str = r#"require "set"

class ScopeSet < Set
end

class Installer < BusinessProcess
  property :access_scope, converts: ->(scopes) { scopes.as_json }

  # A method body's send is routed to the JSON port, which the tree then carries.
  def self.routed(value)
    value.as_json
  end

  def self.convert(value)
    properties[:access_scope][:converts].call(value)
  end
end
"#;

#[test]
fn a_class_body_lambda_answers_enumerable_as_json() {
    emit_and_run::real_blog()
        .write("app/models/business_process.rb", BASE)
        .write("app/models/installer.rb", INSTALLER)
        .run_ruby(
            r#"scopes = ScopeSet.new(["read_products", "write_orders"])
raise "ScopeSet: #{Installer.convert(scopes).inspect}" unless Installer.convert(scopes) == ["read_products", "write_orders"]
raise "Set of symbols" unless Installer.convert(Set.new([:a])) == ["a"]
raise "Hash: #{Installer.convert({ a: 1, "b" => [:c] }).inspect}" unless Installer.convert({ a: 1, "b" => [:c] }) == { "a" => 1, "b" => ["c"] }
raise "Range" unless Installer.convert(1..3) == "1..3"
Pair = Struct.new(:left, :right)
raise "Struct: #{Installer.convert(Pair.new(1, :x)).inspect}" unless Installer.convert(Pair.new(1, :x)) == { "left" => 1, "right" => "x" }
raise "Array" unless Installer.convert([1, :y]) == [1, "y"]
raise "Enumerator" unless Installer.convert([3, 4].each) == [3, 4]
"#,
        )
        .assert_passes();
}

/// Core's CapabilityPayload#normalize round trip: a Hash holding value
/// objects is written through `to_json` and parsed back. The json gem
/// alone writes an object as its `to_s`, which Rails' encoder does not.
#[test]
fn to_json_encodes_value_objects_through_as_json() {
    emit_and_run::real_blog()
        .write("app/models/business_process.rb", BASE)
        .write("app/models/installer.rb", INSTALLER)
        .write(
            "app/models/flow.rb",
            "class Flow\n  def initialize(enabled)\n    @enabled = enabled\n    @label = \"<b>\"\n  end\nend\n",
        )
        .run_ruby(
            r#"flow = Flow.new(true)
text = { flow: flow, list: [flow, :x], at: 1 }.to_json
parsed = JSON.parse(text, symbolize_names: true)
plain = { enabled: true, label: "<b>" }
raise "Hash#to_json: #{text}" unless parsed == { flow: plain, list: [plain, "x"], at: 1 }
raise "html entities: #{text}" unless text.include?("\\u003cb\\u003e")
raise "Object#to_json: #{flow.to_json}" unless JSON.parse(flow.to_json, symbolize_names: true) == plain
raise "state passthrough" unless JSON.generate({ "a" => [1, nil] }) == '{"a":[1,null]}'
raise "pretty" unless JSON.pretty_generate({ "a" => 1 }) == "{\n  \"a\": 1\n}"
"#,
        )
        .assert_passes();
}
