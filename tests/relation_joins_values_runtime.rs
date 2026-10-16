//! `Relation#joins_values`, which Core's `GeneralizedBatchedFinder::Cursor`
//! reads (`ActiveSupport.present?(relation.joins_values)`) to require
//! fully-qualified cursor columns once the relation joins a table. The
//! relation answered `undefined method 'joins_values' for
//! ActiveRecord::Relation` (JobIteration ids, e2e v2-r37).
//!
//! Rails: the specs `joins` / `left_outer_joins` were given as written, in
//! order, each once; `unscope(:joins)` and a relation that never joined
//! answer `[]`. A spawned relation keeps its parent's list and does not
//! write back.
//!
//! Skips when `ruby` with the sqlite3 gem is not on the machine.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

#[test]
fn joins_values_lists_the_join_specs_as_written() {
    if !Command::new("ruby").args(["-rsqlite3", "-e", "1"]).status().is_ok_and(|s| s.success()) {
        eprintln!("skipping: ruby with the sqlite3 gem not available");
        return;
    }
    let out = emit_and_run::real_blog().run_ruby(
        r#"
rel = ActiveRecord::Relation.new(Article)
p rel.joins_values
p rel.left_outer_joins_values
joined = rel.joins("INNER JOIN comments ON comments.article_id = articles.id")
p joined.joins_values
p joined.joins("INNER JOIN comments ON comments.article_id = articles.id").joins_values
p joined.joins("INNER JOIN x ON x.id = articles.id").joins_values
p joined.left_outer_joins("LEFT OUTER JOIN tags ON tags.article_id = articles.id").left_outer_joins_values
p rel.joins_values
p joined.unscope(:joins).joins_values
"#,
    );
    out.assert_passes();
    assert_eq!(
        out.stdout.lines().collect::<Vec<_>>(),
        [
            "[]",
            "[]",
            "[\"INNER JOIN comments ON comments.article_id = articles.id\"]",
            "[\"INNER JOIN comments ON comments.article_id = articles.id\"]",
            "[\"INNER JOIN comments ON comments.article_id = articles.id\", \"INNER JOIN x ON x.id = articles.id\"]",
            "[\"LEFT OUTER JOIN tags ON tags.article_id = articles.id\"]",
            "[]",
            "[]",
        ],
        "{}",
        out.stdout
    );
}
