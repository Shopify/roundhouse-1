//! `Rails::Html::SafeListSanitizer` (rails-html-sanitizer's alias of the
//! HTML4 safe-list sanitizer; Core's ViolationErrorMessageSanitizer
//! declares `@sanitizer = Rails::Html::SafeListSanitizer.new #:
//! Rails::Html::SafeListSanitizer`) is a declared type the analyzer knows,
//! so the initializer's value is emitted, not a runtime refusal.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SANITIZER: &str = r##"class AliasSanitizer
  #: -> void
  def initialize
    @sanitizer = Rails::Html::SafeListSanitizer.new #: Rails::Html::SafeListSanitizer
  end

  #: (String? body) -> String?
  def sanitize(body)
    return unless body

    @sanitizer.sanitize(body)
  end
end
"##;

#[test]
fn the_html_alias_of_the_safe_list_sanitizer_is_a_known_declared_type() {
    let (emitted, _, diagnostics) = emit_and_run::real_blog()
        .write("app/models/alias_sanitizer.rb", SANITIZER)
        .emit_with_app(roundhouse::project::BuildTarget::Ruby);
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == roundhouse::diagnostic::Severity::Error)
        .map(ToString::to_string)
        .filter(|d| d.contains("alias_sanitizer"))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let source = std::fs::read_to_string(emitted.join("app/models/alias_sanitizer.rb")).unwrap();
    assert!(source.contains("@sanitizer = Rails::Html::SafeListSanitizer.new"), "{source}");
    assert!(!source.contains("declared type not supported"), "{source}");
}
