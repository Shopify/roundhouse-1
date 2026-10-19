//! A `**value` argument whose value a lowering rebuilds keeps its splat.
//!
//! Core's `ServerIntents::References::Lowering#lower_reference` calls
//! `@factory.node(type, **declaration.except("field").symbolize_keys)`.
//! The shared keyword lowering erases the splat to a positional value and
//! marks it (`ERASED_KEYWORD_SPLAT`) for the Ruby family to write back.
//! The `symbolize_keys` grounding then rebuilt the value as
//! `ActiveSupport.symbolize_keys(…)` without the mark. The emitted call
//! passed the Hash positionally, and `node(type, nullable: false,
//! **options)` raised ArgumentError (given 2, expected 1) while the
//! module body ran: every sweep-1 test failed at boot.
//!
//! Native Ruby is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const FACTORY: &str = r#"class SchemaFactory
  #: (String type, ?nullable: bool, **untyped) -> Hash[String, untyped]
  def node(type, nullable: false, **options)
    { "type" => type, "nullable" => nullable }.merge(options.transform_keys(&:to_s))
  end

  #: (String type, Hash[String, untyped] declaration) -> Hash[String, untyped]
  def lower(type, declaration)
    node(type, **declaration.except("field").symbolize_keys)
  end
end
"#;

const REPORT: &str = r#"p SchemaFactory.new.lower("string", { "field" => "title", "max_length" => 3, "nullable" => true })
"#;

const EXPECTED: &str = "{\"type\" => \"string\", \"nullable\" => true, \"max_length\" => 3}\n";

#[test]
fn native_ruby_passes_the_converted_hash_as_keywords() {
    let dir = std::env::temp_dir().join(format!("roundhouse-kwsplat-rebuilt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("schema_factory.rb");
    std::fs::write(&file, FACTORY).unwrap();
    // `symbolize_keys` is ActiveSupport's; the control defines the same conversion.
    let prelude = "class Hash; def symbolize_keys = transform_keys(&:to_sym); end\n";
    let output = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{prelude}require {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

#[test]
fn emitted_ruby_passes_the_converted_hash_as_keywords() {
    let fixture = emit_and_run::real_blog().write("app/lib/schema_factory.rb", FACTORY);
    let (emitted, errors) = fixture.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}
