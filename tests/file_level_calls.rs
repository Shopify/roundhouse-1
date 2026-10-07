//! A call at the top level of a support file runs when Ruby loads it.
//! shopify-money registers each subunit converter that way, after the
//! class:
//! `Money::Converters.register(:iso4217, Money::Converters::Iso4217Converter)`.
//! Ingest carried declarations and constants only, and the call
//! vanished with no diagnostic, so `Money::Converters.for(:iso4217)`
//! raised "unknown format" in the emitted tree. The file is now carried
//! as its file program (`App::file_programs`), which runs the call where
//! the source has it; a statement the program cannot carry is reported
//! once, at its own site (`file_statement_unsupported`), and named at
//! boot if Ruby reaches it (`error[file_statement_not_carried]`).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"require "json"

class FileLevelRegistry
  def self.register(key, klass)
    (@entries ||= {})[key] = klass
  end

  def self.entries
    @entries
  end
end

class FileLevelEntry
end

FileLevelRegistry.register(:entry, FileLevelEntry)
"#;

fn file_level(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("file_level") || e.contains("file-level")).collect()
}

#[test]
fn a_file_level_call_runs_where_the_source_has_it() {
    let run = emit_and_run::real_blog()
        .write("app/services/file_level_registry.rb", SOURCE)
        .run_ruby("p FileLevelRegistry.entries\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "{entry: FileLevelEntry}\n", "{}", run.stderr);
    assert!(file_level(&run.errors).is_empty(), "the call is carried: {:#?}", run.errors);
}

/// Each site has at most one diagnostic. The conditional, the `return`
/// and the calls are carried, so none of them is reported; a statement
/// the ingest cannot carry (a shell-out) is reported once at its own
/// site, inside the conditional or after the `return` alike.
const GUARDED: &str = r#"class GuardedRegistry
  def self.register(key)
    (@keys ||= []) << key
  end

  def self.keys
    @keys
  end
end

if ENV["RH_NEVER_SET"] == "1"
  class GuardedExtra
  end
  GuardedRegistry.register(:extra)
  `true`
end

return unless ENV["RH_NEVER_SET"]

class GuardedLate
end

GuardedRegistry.register(:late)
`false`
"#;

#[test]
fn a_file_level_statement_is_reported_once_at_its_own_site() {
    let run = emit_and_run::real_blog()
        .write("app/services/guarded_registry.rb", GUARDED)
        .run_ruby("p GuardedRegistry.keys\np defined?(GuardedExtra)\np defined?(GuardedLate)\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "nil\nnil\nnil\n", "{}", run.stderr);
    let reported = file_level(&run.errors);
    assert_eq!(reported.len(), 2, "the two shell-outs only: {reported:#?}");
    for (site, text) in [("`true`", "in the conditional"), ("`false`", "after the return")] {
        let start = GUARDED.find(site).unwrap();
        let here: Vec<&&String> = reported.iter().filter(|e| e.contains(&format!("start: {start}, "))).collect();
        assert_eq!(here.len(), 1, "one report for the shell-out {text}: {reported:#?}");
        assert!(here[0].contains("file-level statement `"), "{here:#?}");
        assert!(here[0].contains("does not run in the emitted app"), "{here:#?}");
        assert!(here[0].contains("shell command (backticks) is not modeled"), "{here:#?}");
    }
    // The carried statements have no report of their own.
    for carried in ["if ENV", "GuardedRegistry.register(:extra)", "return unless", "GuardedRegistry.register(:late)"] {
        let start = GUARDED.find(carried).unwrap();
        assert!(
            !reported.iter().any(|e| e.contains(&format!("start: {start}, "))),
            "`{carried}` is carried and not reported: {reported:#?}"
        );
    }
    // Neither shell-out is reached: the branch is not taken, and the
    // `return` ends the load first.
    assert!(!run.stderr.contains("file_statement_not_carried"), "{}", run.stderr);
}
