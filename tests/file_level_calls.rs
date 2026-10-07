//! A call at the top level of a support file runs when Ruby loads it.
//! shopify-money registers each subunit converter that way, after the
//! class:
//! `Money::Converters.register(:iso4217, Money::Converters::Iso4217Converter)`.
//! Ingest carries declarations and constants only, and the call used to
//! vanish with no diagnostic, so `Money::Converters.for(:iso4217)`
//! raised "unknown format" in the emitted tree. It is now reported at
//! its own site. A `require` is loading, which the emitted tree does
//! itself, and is not reported.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SOURCE: &str = r#"require "json"

class FileLevelRegistry
  def self.register(key, klass)
    (@entries ||= {})[key] = klass
  end
end

class FileLevelEntry
end

FileLevelRegistry.register(:entry, FileLevelEntry)
"#;

#[test]
fn a_file_level_call_is_reported_at_its_site() {
    let (_tree, errors) = emit_and_run::real_blog()
        .write("app/services/file_level_registry.rb", SOURCE)
        .emit(BuildTarget::Ruby);
    let reported: Vec<&String> = errors.iter().filter(|e| e.contains("file-level call `")).collect();
    assert_eq!(reported.len(), 1, "exactly the register call: {errors:#?}");
    assert!(reported[0].contains("`FileLevelRegistry.register` does not run in the emitted app"), "{reported:#?}");
    assert!(reported[0].contains("classes, modules and constants are carried"), "{reported:#?}");
    let start = SOURCE.find("FileLevelRegistry.register").unwrap();
    assert!(reported[0].contains(&format!("start: {start}, ")), "the span is the call's own: {reported:#?}");
}

/// Each site has one diagnostic. A call inside a file-level conditional
/// is part of the conditional, which `file_level_definition` names when
/// it holds a definition; a call after a file-level `return` is its own
/// statement, reported as a call, while the `return` is named for the
/// definitions it guards.
const GUARDED: &str = r#"class GuardedRegistry
  def self.register(key)
    (@keys ||= []) << key
  end
end

if ENV["RH_NEVER_SET"] == "1"
  class GuardedExtra
  end
  GuardedRegistry.register(:extra)
end

return unless ENV["RH_NEVER_SET"]

class GuardedLate
end

GuardedRegistry.register(:late)
"#;

#[test]
fn a_file_level_call_and_a_file_level_definition_are_never_the_same_site() {
    let (_tree, errors) = emit_and_run::real_blog()
        .write("app/services/guarded_registry.rb", GUARDED)
        .emit(BuildTarget::Ruby);
    let here: Vec<&String> = errors.iter().collect();
    let calls: Vec<&&String> = here.iter().filter(|e| e.contains("file-level call `")).collect();
    let definitions: Vec<&&String> = here.iter().filter(|e| e.contains("file_level_definition not supported")).collect();
    assert_eq!(calls.len(), 1, "only the call after the return: {here:#?}");
    let late = GUARDED.find("GuardedRegistry.register(:late)").unwrap();
    assert!(calls[0].contains(&format!("start: {late}, ")), "{calls:#?}");
    assert_eq!(definitions.len(), 2, "the conditional and the return: {here:#?}");
    let conditional = GUARDED.find("if ENV").unwrap();
    let guard = GUARDED.find("return unless").unwrap();
    assert!(definitions.iter().any(|e| e.contains(&format!("start: {conditional}, "))), "{definitions:#?}");
    assert!(definitions.iter().any(|e| e.contains(&format!("start: {guard}, "))), "{definitions:#?}");
}
