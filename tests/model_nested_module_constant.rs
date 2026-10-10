//! A model body reading its own nested module's constant as it loads,
//! Core's components/platform/app/models/event.rb:
//!
//! ```ruby
//! class Event < GlobalDB::ShopRecord
//!   module Verb
//!     CONFIRMED = "confirmed"
//!   end
//!   GENERALIZED_VERBS = [Verb::CONFIRMED, ...].freeze
//! end
//! ```
//!
//! A model's nested module keeps a file of its own (event/verb.rb), and
//! the reference (qualified to `Event::Verb::CONFIRMED`) resolved to the
//! model's own file by its first segment, so nothing required verb.rb
//! and the body raised `uninitialized constant Event::Verb` (5 unit-1000
//! ids). Natively it prints `["confirmed", "create"]`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "events", force: :cascade do |t|
    t.string "verb"
  end
end
"#;

const EVENT: &str = r#"class Event < ApplicationRecord
  module Verb
    CONFIRMED = "confirmed"
    CREATE = "create"
  end

  GENERALIZED_VERBS = [
    Verb::CONFIRMED,
    Verb::CREATE,
  ].freeze
end
"#;

#[test]
fn a_model_body_reading_its_nested_modules_constant_loads() {
    let overlay = || {
        emit_and_run::empty_app()
            .write("db/schema.rb", SCHEMA)
            .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
            .write("app/models/event.rb", EVENT)
    };
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:?}");
    let event = std::fs::read_to_string(emitted.join("app/models/event.rb")).unwrap();
    assert!(event.contains("require_relative \"event/verb\""), "{event}");
    let run = overlay().run_ruby("p Event::GENERALIZED_VERBS\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "[\"confirmed\", \"create\"]", "{}", run.stderr);
}
