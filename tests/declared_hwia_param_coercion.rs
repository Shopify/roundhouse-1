//! A library method's parameter declared `HashWithIndifferentAccess` and read
//! by a Symbol key, Core's `GraphApi::Admin::CollectionInput.check_access`:
//!
//! ```ruby
//! #: (Hash[Symbol, untyped], HashWithIndifferentAccess) -> bool
//! def self.check_access(input) = input[:input][:publications]
//! ```
//!
//! The lowering reads the string key an indifferent hash answers, but the
//! Cardinal executor hands the method its `arguments`, a plain Hash of Symbol
//! keys: the read answered nil (`undefined method '[]' for nil`, three
//! CollectionUpdateTest ids). On the PostgreSQL (CRuby) tree the parameter is
//! converted on entry, and either key reads.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const INPUT: &str = r##"# typed: true
class Checker
  #: (ActiveSupport::HashWithIndifferentAccess) -> untyped
  def self.publications(input)
    input[:input][:publications]
  end

  #: (ActiveSupport::HashWithIndifferentAccess) -> untyped
  def self.reassigned(input)
    input = input
    input[:input]
  end
end
"##;

fn overlay(adapter: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("database_adapter: {adapter}\n"))
        .write("app/models/checker.rb", INPUT)
}

#[test]
fn the_declared_param_is_converted_on_entry_on_postgresql() {
    let (emitted, _errors) = overlay("postgresql").emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/checker.rb")).unwrap();
    assert!(text.contains("input = ActiveSupport.indifferent_param(input)"), "{text}");
    // A parameter the body assigns keeps what it is given.
    let reassigned = text.split("def self.reassigned").nth(1).unwrap();
    assert!(!reassigned.contains("indifferent_param"), "{text}");
}

#[test]
fn another_adapter_is_left_alone() {
    let (emitted, _errors) = overlay("sqlite").emit(roundhouse::project::BuildTarget::Ruby);
    let text = std::fs::read_to_string(emitted.join("app/models/checker.rb")).unwrap();
    assert!(!text.contains("indifferent_param"), "{text}");
}
