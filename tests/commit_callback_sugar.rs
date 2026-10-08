//! `after_create_commit :method` — the SYMBOL form of Rails' per-lifecycle
//! commit-callback sugar.
//!
//! Only the BLOCK form was ever recognized (`BLOCK_CALLBACK_HOOKS`); the
//! symbol form fell through to `Unknown`, so the declaration was DROPPED.
//! The target method still emitted, and nothing ever called it —
//! campfire's `User` never granted membership to the open rooms, and
//! `Message::Searchable` never wrote a row to its search index. Both
//! failed silently, which is what kept it hidden.
//!
//! Each spelling is `after_commit` with the `on:` Rails enforces for it
//! (`set_options_for_callbacks!`), one callback in the model's commit
//! chain (`lower::model_to_library::transaction_callbacks`).
//! `after_save_commit` is `on: [:create, :update]`: never on destroy.
//! The chains against native Rails: tests/transaction_callbacks_pg_runtime.rs.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

fn thing_src() -> String {
    let app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "things", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#,
        ),
        (
            "app/models/thing.rb",
            r#"class Thing < ApplicationRecord
  after_create_commit  :on_create
  after_update_commit  :on_update
  after_destroy_commit :on_destroy
  after_save_commit    :on_save

  def on_create; end
  def on_update; end
  def on_destroy; end
  def on_save; end
end
"#,
        ),
    ]))
    .expect("ingest callback app");
    let files = ruby::emit_lowered_models(&app);
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("thing.rb"))
        .map(|f| f.content.clone())
        .expect("thing.rb")
}

/// The commit chain, which runs from its last callback to its first:
/// each spelling guarded by the action it enforces.
#[test]
fn each_sugar_spelling_enforces_its_own_action() {
    let src = thing_src();
    let chain = "  def _run_commit_callbacks\n    on_save if self._transaction_include_create? || self._transaction_include_update?\n    on_destroy if self._transaction_include_destroy?\n    on_update if self._transaction_include_update?\n    on_create if self._transaction_include_create?\n  end\n";
    assert!(src.contains(chain), "expected the commit chain:\n{src}");
}

/// No legacy per-lifecycle hook override is left behind.
#[test]
fn the_sugar_overrides_no_runtime_hook() {
    let src = thing_src();
    for hook in ["after_commit", "after_create_commit", "after_update_commit", "after_destroy_commit", "after_save_commit"] {
        assert!(!src.contains(&format!("def {hook}\n")), "no `{hook}` override:\n{src}");
    }
}

fn lambda_thing_src(decl: &str) -> String {
    let model = format!(
        "class Thing < ApplicationRecord\n  {decl}\n\n  def on_create; end\nend\n"
    );
    let app = ingest_app_from_tree(tree(&[
        (
            "db/schema.rb",
            r#"ActiveRecord::Schema.define do
  create_table "things", force: :cascade do |t|
    t.string "name", null: false
  end
end
"#,
        ),
        ("app/models/thing.rb", &model),
    ]))
    .expect("ingest callback app");
    let files = ruby::emit_lowered_models(&app);
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("thing.rb"))
        .map(|f| f.content.clone())
        .expect("thing.rb")
}

/// Rails means the same thing by a BLOCK and a zero-arity LAMBDA
/// ARGUMENT, and campfire's `Message` writes the second
/// (`after_create_commit -> { room.receive(self) }`): the body becomes a
/// method of the model, which the chain calls on create.
#[test]
fn a_zero_arity_lambda_argument_is_the_block_form() {
    let src = lambda_thing_src("after_create_commit -> { on_create }");
    assert!(src.contains("  def _thing_transaction_callback_0\n    on_create\n  end\n"), "the lambda body:\n{src}");
    assert!(
        src.contains("  def _run_commit_callbacks\n    self._thing_transaction_callback_0 if self._transaction_include_create?\n  end\n"),
        "the chain calls it on create:\n{src}"
    );
}

/// A one-parameter lambda is `instance_exec(record)` in
/// ActiveSupport::Callbacks: `self` and the parameter are both the
/// record (checked natively in tests/transaction_callbacks_pg_runtime.rs).
#[test]
fn a_lambda_parameter_is_the_record() {
    let src = lambda_thing_src("after_create_commit ->(record) { record.on_create }");
    assert!(src.contains("  def _thing_transaction_callback_0\n    self.on_create\n  end\n"), "the record is self:\n{src}");
}

/// A block with two parameters (`InstanceExec2`, the record and a block)
/// is not lowered, and stays reported.
#[test]
fn a_two_parameter_lambda_declines() {
    let src = lambda_thing_src("after_create_commit ->(record, blk) { record.on_create }");
    assert!(!src.contains("_run_commit_callbacks"), "a two-parameter lambda must not lower:\n{src}");
}
