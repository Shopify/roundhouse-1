//! A key into a plain `Hash[String, _]` is emitted as written.
//!
//! The signature says `Hash[String, untyped]`, but at run time the Hash
//! is whatever the caller passed. Indexing it with a Symbol key misses
//! the String entry natively. Coercing the key with `.to_s` (the params
//! store's indifferent access) read `"x"`'s value for `:x`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LOOKUP: &str = r#"module Lookup
  #: (Hash[String, untyped], Array[untyped]) -> Array[untyped]
  def self.pick(values, keys)
    keys.map { |key| values[key] }
  end
end
"#;

#[test]
fn a_symbol_key_into_a_string_typed_hash_reads_its_own_entry() {
    let run = emit_and_run::empty_app()
        .write("app/models/lookup.rb", LOOKUP)
        .run_ruby("p Lookup.pick({ \"x\" => 1, x: 2 }, [\"x\", :x, :y])\n");
    run.assert_passes();
    assert_eq!(run.stdout, "[1, 2, nil]\n");
}
