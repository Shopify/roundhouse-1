//! A block-form model callback becomes a hook only on the shapes
//! `markers::push_block_callback` lowers; every other one reports as
//! unlowered DSL instead of being dropped or spliced half-bound.

use std::path::Path;

use roundhouse::ingest::ingest_app;

fn copy_tree(src: &Path, dst: &Path) {
    if src.is_dir() {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if ["tmp", "log", "storage", "node_modules", ".git"].contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            copy_tree(&entry.path(), &dst.join(name));
        }
    } else {
        std::fs::copy(src, dst).unwrap();
    }
}

/// The diagnostics (any severity) and the lowered `Article` hook names
/// for real-blog's Article with `callback` added to its class body.
fn lower_with(tag: &str, callback: &str) -> (Vec<String>, bool) {
    let dir = std::env::temp_dir().join(format!("roundhouse-block-callback-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(roundhouse::fixtures::real_blog(), &dir);
    let model = dir.join("app/models/article.rb");
    let source = std::fs::read_to_string(&model).unwrap().replacen(
        "class Article < ApplicationRecord\n",
        &format!("class Article < ApplicationRecord\n{callback}\n"),
        1,
    );
    std::fs::write(&model, source).unwrap();
    let mut app = ingest_app(&dir).unwrap();
    let ((lowered, tree), pushed) = roundhouse::emit::diagnostics::scope(|| {
        let lowered = roundhouse::session::analyze_and_lower(&mut app);
        let tree = roundhouse::project::target_files(&app, &dir, roundhouse::project::BuildTarget::Ruby)
            .expect("ruby files");
        (lowered, tree)
    });
    // Unclaimed model DSL reports through `analyze::diagnose` (the `check`
    // and emit gate); a by-name refusal from `push_block_callback` is pushed
    // during emit.
    let gate = roundhouse::analyze::diagnose(&app);
    let messages =
        lowered.iter().chain(pushed.iter()).chain(gate.iter()).map(|d| d.message.clone()).collect();
    let article = tree
        .iter()
        .find(|(path, _)| path.ends_with("app/models/article.rb"))
        .map(|(_, content)| content.clone())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (messages, article.contains("def before_save"))
}

fn reports_unlowered(messages: &[String]) -> bool {
    messages.iter().any(|m| {
        m.contains("model DSL call on `Article` not lowered")
            || (m.contains("on `Article`") && m.contains("the callback is not lowered"))
    })
}

/// An `if:` naming a method lowers into a guard around the body.
#[test]
fn a_block_callback_with_a_symbol_if_option_lowers() {
    let (messages, hooked) = lower_with("if", "  before_save(if: :title?) { self.body = \"x\" }");
    assert!(hooked);
    assert!(!reports_unlowered(&messages), "{messages:#?}");
}

/// A proc condition is not modeled; the callback used to count as claimed
/// and vanish with no hook and no report.
#[test]
fn a_block_callback_with_a_proc_if_option_reports_instead_of_vanishing() {
    let (messages, hooked) = lower_with("if-proc", "  before_save(if: -> { title? }) { self.body = \"x\" }");
    assert!(!hooked);
    assert!(reports_unlowered(&messages), "{messages:#?}");
}

/// A default reading a local of the enclosing scope (`|key: prefix|`)
/// has no binding inside the generated hook, so the block is declined.
#[test]
fn a_block_default_reading_an_outer_local_is_declined() {
    let (messages, hooked) = lower_with(
        "outer-local",
        "  prefix = \"ok\"\n  before_save { |key: prefix| self.body = key }",
    );
    assert!(!hooked);
    assert!(reports_unlowered(&messages), "{messages:#?}");
}

/// A default reading an earlier parameter is bound first, so it lowers.
#[test]
fn a_block_default_reading_an_earlier_parameter_lowers() {
    let (messages, hooked) = lower_with(
        "earlier-param",
        "  before_save { |a: \"x\", b: a| self.body = \"#{body} #{b}\" }",
    );
    assert!(hooked);
    assert!(!reports_unlowered(&messages), "{messages:#?}");
}

/// Rails runs a one-parameter callback block as
/// `record.instance_exec(record, &blk)`, so its single parameter is the
/// record (tests/callback_block_record_param.rs runs it) and it lowers.
#[test]
fn a_single_block_parameter_binds_the_record_and_lowers() {
    let (messages, hooked) =
        lower_with("record-block", "  before_save { |key| self.body = key.title }");
    assert!(hooked);
    assert!(!reports_unlowered(&messages), "{messages:#?}");
}

/// Required keywords and rest parameters have no binding Rails' callback
/// passes, and a lambda argument with parameters is not bound by the
/// generated parameterless hook: each is declined or refused by name.
#[test]
fn callbacks_with_required_or_rest_parameters_are_declined() {
    for (tag, callback) in [
        ("required-keyword-block", "before_save { |key:| self.body = key }"),
        (
            "rest-block",
            "before_save { |*keys| self.body = keys.first }",
        ),
        ("required-lambda", "before_save ->(key) { self.body = key }"),
        (
            "rest-lambda",
            "before_save ->(*keys) { self.body = keys.first }",
        ),
    ] {
        let (messages, hooked) = lower_with(tag, &format!("  {callback}"));
        assert!(!hooked, "{tag} unexpectedly lowered to a hook");
        assert!(reports_unlowered(&messages), "{tag}: {messages:#?}");
    }
}

/// A method-reference block cannot be spliced into a generated hook; it
/// must remain unclaimed rather than silently disappearing.
#[test]
fn a_method_reference_callback_is_declined() {
    let (messages, hooked) = lower_with(
        "method-reference",
        "  before_save(&method(:callback))",
    );
    assert!(!hooked);
    assert!(reports_unlowered(&messages), "{messages:#?}");
}
