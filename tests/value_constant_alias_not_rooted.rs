//! A read through a value constant that aliases a module is written as the
//! source wrote it. Core's CollectionSourceConditionValidator (module
//! `Merchandising::Collections`) reads `CollectionSourceCondition::MatchType::
//! ANY`, where the condition class holds `MatchType = Merchandising::
//! Collections::CollectionSource::MatchType`. `ANY` is declared under
//! `CollectionSource::MatchType`, so the constant pass found no declaration
//! at the full name under the enclosing namespace, took the nesting for a
//! shadow, and rooted the read as `::CollectionSourceCondition::MatchType::
//! ANY`: `uninitialized constant CollectionSourceCondition` on every
//! validation of a collection source condition.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"module Merch
  module Collections
    class CollectionSource
      module MatchType
        ANY = "any"
        ALL = "all"
      end
    end
  end
end
"#;

const CONDITION: &str = r#"module Merch
  module Collections
    class CollectionSourceCondition
      MatchType = Merch::Collections::CollectionSource::MatchType
    end
  end
end
"#;

const VALIDATOR: &str = r#"module Merch
  module Collections
    module CollectionSourceConditionValidator
      def any_match?(value)
        value == CollectionSourceCondition::MatchType::ANY
      end

      def modes
        [CollectionSourceCondition::MatchType::ANY, CollectionSourceCondition::MatchType::ALL]
      end
    end
  end
end
"#;

const SCRIPT: &str = r#"probe = Class.new { include Merch::Collections::CollectionSourceConditionValidator }.new
p [probe.any_match?("any"), probe.any_match?("all"), probe.modes]
"#;

#[test]
fn a_read_through_an_aliased_module_constant_stays_lexical() {
    let run = emit_and_run::empty_app()
        .write("app/models/merch/collections/collection_source.rb", SOURCE)
        .write("app/models/merch/collections/collection_source_condition.rb", CONDITION)
        .write("app/models/merch/collections/collection_source_condition_validator.rb", VALIDATOR)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, "[true, false, [\"any\", \"all\"]]\n");
}
