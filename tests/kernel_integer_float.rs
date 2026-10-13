//! Kernel's `Integer(...)` answers an Integer and `Float(...)` a Float, so
//! a constant read from the environment (`LIMIT = Integer(ENV.fetch(…, 3))`)
//! is a value constant and arithmetic on either is checked.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ADDED: &str = r#"  validates :title, presence: true
  MINIMUM_TITLE_LENGTH = Integer(ENV.fetch("ARTICLE_MINIMUM_TITLE_LENGTH", 3))

  def self.long_enough?(title) = title.length >= MINIMUM_TITLE_LENGTH
  def self.from_hex(text) = Integer(text, 16) + 1
  def self.half(text) = Float(text) / 2
"#;

const SCRIPT: &str = r#"
raise "constant" unless Article::MINIMUM_TITLE_LENGTH == 3
raise "comparison" unless Article.long_enough?("abc") && !Article.long_enough?("ab")
raise "Integer with a base" unless Article.from_hex("ff") == 256
raise "Float" unless Article.half("2.5") == 1.25
puts "Kernel Integer and Float passed"
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::real_blog().edit("app/models/article.rb", "  validates :title, presence: true\n", ADDED)
}

#[test]
fn kernel_integer_and_float_execute_after_emission() {
    app().run_ruby(SCRIPT).assert_passes();
}

#[test]
#[ignore = "requires the native Spinel compiler; set SPINEL"]
fn kernel_integer_and_float_execute_in_native_spinel() {
    app().run_spinel(SCRIPT).assert_passes();
}

/// `Integer(x, exception: false)` answers nil for a failed conversion, so it
/// is not typed a plain Integer: it is `Integer?` (`kernel_conversion_ty`,
/// core `kernel.rbs`), arithmetic on it is flagged as on a nilable, and
/// `|| 0` makes it an Integer again.
#[test]
fn integer_with_exception_false_is_typed_nilable_not_integer() {
    let emit = |body: &str| {
        emit_and_run::real_blog()
            .edit(
                "app/models/article.rb",
                "  validates :title, presence: true\n",
                &format!("  validates :title, presence: true\n  def self.parsed(text) = {body}\n"),
            )
            .emit(roundhouse::project::BuildTarget::Ruby)
            .1
    };
    let errors = emit("Integer(text, exception: false) + \"x\"");
    assert!(
        errors.iter().any(|e| e.contains("incompatible operand types: Integer? + String")),
        "typed as the nilable, not as Integer: {errors:?}"
    );
    let errors = emit("(Integer(text, exception: false) || 0) + 1");
    assert!(!errors.iter().any(|e| e.contains("incompatible operand types")), "{errors:?}");
}
