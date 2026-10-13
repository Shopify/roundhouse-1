//! `errors.add` grounding (`lower::apply_errors_add_lowering`).
//!
//! Shape tests over the accumulator rewrite: self-receiver `add` calls
//! ground to `errors << "Humanized msg"` (:base bare, dynamic message
//! interpolated, missing message defaulted), and non-self receivers
//! keep their dynamic call with a `lower_residue` warning.

use roundhouse::analyze::{Analyzer, Diagnostic};
use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_library_classes;
use roundhouse::lower::apply_errors_add_lowering;
use roundhouse::App;

fn lower_and_emit(source: &str) -> (String, Vec<Diagnostic>) {
    lower_and_emit_on(source, None)
}

/// On the PostgreSQL CRuby tree, which records each push through the
/// `ActiveModel::ErrorAttributes` hint rather than an `error_details` array.
fn lower_and_emit_on_postgresql(source: &str) -> (String, Vec<Diagnostic>) {
    lower_and_emit_on(source, Some(roundhouse::database::DatabaseAdapter::Postgresql))
}

fn lower_and_emit_on(source: &str, adapter: Option<roundhouse::database::DatabaseAdapter>) -> (String, Vec<Diagnostic>) {
    let classes =
        ingest_library_classes(source.as_bytes(), "test.rb").expect("ingest test source");
    let mut app = App::new();
    app.database_yml = adapter.map(|adapter| roundhouse::database::DatabaseYml {
        path: "config/database.yml".into(),
        adapter,
        renderer: None,
        present: true,
        manifest: false,
        test_adapters: Vec::new(),
    });
    for lc in classes {
        app.library_classes.push(lc);
    }
    Analyzer::new(&app).analyze(&mut app);
    let diags = apply_errors_add_lowering(&mut app);
    let out = emit_library(&app)
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    (out, diags)
}

