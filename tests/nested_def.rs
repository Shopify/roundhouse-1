//! A nested `def` (in a block or a method body) used to ingest as a bare nil,
//! so `Class.new(Parent) { private def m = ... }` emitted as
//! `{ private nil }` with no diagnostic: a class missing the method it
//! was made for. Upstream made the site a refusal; this branch carries the
//! def as a `MethodDefExpr`, so the site is written back as the def itself.
//! Either way the def must not vanish silently: no `private nil`, and no
//! diagnostic-free emit of a class missing its method.

use roundhouse::App;
use roundhouse::analyze::{Analyzer, Severity, diagnose};
use roundhouse::emit::ruby::emit_library;
use roundhouse::ingest::ingest_library_classes;

#[test]
fn a_def_inside_a_block_is_emitted_not_dropped() {
    let source = r#"
class Util
  def build
    Class.new(StandardError) { private def detail = "x" }
  end
end
"#;
    let classes = ingest_library_classes(source.as_bytes(), "test.rb").expect("ingest test source");
    let mut app = App::new();
    app.library_classes.extend(classes);
    Analyzer::new(&app).analyze(&mut app);

    let errors: Vec<_> = diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "a carried nested def is not an error: {errors:?}");

    let out = emit_library(&app)
        .into_iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!out.contains("private nil"), "the def must not vanish silently:\n{out}");
    assert!(
        out.contains("private def detail"),
        "the nested def should be written back as the def:\n{out}"
    );
}
