//! The bare `ActiveSupport::HashWithIndifferentAccess` constant read as a
//! value: Core's `VersionedMemCacheStore#versions` hands the class to
//! Paquito's codec factory. The CRuby tree carries Rails' class, so the
//! read is the class and a converted hash is an instance of it, as in
//! Rails. `.new` stays the lowering's.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const CODEC: &str = r##"module Codec
  def self.versions
    [Symbol, ActiveSupport::HashWithIndifferentAccess]
  end

  def self.bare
    HashWithIndifferentAccess
  end

  def self.kind(value)
    case value
    when HashWithIndifferentAccess then "indifferent"
    when Hash then "hash"
    else "other"
    end
  end

  def self.indifferent?(value)
    value.is_a?(ActiveSupport::HashWithIndifferentAccess)
  end
end
"##;

#[test]
fn bare_indifferent_constant_is_rails_class_on_the_ruby_tree() {
    let run = emit_and_run::empty_app()
        .write("app/models/codec.rb", CODEC)
        .run_ruby(
            r##"
raise "versions #{Codec.versions.inspect}" unless Codec.versions == [Symbol, ActiveSupport::HashWithIndifferentAccess]
raise "bare #{Codec.bare.inspect}" unless Codec.bare.equal?(ActiveSupport::HashWithIndifferentAccess)
converted = { a: 1 }.with_indifferent_access
raise "is_a? false" unless converted.is_a?(ActiveSupport::HashWithIndifferentAccess)
raise "plain hash is_a?" if { a: 1 }.is_a?(ActiveSupport::HashWithIndifferentAccess)
raise "converted indifferent? false" unless Codec.indifferent?(converted)
raise "plain indifferent?" if Codec.indifferent?({ "a" => 1 })
raise "kind #{Codec.kind(converted)}" unless Codec.kind(converted) == "indifferent"
raise "kind plain #{Codec.kind({ a: 1 })}" unless Codec.kind({ a: 1 }) == "hash"
puts "ALL OK"
"##,
        );
    // `check` still names each bare read (the CRuby tree is the only one
    // that carries the class); nothing else is reported, and the program ran.
    assert!(!run.errors.is_empty(), "the bare reads are named");
    assert!(
        run.errors.iter().all(|e| e.contains("constant not supported") && e.contains("HashWithIndifferentAccess")),
        "{:#?}",
        run.errors
    );
    assert!(run.success, "stdout: {}\nstderr: {}", run.stdout, run.stderr);
    assert!(run.stdout.contains("ALL OK"), "{}", run.stdout);
}

/// No other tree defines the class: the read is refused there, by name.
#[test]
fn other_trees_refuse_the_bare_constant() {
    let (_, errors) = emit_and_run::empty_app()
        .write("app/models/codec.rb", CODEC)
        .emit(BuildTarget::Spinel);
    assert!(
        errors.iter().any(|e| e.contains("HashWithIndifferentAccess") && e.contains("the spinel tree does not load the gem")),
        "{errors:#?}"
    );
}
