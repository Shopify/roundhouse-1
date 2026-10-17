//! A `T::Struct` that overrides `serialize` and calls `super()`: sorbet's
//! `T::Props::Serializable#serialize` lives on the ancestor. Core's
//! `CustomDiscountSignaturePayloadType` post-processes the Hash it gets.
//! The flattened struct raised `super: no superclass method 'serialize'`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PAYLOAD: &str = r##"class Payload < T::Struct
  const :title, String
  const :count, Integer

  def serialize(_strict = true)
    obj = super()
    obj["title"] = obj["title"].upcase
    obj
  end
end
"##;

const PLAIN: &str = r##"class Plain < T::Struct
  const :title, String

  def serialize(_strict = true)
    { "only" => title }
  end
end
"##;

#[test]
fn a_struct_serialize_override_reaches_the_generated_one_through_super() {
    emit_and_run::empty_app()
        .write("app/models/payload.rb", PAYLOAD)
        .write("app/models/plain.rb", PLAIN)
        .run_ruby(
            r##"
got = Payload.new(title: "ab", count: 2).serialize
raise got.inspect unless got == { "title" => "AB", "count" => 2 }
raise Plain.new(title: "x").serialize.inspect unless Plain.new(title: "x").serialize == { "only" => "x" }
puts "ALL OK"
"##,
        )
        .assert_passes();
}
