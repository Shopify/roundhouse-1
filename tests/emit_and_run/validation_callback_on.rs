use super::emit_and_run;

/// `before_validation :m, on: :create` / `after_validation :m, on: :update`
/// guard the callback by the record's state. The guard used to be a bare
/// `new_record?`, which the strict targets spelled as a call the model
/// can't make: rust `new_record_pred()` (E0425, cannot find function) and
/// go `self.NewRecord` (no such field), since `new_record?` lives on the
/// runtime Base that a rust or go struct doesn't inherit. It is now
/// `self.persisted?`, synthesized per model on every target.
fn app() -> emit_and_run::Overlay {
    emit_and_run::real_blog().edit(
        "app/models/article.rb",
        "class Article < ApplicationRecord\n",
        "class Article < ApplicationRecord\n  before_validation :mark_created, on: :create\n  after_validation :mark_updated, on: :update\n\n  def mark_created\n    @seen = \"create\"\n  end\n\n  def mark_updated\n    @seen = \"update\"\n  end\n\n  def seen\n    @seen\n  end\n",
    )
}

#[test]
fn strict_targets_guard_on_the_models_own_persisted() {
    let (rust, _) = app().emit(roundhouse::project::BuildTarget::Rust);
    let model = std::fs::read_to_string(rust.join("src/models/article.rs")).unwrap();
    assert!(!model.contains("if new_record_pred()") && !model.contains("!(new_record_pred())"), "bare new_record_pred() in:\n{model}");
    assert!(model.contains("self.persisted_pred()"), "{model}");

    let (go, _) = app().emit(roundhouse::project::BuildTarget::Go);
    let model = std::fs::read_to_string(go.join("app/v2/article.go")).unwrap();
    assert!(!model.contains("self.NewRecord {") && !model.contains("!(self.NewRecord)"), "self.NewRecord in:\n{model}");
}

/// Rails 8.1.4: saving a new record runs only the `on: :create` callback,
/// and saving it again runs only the `on: :update` one.
#[test]
fn validation_callbacks_run_in_their_context() {
    app()
        .run_ruby(
            r#"a = Article.new(title: "Hello", body: "A body long enough to pass validation.")
a.save!
created = a.seen
a.title = "Hi"
a.save!
raise "seen: #{[created, a.seen].inspect}" unless [created, a.seen] == ["create", "update"]
puts "validation callback contexts passed"
"#,
        )
        .assert_passes();
}
