//! A class body calls a class-side method its app superclass defines.
//!
//! Core's `class TransactionInput < Component::Schema` then `attribute
//! :id, Integer`: `Component::Schema` has `class << self; def
//! attribute(...)`, so Ruby runs the call on the subclass through the
//! inherited method. The superclass is an app class, not a foreign base,
//! so the call was dropped as "not modelled", and every schema lacked
//! its attributes.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r#"module Component
  class Schema
    class << self
      #: (Class subclass) -> void
      def inherited(subclass)
        super
        subclass.instance_variable_set(:@fields, fields.dup)
      end

      #: () -> Hash[Symbol, untyped]
      def fields
        @fields ||= {} #: Hash[Symbol, untyped]?
      end

      #: (Symbol name, Class type, ?optional: bool) -> void
      def attribute(name, type, optional: false)
        fields[name] = [type, optional]
      end
    end
  end
end
"#;

const INPUT: &str = r#"module Billing
  class TransactionInput < Component::Schema
    attribute :id, Integer
    self.attribute :note, String, optional: true

    #: () -> Array[Symbol]
    def self.names
      fields.keys
    end
  end
end
"#;

const REFUND: &str = r#"module Billing
  class RefundInput < TransactionInput
    attribute :reason, String
  end
end
"#;

#[test]
fn an_app_superclass_s_class_macro_runs_in_the_subclass_body() {
    let script = "p Billing::TransactionInput.names, Billing::RefundInput.fields, Component::Schema.fields\n";
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{SCHEMA}\n{INPUT}\n{REFUND}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[:id, :note]\n{id: [Integer, false], note: [String, true], reason: [String, false]}\n{}\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/component/schema.rb", SCHEMA)
        .write("app/models/billing/transaction_input.rb", INPUT)
        .write("app/models/billing/refund_input.rb", REFUND)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
