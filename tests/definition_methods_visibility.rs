//! `definition_methods do … end` (graphql-ruby's interface DSL) module_evals
//! its block on a module of its own that the interface extends, so a
//! `private` in the block scopes to that block's defs, not to the
//! interface. A Core interface (`GraphApi::Admin::DiscountApplication`)
//! that writes `private` there was refused at ingest, which left the whole
//! file out of the emitted app and every field type naming it raised a
//! NameError.
//!
//! CRuby is the oracle: the same resolution report runs on the source with
//! native Ruby and on the emitted app, and the two must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

/// graphql-ruby's `Interface::DefinitionMethods` mechanism, reduced to the
/// part that matters here: the block is module_eval'd on a module the
/// interface extends.
const DSL: &str = r##"module Conf
  module InterfaceType
    def definition_methods(&block)
      mod = Module.new
      mod.module_eval(&block)
      extend(mod)
    end
  end
end
"##;

const INTERFACE: &str = r##"module Conf
  module Discount
    extend Conf::InterfaceType

    definition_methods do
      def resolve_type(object)
        resolve_with_correct_types(object)
      end

      private

      def resolve_with_correct_types(object)
        case object
        when String then :text
        else :other
        end
      end
    end
  end

  class Field
    def self.type = Conf::Discount
  end
end
"##;

const REPORT: &str = r##"
puts Conf::Field.type.name
puts Conf::Discount.resolve_type("a")
puts Conf::Discount.resolve_type(1)
puts Conf::Discount.respond_to?(:resolve_with_correct_types)
puts Conf::Discount.respond_to?(:resolve_with_correct_types, true)
"##;

const EXPECTED: &str = "Conf::Discount\ntext\nother\nfalse\ntrue\n";

#[test]
fn native_ruby_scopes_private_to_the_definition_methods_block() {
    let output = Command::new("ruby")
        .arg("-e")
        .arg(format!("{DSL}\n{INTERFACE}\n{REPORT}"))
        .output()
        .expect("native Ruby control");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_interface_with_private_in_definition_methods_loads_and_matches_native() {
    let run = emit_and_run::real_blog()
        .write("app/lib/conf/interface_type.rb", DSL)
        .write("app/lib/conf/discount.rb", INTERFACE)
        .run_ruby(REPORT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}
