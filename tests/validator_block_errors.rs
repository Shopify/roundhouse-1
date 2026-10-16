//! `errors.added?` (and its sisters) inside a plain ActiveModel::Validations
//! class's `validate do ... end` block. The block's `errors` is Rails'
//! ActiveModel::Errors, whose queries by attribute the emitted accumulator
//! (an Array of messages) cannot answer on its own: the call goes through
//! the runtime's `ActiveModel::ErrorAttributes`, as it does in a model.
//! Core's MerchantSessionValidator reads `errors.added?(:user,
//! :session_revoked)` in one.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const VALIDATOR: &str = r#"class SessionValidator
  include ActiveModel::Validations

  attr_reader :user

  def initialize(user)
    @user = user
  end

  validate do
    errors.add(:user, :blank) unless user.present?
  end

  validate do
    errors.add(:user, :session_revoked) if user == "revoked"
  end

  validate do
    if user && !errors.added?(:user, :session_revoked)
      errors.add(:user, :content_changed) if user == "changed"
    end
  end
end
"#;

const LOCALE: &str = r#"en:
  activemodel:
    errors:
      models:
        session_validator:
          attributes:
            user:
              session_revoked: session revoked
              content_changed: content changed
"#;

const PROBE: &str = r#"class ValidatorProbe
  def self.lines
    ["revoked", "changed", "fine", nil].map do |user|
      validator = SessionValidator.new(user)
      validator.valid?
      [user.inspect, validator.errors.added?(:user, :session_revoked), validator.errors.added?(:user, :blank),
       validator.errors.added?(:user, :content_changed), validator.errors.full_messages].inspect
    end
  end
end
"#;

#[test]
fn errors_added_in_a_validate_block_asks_the_errors() {
    let run = emit_and_run::empty_app()
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("config/locales/en.yml", LOCALE)
        .write("app/models/session_validator.rb", VALIDATOR)
        .write("app/models/validator_probe.rb", PROBE)
        .run_ruby("puts ValidatorProbe.lines");
    run.assert_passes();
    let validator = std::fs::read_to_string(run.emitted.join("app/models/session_validator.rb")).unwrap();
    assert!(!validator.contains("self.errors.added?"), "the Array accumulator has no added?:\n{validator}");
    let expected = [
        "[\"\\\"revoked\\\"\", true, false, false, [\"User session revoked\"]]",
        "[\"\\\"changed\\\"\", false, false, true, [\"User content changed\"]]",
        "[\"\\\"fine\\\"\", false, false, false, []]",
        "[\"nil\", false, true, false, [\"User can't be blank\"]]",
    ];
    for line in expected {
        assert!(run.stdout.contains(line), "missing {line}\n{}", run.stdout);
    }
}
