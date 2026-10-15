//! A class body keeps a lambda in a local and passes it on: Core's
//! `tax_document_post_resolve_access = lambda do |document, ctx| … end`
//! given to `post_resolve_access(&tax_document_post_resolve_access)`,
//! `string = -> { StringFilterInput }` given to `field :body, string`, and
//! `tags = lambda { |_, args| … }` given to `statsd_measure …, tags: tags`.
//! Ingest refused every read other than `.call`'s receiver ("the constants
//! reading it cannot keep its scope") and left the whole file's classes
//! out of the emitted app with only a warning, so the nested class the
//! payload field named, `GraphApi::Admin::TaxDocument`, was an
//! uninitialized constant (`MissingReturnTypeError`).
//!
//! A lambda that reads no `self` is the same code wherever its literal
//! stands, so the read is the literal.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const REGISTRY: &str = r#"class Registry
  def self.entries
    @entries ||= []
  end

  def self.field(name, type, required: false)
    entries << [name, type, required]
  end

  def self.access(&block)
    entries << [:access, block]
  end
end
"#;

const FILTERS: &str = r#"class Filters < Registry
  class StringFilter; end

  string = -> { StringFilter }
  check = lambda do |document, ctx|
    next true if ctx[:admin]
    document.start_with?("ok")
  end

  field :body, string, required: false
  field :handle, string, required: true
  access(&check)
end
"#;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app().write("app/models/registry.rb", REGISTRY).write("app/models/filters.rb", FILTERS)
}

#[test]
fn a_self_free_lambda_local_read_as_a_value_keeps_the_class() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("class-body lambda")), "{errors:#?}");
    let filters = std::fs::read_to_string(dir.join("app/models/filters.rb")).expect("emitted filters.rb");
    assert!(filters.contains("class Filters"), "{filters}");
    let run = app().run_ruby(
        "entries = Filters.entries\n\
         p entries.map(&:first)\n\
         p entries[0][1].call\n\
         p entries.last[1].call(\"okay\", {}), entries.last[1].call(\"no\", {}), entries.last[1].call(\"no\", { admin: true })\n",
    );
    run.assert_passes();
    assert_eq!(run.stdout, "[:body, :handle, :access]\nFilters::StringFilter\ntrue\nfalse\ntrue\n");
}
