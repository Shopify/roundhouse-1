//! A block-form lifecycle hook that names its argument:
//! `before_save { |record| record.title = record.title.to_s.strip }`.
//! Rails runs a one-argument callback proc as
//! `record.instance_exec(record, &blk)`, so the record is both `self`
//! and the argument. The block body was spliced into the hook method
//! with the parameter left unbound, and the first save died on
//! `undefined local variable or method 'record'`.
//!
//! The expected values are native Active Record's on PostgreSQL for the
//! same model and script (`before_save` blocks, `after_create` block,
//! `create!` then `find`):
//! "hi", "created hi", "hi", "a long enough body same".

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const HOOKS: &str = concat!(
    "  before_save { |record| record.title = record.title.to_s.strip }\n",
    "  before_save { |article| self.body = body + (article.equal?(self) ? \" same\" : \" other\") }\n",
    "  after_create { |r| r.body = \"created #{r.title}\" }\n",
    "\n",
    "  validates :title, presence: true\n",
);

#[test]
fn a_hook_block_argument_is_the_record_itself() {
    let run = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true\n",
            HOOKS,
        )
        .run_ruby(concat!(
            "a = Article.create!(title: \"  hi  \", body: \"a long enough body\")\n",
            "p a.title, a.body\n",
            "b = Article.find(a.id)\n",
            "p b.title, b.body\n",
        ));
    run.assert_passes();
    assert_eq!(
        run.stdout.trim_end(),
        "\"hi\"\n\"created hi\"\n\"hi\"\n\"a long enough body same\""
    );
}

/// Two positional parameters have no Rails meaning (native Active
/// Record raises ArgumentError on the call); refused by name rather
/// than spliced with an unbound name.
#[test]
fn a_hook_block_with_two_parameters_is_refused_by_name() {
    let (_emitted, errors) = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "  validates :title, presence: true\n",
            "  before_save { |record, other| record }\n\n  validates :title, presence: true\n",
        )
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("block_callback_params")
            && e.contains("`before_save { |…| … }` on `Article`")),
        "{errors:#?}"
    );
}
