//! `A::B = value` in a module body defines `A::B`, in source order.
//!
//! Generated protobuf `_pb.rb` files nest every message through a path
//! write: `CustomFields = …lookup("….CustomFields").msgclass` and then
//! `CustomFields::Field = …msgclass` (Core's CheckoutProtobufDefinitions,
//! Sales::Subscriptions::ProtobufDefinitions, Shopify::Protobuf). The
//! class-body walk matched only `NAME = value`, so each path write fell
//! through the statement loop: the emitted module lacked it, every read
//! reported "constant not supported", and Ruby raised NameError.
//!
//! The write is carried as a constant named by its written path, among
//! the body's other constants in source order, and the registry types it
//! by its last segment's declaration. The control runs the same script on
//! CRuby against the source.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const DEFS: &str = r#"module Defs
  module Orders
    Ref = Struct.new(:a)
    Ref::Obj = Struct.new(:b)
    Ref::Obj::Deep = Struct.new(:c)
    Ref::LIMIT = 5
    ::Defs::TOP = "top"
    Plain = Object.new.class
    Ref::Other = Plain
  end
end
"#;

const USER: &str = r#"class UserOfDefs
  def go = [Defs::Orders::Ref, Defs::Orders::Ref::Obj, Defs::Orders::Ref::Obj::Deep, Defs::Orders::Ref::Other]

  def limit = Defs::Orders::Ref::LIMIT + 1

  def top = Defs::TOP

  def deep = Defs::Orders::Ref::Obj::Deep.new(3).c
end
"#;

const SCRIPT: &str = r#"u = UserOfDefs.new
p u.go.size
p [Defs::Orders::Ref.new(1).a, Defs::Orders::Ref::Obj.new(2).b, u.deep]
p [u.limit, u.top, Defs::Orders::Ref::Other == Object]
p Defs::Orders::Ref.constants(false).sort
p Defs::Orders::Ref::Obj.name
"#;

const EXPECTED: &str = "4\n[1, 2, 3]\n[6, \"top\", true]\n[:LIMIT, :Obj, :Other]\n\"Defs::Orders::Ref::Obj\"\n";

#[test]
fn a_constant_path_write_defines_the_nested_constant_in_order() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{DEFS}\n{USER}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);

    let run = emit_and_run::empty_app()
        .write("app/models/defs.rb", DEFS)
        .write("app/models/user_of_defs.rb", USER)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);

    // The reads type from the path writes' values.
    let sig = std::fs::read_to_string(run.emitted.join("sig/app/models/user_of_defs.rbs")).expect("user_of_defs.rbs");
    assert!(sig.contains("def limit: () -> Integer"), "{sig}");
    assert!(sig.contains("def top: () -> String"), "{sig}");
}
