//! A lowered `T::Struct` answers sorbet's `serialize`, and a bare
//! `ActionDispatch::Request` answers `controller_class`.
//!
//! Core's `Platform::LogEvent.perform` builds `Event.new(input.serialize)`
//! from a `T::Struct` input, and `CurrentRequest#entrypoint` reads
//! `request.controller_class&.name` on every job. `serialize` is the
//! props keyed by String, nil props left out, nested structs serialized;
//! `controller_class` is `PASS_NOT_FOUND` for a request with no route
//! capture, the named class for one with, and `MissingController` for an
//! unknown name.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTS: &str = r#"module Skywatch
  class Leaf < T::Struct
    const :id, Integer
  end

  class Event < T::Struct
    const :shop_id, Integer
    const :verb, String
    const :body, T.nilable(String)
    const :arguments, T.nilable(T::Array[T.untyped]), default: []
    const :leaf, T.nilable(Leaf)
    prop :leaves, T::Array[Leaf], default: []
  end
end
"#;

const SCRIPT: &str = r#"
e = Skywatch::Event.new(shop_id: 1, verb: "create", arguments: [1, "a"], leaf: Skywatch::Leaf.new(id: 7),
                        leaves: [Skywatch::Leaf.new(id: 8)])
expected = { "shop_id" => 1, "verb" => "create", "arguments" => [1, "a"], "leaf" => { "id" => 7 }, "leaves" => [{ "id" => 8 }] }
raise "serialize: #{e.serialize.inspect}" unless e.serialize == expected
raise "keys are strings" unless e.serialize.keys.all?(String)
raise "nil omitted: #{Skywatch::Event.new(shop_id: 2, verb: "x").serialize.inspect}" unless Skywatch::Event.new(shop_id: 2, verb: "x").serialize == { "shop_id" => 2, "verb" => "x", "arguments" => [], "leaves" => [] }

request = ActionDispatch::Request.new({})
raise "bare request: #{request.controller_class.inspect}" unless request.controller_class == ActionDispatch::Request::PASS_NOT_FOUND
raise "pass-not-found name" unless request.controller_class.name == "ActionDispatch::Request::PASS_NOT_FOUND"
request.path_parameters = { controller: "skywatch/pings", action: "show" }
begin
  request.controller_class
  raise "unknown controller must raise"
rescue ActionDispatch::MissingController
end
module Skywatch
  class PingsController; end
end
raise "named controller" unless request.controller_class == Skywatch::PingsController
puts "struct serialize passed"
"#;

#[test]
fn a_lowered_struct_serializes_and_a_request_names_its_controller_class() {
    emit_and_run::real_blog()
        .write("app/services/skywatch.rb", STRUCTS)
        .run_ruby(SCRIPT)
        .assert_passes();
}
