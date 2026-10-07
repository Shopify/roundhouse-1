//! A statement ingest cannot represent is refused at its own site.
//!
//! Before, one such statement anywhere in a support file failed the
//! file's library-class ingest, and the walk left every class, module
//! and constant in the file out of the emitted app (an
//! `ingest_unsupported` error, but a whole file gone for one line). Now
//! the statement alone becomes a raise of `NotImplementedError` naming
//! the construct; the statements around it, the rest of the method and
//! the rest of the file run as written, and `check` reports the refusal
//! as `error[ingest_unsupported]` at its line.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;
use std::process::Command;

/// `for` is a refused construct (`ingest_expr`: it needs
/// enclosing-scope lowering). It sits mid-method, and in one branch
/// of another method, beside code that runs.
const TALLY: &str = r#"module Tally
  LOG = []

  def self.sum(xs)
    LOG << :before
    total = 0
    for x in xs
      total += x
    end
    LOG << :after
    total
  end

  def self.pick(flag, xs)
    if flag
      for x in xs
        LOG << x
      end
      :looped
    else
      LOG << :skipped
      :plain
    end
  end

  def self.count(xs)
    xs.length
  end
end
"#;

const REPORT: &str = r#"
results = []
results << Tally.count([1, 2, 3])
results << Tally.pick(false, [1])
begin
  Tally.sum([1, 2])
  results << :no_raise
rescue NotImplementedError => e
  results << e.class
  results << e.message.include?("`for` requires enclosing-scope lowering")
end
# A ScriptError: an app's bare `rescue` does not swallow the gap.
caught = begin
  begin
    Tally.pick(true, [7])
  rescue => e
    :swallowed
  end
rescue NotImplementedError
  :escaped
end
results << caught
results << Tally::LOG
p results
"#;

#[test]
fn the_refused_statement_raises_and_the_rest_of_the_file_runs() {
    let run = emit_and_run::real_blog().write("app/lib/tally.rb", TALLY).run_ruby(REPORT);
    let refusals: Vec<&String> = run.errors.iter().filter(|e| e.contains("`for` requires")).collect();
    assert_eq!(refusals.len(), 2, "one error per refused statement:\n{}", run.errors.join("\n"));
    assert_eq!(run.errors.len(), 2, "only the refusals:\n{}", run.errors.join("\n"));
    assert!(run.success, "emitted program failed in {}:\n{}", run.emitted.display(), run.stderr);
    assert_eq!(
        run.stdout.trim(),
        "[3, :plain, NotImplementedError, true, :escaped, [:skipped, :before]]",
        "{}",
        run.stderr
    );
}

fn check(root: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(root).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn check_reports_each_refusal_at_its_line() {
    let root = std::env::temp_dir().join(format!("roundhouse-site-refusal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("lib/tally.rb", TALLY),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let text = check(&root);
    let _ = std::fs::remove_dir_all(&root);
    let suffix = "error[ingest_unsupported]: `for` requires enclosing-scope lowering";
    assert!(text.contains(&format!("lib/tally.rb:7:5: {suffix}")), "{text}");
    assert!(text.contains(&format!("lib/tally.rb:16:7: {suffix}")), "{text}");
    assert_eq!(text.matches("error[ingest_unsupported]").count(), 2, "{text}");
    assert!(!text.contains("out of the emitted app"), "the file was dropped:\n{text}");
}
