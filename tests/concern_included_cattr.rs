//! `cattr_accessor` inside a Concern's `included do` defines the class
//! attribute on each includer, as the same declaration in its class body
//! would.
//!
//! Core's concerns declare `included do cattr_accessor :x; self.x = [] end`
//! with a `class_methods` writer `def add(*a) = (self.x = x + a)`. The
//! concern walk carried a `class_attribute`'s accessors to the includer but
//! dropped the `cattr_*` expansion, so check reported `no known method x=`
//! on the includer (runs/probes/concern-cattr: one error per shape: a
//! block parameter, a statement before the declaration, the plain block).
//! Each block's `self.x = []` seed is a write through that cattr's
//! class-side writer, claimed like a `class_attribute` write; so is the
//! includer's own `self.dd_methods = [:d]` beside its own declaration.
//! The emitted Ruby must behave as ActiveSupport does.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PC_A: &str = r#"module PcA
  extend ActiveSupport::Concern
  included do |base|
    cattr_accessor :aa_methods
    self.aa_methods = []
  end
  class_methods do
    def add_a(*args) = (self.aa_methods = aa_methods + args)
  end
end
"#;

const PC_B: &str = r#"module PcB
  extend ActiveSupport::Concern
  included do
    raise("no") if name.nil?
    cattr_accessor :bb_methods
    self.bb_methods = []
  end
  class_methods do
    def add_b(*args) = (self.bb_methods = bb_methods + args)
  end
end
"#;

const PC_C: &str = r#"module PcC
  extend ActiveSupport::Concern
  included do
    cattr_accessor :cc_methods
    self.cc_methods = []
  end
  class_methods do
    def add_c(*args) = (self.cc_methods = cc_methods + args)
  end
end
"#;

const STORE: &str = r#"class Store < ApplicationRecord
  include PcA
  include PcB
  include PcC
  cattr_accessor :dd_methods
  self.dd_methods = [:d]
end
"#;

const SCRIPT: &str = r#"Store.add_a(:one)
Store.add_a(:two)
Store.add_b(:b, :bb)
Store.add_c(:c)
p [Store.aa_methods, Store.bb_methods, Store.cc_methods, Store.dd_methods]
p [Store.method_defined?(:aa_methods), Store.method_defined?(:cc_methods=), Store.respond_to?(:bb_methods=)]
"#;

#[test]
fn a_concern_s_cattr_accessor_is_the_includer_s_class_attribute() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_record\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n{PC_A}\n{PC_B}\n{PC_C}\n{STORE}\n{SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[[:one, :two], [:b, :bb], [:c], [:d]]\n[true, true, true]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::real_blog()
        // A concrete model reads its table: ActiveRecord raises on the first
        // query without one, and check refuses it (model_table_not_in_schema).
        .edit("db/schema.rb", "create_table \"articles\", force: :cascade do |t|", "create_table \"stores\", force: :cascade do |t|\n  end\n\n  create_table \"articles\", force: :cascade do |t|")
        .write("app/models/concerns/pc_a.rb", PC_A)
        .write("app/models/concerns/pc_b.rb", PC_B)
        .write("app/models/concerns/pc_c.rb", PC_C)
        .write("app/models/store.rb", STORE)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
