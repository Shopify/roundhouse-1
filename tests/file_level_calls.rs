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
    let reported: Vec<&String> = errors.iter().filter(|e| e.contains("file-level call not supported")).collect();
    assert_eq!(reported.len(), 1, "exactly the register call: {errors:#?}");
    assert!(reported[0].contains("`FileLevelRegistry.register`"), "{reported:#?}");
    let start = SOURCE.find("FileLevelRegistry.register").unwrap();
    assert!(reported[0].contains(&format!("start: {start}, ")), "the span is the call's own: {reported:#?}");
}
