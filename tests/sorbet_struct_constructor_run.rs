//! A lowered `T::Struct` builds as sorbet's generated constructor does.
//!
//! `T::Struct#initialize` takes the props as keywords or as one
//! positional Hash, and a `T.nilable` prop is optional (nil when
//! omitted). Core builds `Webhooks::RequiredAccess` from a config Hash
//! whose every prop is nilable; the keyword-only constructor with all
//! keywords required died `given 1, expected 0; required keywords: …`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const STRUCTS: &str = r#"module Skywatch
  class Access < T::Struct
    prop :scope, T.nilable(T.any(String, Symbol))
    prop :permission, T.nilable(String)
    prop :flag, T.nilable(String), default: "on"
  end

  class Named < T::Struct
    const :name, String
    prop :note, T.nilable(String)
  end
end
"#;

const SCRIPT: &str = r#"
a = Skywatch::Access.new
raise "nilable omitted is nil" unless a.scope.nil? && a.permission.nil?
raise "default kept" unless a.flag == "on"
b = Skywatch::Access.new(scope: :read, flag: "off")
raise "keywords" unless b.scope == :read && b.flag == "off" && b.permission.nil?
hash = ActiveSupport::HashWithIndifferentAccess.new("scope" => "write", "permission" => "p")
c = Skywatch::Access.new(hash)
raise "positional hash" unless c.scope == "write" && c.permission == "p" && c.flag == "on"
d = Skywatch::Access.new({ scope: "x", flag: nil })
raise "symbol hash" unless d.scope == "x" && d.flag.nil?
n = Skywatch::Named.new(name: "n")
raise "required const with nilable omitted" unless n.name == "n" && n.note.nil?
begin
  Skywatch::Named.new(note: "x")
  raise "missing required const must raise"
rescue ArgumentError
end
puts "struct constructor passed"
"#;

#[test]
fn a_lowered_struct_takes_a_positional_hash_and_optional_nilable_props() {
    emit_and_run::real_blog()
        .write("app/services/skywatch.rb", STRUCTS)
        .run_ruby(SCRIPT)
        .assert_passes();
}

const PROPS_STRUCT: &str = r#"module Skywatch
  class Profile < T::Struct
    const :id, Integer
    const :name, String
    prop :tags, T::Array[String], default: []
    const :note, T.nilable(String)

    def self.from_attributes(attributes, extra)
      selected = attributes.with_indifferent_access.slice(*props.keys).merge({ tags: extra })
      new(**selected)
    end
  end
end
"#;

const PROPS_SCRIPT: &str = r#"
raise "props names in declaration order: #{Skywatch::Profile.props.keys.inspect}" unless Skywatch::Profile.props.keys == %i[id name tags note]
raise "const is immutable" unless Skywatch::Profile.props[:name][:immutable] == true && Skywatch::Profile.props[:tags][:immutable].nil?
raise "nilable is flagged" unless Skywatch::Profile.props[:note][:_tnilable] == true
profile = Skywatch::Profile.from_attributes({ "id" => 1, "name" => "n", "ignored" => 2 }, ["a"])
raise "slice by props" unless profile.id == 1 && profile.name == "n" && profile.tags == ["a"] && profile.note.nil?
puts "struct props passed"
"#;

#[test]
fn a_lowered_struct_answers_props_by_name() {
    emit_and_run::real_blog()
        .write("app/services/skywatch_profile.rb", PROPS_STRUCT)
        .run_ruby(PROPS_SCRIPT)
        .assert_passes();
}