#[test]
fn field_and_literal_message_ground_humanized() {
    let (out, diags) = lower_and_emit(
        r#"
class Draft
  def validate_body
    errors.add(:short_id, "is taken")
  end
end
"#,
    );
    assert!(!out.contains("errors.add"), "site should be grounded:\n{out}");
    // Rails humanize strips a trailing `_id`: :short_id → "Short".
    assert!(
        out.contains(r#"errors << "Short is taken""#),
        "expected humanized accumulator push:\n{out}"
    );
    assert!(diags.is_empty(), "self-receiver site should not produce residue: {diags:?}");
}

#[test]
fn base_contributes_bare_message() {
    let (out, _) = lower_and_emit(
        r#"
class Draft
  def validate_body
    errors.add(:base, "record stale")
  end
end
"#,
    );
    assert!(
        out.contains(r#"errors << "record stale""#),
        ":base must not be humanized into the message:\n{out}"
    );
}

#[test]
fn missing_message_defaults_to_is_invalid() {
    let (out, _) = lower_and_emit(
        r#"
class Draft
  def validate_body
    errors.add(:title)
  end
end
"#,
    );
    assert!(
        out.contains(r#"errors << "Title is invalid""#),
        "expected the Rails default message:\n{out}"
    );
}

#[test]
fn dynamic_message_interpolates_after_field() {
    let (out, _) = lower_and_emit(
        r#"
class Draft
  def validate_body(why)
    errors.add(:title, why)
  end
end
"#,
    );
    assert!(!out.contains("errors.add"), "{out}");
    assert!(
        out.contains(r#"errors << "Title #{why}""#),
        "expected interpolated message:\n{out}"
    );
}

#[test]
fn non_self_receiver_grounds_on_its_own_accumulator() {
    // `record.errors.add(...)` from outside the model (lobsters'
    // duplicate-comment guard) grounds the same way — the receiver is
    // preserved, so the append lands on record's accumulator.
    let (out, diags) = lower_and_emit(
        r#"
class Reviewer
  def flag(record)
    record.errors.add(:title, "not yours")
  end
end
"#,
    );
    assert!(
        out.contains(r#"record.errors << "Title not yours""#),
        "outside-the-record add should ground with its receiver:\n{out}"
    );
    assert!(diags.is_empty(), "non-self site should ground without residue: {diags:?}");
}

#[test]
fn message_keyword_unwraps_to_the_message() {
    // Rails' options spelling: the sole `message:` entry carries the
    // message; any other option set stays residue.
    let (out, diags) = lower_and_emit(
        r#"
class Story
  def check
    errors.add(:moderation_reason, message: "is required")
  end
end
"#,
    );
    assert!(
        out.contains(r#"errors << "Moderation reason is required""#),
        "expected the message: value baked after the humanized field:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn non_message_options_keep_dispatch_with_residue() {
    let (out, diags) = lower_and_emit(
        r#"
class Story
  def check
    errors.add(:url, message: "is taken", strict: true)
  end
end
"#,
    );
    assert!(
        out.contains("errors.add"),
        "unrecognized option set must keep its dynamic call:\n{out}"
    );
    assert_eq!(diags.len(), 1, "expected one residue entry: {diags:?}");
    assert!(diags[0].message.contains("unrecognized arg shape"), "{:?}", diags[0].message);
}

#[test]
fn literal_concat_message_folds_before_bake() {
    // Lobsters wraps long messages as `"a " << "b"` — string-literal
    // concat folds into the baked literal (a runtime `<<` on a frozen
    // literal is a hazard the bake sidesteps).
    let (out, diags) = lower_and_emit(
        r#"
class Comment
  def check
    errors.add(:comment, "^You have already posted " << "here recently.")
  end
end
"#,
    );
    assert!(
        out.contains(r#"errors << "Comment ^You have already posted here recently.""#),
        "expected the concat folded into one literal:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn parameter_field_humanizes_at_run_time() {
    // An EachValidator's `validate_each(record, attribute, value)`.
    let (out, diags) = lower_and_emit_on_postgresql(
        r#"
class LowercaseValidator
  def validate_each(record, attribute, value)
    record.errors.add(attribute, "must be lowercase")
  end
end
"#,
    );
    assert!(!out.contains("errors.add"), "site should be grounded:\n{out}");
    assert!(
        out.contains(
            r##"    record.errors << if attribute.to_s == "base"
      "must be lowercase"
    else
      "#{ActiveSupport.humanize(attribute.to_s)} must be lowercase"
    end"##
        ),
        "expected the run-time full message:\n{out}"
    );
    assert!(diags.is_empty(), "a parameter field grounds without residue: {diags:?}");
}

#[test]
fn error_type_symbols_bake_their_default_message() {
    let (out, diags) = lower_and_emit(
        r#"
class Draft
  def validate_body(attribute)
    errors.add(:title, :blank)
    errors.add(:base, :invalid)
    errors.add(attribute, :taken)
    errors.add(:title, :too_long)
  end
end
"#,
    );
    assert!(out.contains(r#"errors << "Title can't be blank""#), "{out}");
    assert!(out.contains(r#"errors << "is invalid""#), "{out}");
    assert!(out.contains(r##""#{ActiveSupport.humanize(attribute.to_s)} has already been taken""##), "{out}");
    // :too_long interpolates a count: no count-free default, so residue.
    assert!(out.contains("errors.add(:title, :too_long)"), "{out}");
    assert_eq!(diags.len(), 1, "{diags:?}");
}

#[test]
fn computed_field_stays_residue() {
    let (out, diags) = lower_and_emit(
        r#"
class Draft
  def validate_body
    errors.add(field_name, "is odd")
  end
end
"#,
    );
    assert!(out.contains("errors.add(field_name"), "{out}");
    assert_eq!(diags.len(), 1, "{diags:?}");
}

#[test]
fn class_typed_errors_reader_keeps_its_own_add() {
    // An app's own error collection (Core's CustomerFoundations::Errors)
    // has a real `add(attribute, code)`; it is not the Array[String]
    // accumulator, so the call stays as written and is not residue.
    let (out, diags) = lower_and_emit(
        r#"
class AppErrors
  #: () -> void
  def initialize = @list = []

  #: (Symbol, Symbol) -> void
  def add(attribute, code) = @list << [attribute, code]
end

class Signup
  #: () -> AppErrors
  def errors
    @errors ||= AppErrors.new #: AppErrors?
  end

  #: () -> void
  def check
    errors.add(:email, :taken)
  end
end
"#,
    );
    assert!(out.contains("errors.add(:email, :taken)"), "{out}");
    assert!(!out.contains("has already been taken"), "{out}");
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_record_that_is_not_a_plain_read_stays_residue() {
    // The message and its error are two pushes; a receiver call would run once per push.
    let (out, diags) = lower_and_emit(
        r#"
class Draft
  def flag
    Draft.latest.errors.add(:title, "is odd")
    Draft.latest.errors.clear
  end
end
"#,
    );
    assert!(out.contains(r#"Draft.latest.errors.add(:title, "is odd")"#), "{out}");
    assert!(out.contains("Draft.latest.errors.clear"), "{out}");
    assert_eq!(diags.len(), 2, "{diags:?}");
}

#[test]
fn each_push_records_the_error_rails_keeps() {
    let (out, diags) = lower_and_emit(
        r#"
class Draft
  def validate_body(why)
    errors.add(:title, :taken)
    errors.add(:title, why)
  end
end
"#,
    );
    assert!(
        out.contains(r#"error_details << ActiveRecord::ErrorDetail.new("title", "taken", "has already been taken")"#),
        "a Symbol message is the error type:\n{out}"
    );
    assert!(
        out.contains(r#"error_details << ActiveRecord::ErrorDetail.new("title", why, why)"#),
        "a String message is its own type:\n{out}"
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_message_that_is_not_a_plain_read_stays_residue() {
    // The full text and the record each read the message; a call would run twice.
    let (out, diags) = lower_and_emit(
        r#"
class Draft
  def validate_body(record)
    errors.add(:title, reason_for(record))
    errors.add(:body, next_message)
  end
end
"#,
    );
    assert!(out.contains("errors.add(:title, reason_for(record))"), "{out}");
    assert!(out.contains("errors.add(:body, next_message)"), "{out}");
    assert_eq!(diags.len(), 2, "{diags:?}");
}
