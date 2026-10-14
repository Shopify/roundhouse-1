//! A module that `extend`s `T::Generic` answers `Mod[X, Y]` with itself
//! on the emitted tree. The emit drops the `extend T::Generic` and erases
//! the tree's own applications (src/ingest/sorbet_check.rs,
//! erase_generic_applications), but a gem that loads real sorbet-runtime
//! applies the generic in its `sig` blocks against the emitted module:
//! shopify_metafields' `::Shopify::Adt::Result[T.attached_class, ...]`
//! raised "undefined method '[]' for module Shopify::Adt::Result" when
//! `Type.coerce` was defined. A class with its own class-side `[]` keeps it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const RESULT: &str = r#"module Shopify
  module Adt
    module Result
      extend(T::Sig)
      extend(T::Generic)

      OkType = type_member(:out)
      ErrorType = type_member(:out)

      def self.ok(value)
        [:ok, value]
      end
    end
  end
end
"#;

/// Its own `[]` means something else, so no generic application is added.
const LOOKUP: &str = r#"class Lookup
  extend T::Generic

  def self.[](key)
    "looked up #{key}"
  end
end
"#;

#[test]
fn an_emitted_generic_module_answers_its_type_application() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/shopify/adt/result.rb", RESULT)
        .write("app/models/lookup.rb", LOOKUP)
        .run_ruby(
            "applied = Shopify::Adt::Result[Integer, String]\n\
             puts [applied.equal?(Shopify::Adt::Result), applied.ok(1).inspect, Lookup[:a]].inspect\n",
        );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.trim(), r#"[true, "[:ok, 1]", "looked up a"]"#, "{}", run.stderr);
}
