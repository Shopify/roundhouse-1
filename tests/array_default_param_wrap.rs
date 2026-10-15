//! `Array.wrap(param)` where the parameter's only evidence is a `[]`
//! default.
//!
//! graphql-easy-testing's `on_mutation(selections: [])` hands its
//! argument to `mutation_query(selections: [])`, which writes
//! `Array.wrap(selections)`. Test bodies call `on_mutation` with a Hash
//! (`selections: { tax_document: [:id] }`) and are not call sites the
//! analyzer sees, so the parameter typed `Array` from the default alone,
//! the wrap folded to the bare value, and the Hash reached
//! `selections - scalar_selections` ("undefined method '-' for an
//! instance of Hash"). A parameter with only an `[]` default is open,
//! as one with a `nil` default is.

use roundhouse::det::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::{target_files, BuildTarget};

const MODEL: &str = r#"class Gen
  def on(args: {}, selections: [])
    query(args: args, selections: selections)
  end

  def query(args: {}, selections: [])
    Array.wrap(selections)
  end
end
"#;

fn emitted(target: BuildTarget) -> String {
    emitted_model(target, MODEL)
}

fn emitted_model(target: BuildTarget, model: &str) -> String {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    tree.insert(PathBuf::from("db/schema.rb"), b"ActiveRecord::Schema.define(version: 1) do\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/gen.rb"), model.as_bytes().to_vec());
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = target_files(&app, Path::new("fixtures/tiny-blog"), target).expect("emit");
    files
        .into_iter()
        .find(|(p, _)| p.ends_with("models/gen.rb"))
        .map(|(_, c)| c)
        .expect("gen.rb is emitted")
}

#[test]
fn a_hash_passed_for_an_empty_array_default_is_wrapped() {
    let src = emitted(BuildTarget::Ruby);
    // The call stays; a folded wrap would have left the bare parameter.
    assert!(src.contains("Array.wrap(selections)"), "{src}");
    // activesupport's core_ext/array/wrap.rb.
    let wrap = "class Array\n  def self.wrap(object)\n    if object.nil?\n      []\n    elsif object.respond_to?(:to_ary)\n      object.to_ary || [object]\n    else\n      [object]\n    end\n  end\nend\n";
    let script = format!(
        "{wrap}{src}\ng = Gen.new\n\
         p [g.on, g.on(selections: [:a]), g.on(selections: {{ doc: [:id] }}), g.on(selections: :id)]\n"
    );
    let output = Command::new("ruby").args(["-e", &script]).output().unwrap();
    assert!(
        output.status.success(),
        "{src}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "[[], [:a], [{doc: [:id]}], [:id]]",
        "{src}"
    );
}

/// The same, with the signature the gem writes (`selections: selection`,
/// an alias for a name, a Hash or an Array of them): the declared type
/// names a Hash, so it is no evidence of an Array either.
const SIGNED_MODEL: &str = r#"class Gen
  #: type selectionHash = Hash[Symbol, Array[Symbol]]
  #: type selection = Symbol | selectionHash | Array[Symbol | selectionHash]

  #: (?selections: selection) -> untyped
  def on(selections: [])
    query(selections: selections)
  end

  #: (?selections: selection) -> untyped
  def query(selections: [])
    Array.wrap(selections)
  end
end
"#;

#[test]
fn a_signed_union_alias_keeps_the_wrap() {
    let src = emitted_model(BuildTarget::Ruby, SIGNED_MODEL);
    assert!(
        src.contains("Array.wrap(selections)") || src.contains("is_a?(Array)"),
        "the wrap of a `selection` parameter was folded away:\n{src}"
    );
}

/// A second caller that passes an Array literal is one visible site; the
/// pass-through from the untyped `on` is the other. The parameter is
/// still open: `on`'s callers are not sites the analyzer sees.
const TWO_CALLERS_MODEL: &str = r#"class Gen
  def on(args: {}, selections: [])
    query(args: args, selections: selections)
  end

  def fixed
    query(selections: [:id])
  end

  def query(args: {}, selections: [])
    Array.wrap(selections)
  end
end
"#;

#[test]
fn an_array_literal_site_does_not_close_the_parameter() {
    let src = emitted_model(BuildTarget::Ruby, TWO_CALLERS_MODEL);
    assert!(src.contains("Array.wrap(selections)") || src.contains("is_a?(Array)"), "{src}");
}
