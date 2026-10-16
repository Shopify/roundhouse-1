//! A concern reads `Condition::MatchType::ANY` where `Condition` (a class
//! beside it in `Merchandising::Collections`) holds `MatchType =
//! Source::MatchType`, a value constant (Core's
//! `CollectionSourceConditionValidator`). Emitted, the read was rooted as
//! `::Condition::MatchType::ANY` (`uninitialized constant Condition`),
//! though the lexical lookup finds `Merchandising::Collections::Condition`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r#"ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "conditions", force: :cascade do |t|
    t.string "match_type"
    t.string "relation"
  end
end
"#;

const MODELS: &[(&str, &str)] = &[
    ("application_record", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    (
        "merchandising/collections/source",
        "module Merchandising\n  module Collections\n    class Source\n      module MatchType\n        ANY = \"any\"\n        ALL = \"all\"\n      end\n    end\n  end\nend\n",
    ),
    (
        "merchandising/collections/condition_validator",
        r##"module Merchandising
  module Collections
    module ConditionValidator
      extend ActiveSupport::Concern

      included do
        validate :validate_match_type_for_relation
      end

      def validate_match_type_for_relation
        explicit = self[:match_type]
        return if explicit.blank?

        if relation == "any" && explicit != Condition::MatchType::ANY
          errors.add(:base, "must be any")
        elsif relation == "all" &&
            explicit != Condition::MatchType::ALL
          errors.add(:base, "must be all")
        end
      end
    end
  end
end
"##,
    ),
    (
        "merchandising/collections/condition",
        "module Merchandising\n  module Collections\n    class Condition < ApplicationRecord\n      include ConditionValidator\n\n      MatchType = Merchandising::Collections::Source::MatchType\n    end\n  end\nend\n",
    ),
];

const PROBE: &str = r##"module ValueConstantProbe
  def self.run
    ok = Merchandising::Collections::Condition.new(relation: "any", match_type: "any")
    bad_any = Merchandising::Collections::Condition.new(relation: "any", match_type: "all")
    bad_all = Merchandising::Collections::Condition.new(relation: "all", match_type: "any")
    puts "valid: #{ok.valid?} #{bad_any.valid?} #{bad_all.valid?}"
    puts "errors: #{bad_any.errors.full_messages.inspect} #{bad_all.errors.full_messages.inspect}"
  end
end
"##;

#[test]
fn a_value_constant_read_through_a_sibling_class_is_not_rooted() {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("lib/value_constant_probe.rb", PROBE);
    for (name, source) in MODELS {
        app = app.write(&format!("app/models/{name}.rb"), source);
    }
    let run = app.run_ruby("ValueConstantProbe.run");
    run.assert_passes();
    assert_eq!(
        run.stdout,
        "valid: true false false\nerrors: [\"must be any\"] [\"must be all\"]\n",
        "{}",
        run.stderr
    );
}
