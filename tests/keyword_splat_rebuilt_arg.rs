//! A `**value` argument whose value a lowering rebuilds keeps its splat.
//!
//! Core's `ServerIntents::References::Lowering#lower_reference` calls
//! `@factory.node(type, **declaration.except("field").symbolize_keys)`.
//! The shared keyword lowering erases the splat to a positional value and
//! marks it (`ERASED_KEYWORD_SPLAT`) for the Ruby family to write back.
//! The `symbolize_keys` grounding then rebuilt the value as
//! `ActiveSupport.symbolize_keys(…)` without the mark. The emitted call
//! passed the Hash positionally, and `node(type, nullable: false,
//! **options)` raised ArgumentError (given 2, expected 1) while the
//! module body ran: every sweep-1 test failed at boot.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;

const FACTORY: &str = r#"class SchemaFactory
  #: (String type, ?nullable: bool, **untyped) -> Hash[String, untyped]
  def node(type, nullable: false, **options)
    { "type" => type, "nullable" => nullable }.merge(options.transform_keys(&:to_s))
  end

  #: (String type, Hash[String, untyped] declaration) -> Hash[String, untyped]
  def lower(type, declaration)
    node(type, **declaration.except("field").symbolize_keys)
  end
end
"#;

fn emitted() -> String {
    let files: Vec<(&str, String)> = vec![("app/models/schema_factory.rb", FACTORY.into())];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.into_bytes())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    let class = app
        .library_classes
        .iter()
        .find(|c| c.name.0.as_str() == "SchemaFactory")
        .expect("SchemaFactory class");
    format!(
        "class SchemaFactory\n{}end\n",
        class.methods.iter().map(roundhouse::emit::ruby::emit_method).collect::<String>()
    )
}

#[test]
fn a_rebuilt_symbolize_keys_argument_is_still_a_keyword_splat() {
    let out = emitted();
    let lower = out
        .lines()
        .skip_while(|l| !l.contains("def lower"))
        .nth(1)
        .unwrap_or_else(|| panic!("no lower body in:\n{out}"));
    assert!(lower.contains("symbolize_keys"), "the call is still converted:\n{out}");
    assert!(lower.contains("node(type, **"), "the converted Hash is passed as keywords:\n{out}");
}
