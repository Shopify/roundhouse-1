//! An `ActiveSupport::Concern`'s `included do` statements that no
//! lowering claims run in each includer's class body at include time, in
//! source order, as `class_eval` runs the block there.
//!
//! Core's `MutationGuard` is the shape: `class_attribute :gate`, then
//! `self.gate = Gate.new` with `Gate` the concern's own nested class.
//! Before, only the block's defs reached the includer; its statements were
//! dropped, so `Store.guard_gate` was undefined and the concern's class
//! method calling it raised NameError. Each includer gets its own object
//! (the block runs once per include), the bare constant resolves in the
//! concern's lexical scope, and a statement the includer writes after
//! `include` runs after the carried ones.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r#"module Guarded
  extend ActiveSupport::Concern

  class Gate
    def initialize
      @disabled = false
    end

    attr_accessor :disabled
  end

  included do
    class_attribute :guard_gate, instance_accessor: false, instance_predicate: false
    self.guard_gate = Gate.new
    class_attribute :trail, default: [:declared]
    self.trail += [:included]
    self.trail += [name]
  end

  class_methods do
    def guarded?
      !guard_gate.disabled
    end
  end

  def guarded?
    self.class.guarded?
  end
end
"#;

const STORE: &str = r#"class Store < ApplicationRecord
  include Guarded
  self.trail += [:after]
end
"#;

const DEPOT: &str = r#"class Depot < ApplicationRecord
  include Guarded
end
"#;

const SCRIPT: &str = r#"p [Store.guarded?, Store.guard_gate.class.name, Store.guard_gate.equal?(Depot.guard_gate)]
p [Store.trail, Depot.trail]
Store.guard_gate.disabled = true
p [Store.guarded?, Depot.guarded?, Store.method_defined?(:guard_gate), Store.public_method_defined?(:guarded?)]
"#;

#[test]
fn included_block_statements_run_in_each_includer_after_emission() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_record\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n{CONCERN}\n{STORE}\n{DEPOT}\n{SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected: Vec<String> = String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect();
    for probe in [
        r#"[true, "Guarded::Gate", false]"#,
        r#"[[:declared, :included, "Store", :after], [:declared, :included, "Depot"]]"#,
        "[false, true, false, true]",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }
    let run = emit_and_run::real_blog()
        // A concrete model reads its table: ActiveRecord raises on the first
        // query without one, and check refuses it (model_table_not_in_schema).
        // The control never queries, so an empty table changes nothing.
        .edit("db/schema.rb", "create_table \"articles\", force: :cascade do |t|", "create_table \"stores\", force: :cascade do |t|\n  end\n\n  create_table \"depots\", force: :cascade do |t|\n  end\n\n  create_table \"articles\", force: :cascade do |t|")
        .write("app/models/concerns/guarded.rb", CONCERN)
        .write("app/models/store.rb", STORE)
        .write("app/models/depot.rb", DEPOT)
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from ActiveRecord line for line");
}
