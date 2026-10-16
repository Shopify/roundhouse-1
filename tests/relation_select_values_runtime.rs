//! `Relation#select_values`, which Core's
//! `Podding::Database::Iteration.check_relation_for_batching` reads to
//! see the columns a batched relation selects. The relation answered
//! `undefined method 'select_values' for ActiveRecord::Relation`
//! (JobIteration ids, e2e v2-r35).
//!
//! Rails: the fields `select` was given as written, in order, each once;
//! `unscope(:select)` and a relation that never selected answer `[]`.
//! A spawned relation keeps its parent's list and does not write back.
//!
//! Skips when `ruby` with the sqlite3 gem is not on the machine.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

#[test]
fn select_values_lists_the_selected_fields_as_written() {
    if !Command::new("ruby").args(["-rsqlite3", "-e", "1"]).status().is_ok_and(|s| s.success()) {
        eprintln!("skipping: ruby with the sqlite3 gem not available");
        return;
    }
    let out = emit_and_run::real_blog().run_ruby(
        r#"
rel = ActiveRecord::Relation.new(Article)
p rel.select_values
chosen = rel.select(:id, "title")
p chosen.select_values
p chosen.select(:id).select_values
p chosen.select(:body).select_values
p rel.select_values
p chosen.unscope(:select).select_values
"#,
    );
    out.assert_passes();
    assert_eq!(
        out.stdout.lines().collect::<Vec<_>>(),
        ["[]", "[:id, \"title\"]", "[:id, \"title\"]", "[:id, \"title\", :body]", "[]", "[]"],
        "{}",
        out.stdout
    );
}
