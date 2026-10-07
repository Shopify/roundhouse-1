//! The CRuby tree puts the verbatim library emit back over an extras
//! façade (`emit::ruby::restore_extras_facades`). That restore re-renders
//! the library, and it returns early when the tree carries no file at a
//! façade path, because then nothing could be overwritten. Both halves:
//! the façade swapped by the scaffold base comes back byte for byte, and
//! a tree without one is left exactly as it was.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby::{apply_extras_facades, emit_library, restore_extras_facades};
use roundhouse::ingest::ingest_app_from_tree;

const SCHEMA: &[u8] = b"ActiveRecord::Schema.define do\n  create_table \"posts\", force: :cascade do |t|\n    t.string \"body\", null: false\n  end\nend\n";

const SPONGE: &str = r#"class Sponge
  def fetch(host)
    host.to_s
  end
end
"#;

const PLAIN: &str = r#"class Plain
  def name
    "plain"
  end
end
"#;

fn library_files(extras: &[(&str, &str)]) -> (roundhouse::App, Vec<(String, String)>) {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    tree.insert(PathBuf::from("db/schema.rb"), SCHEMA.to_vec());
    for (path, body) in extras {
        tree.insert(PathBuf::from(path), body.as_bytes().to_vec());
    }
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = emit_library(&app)
        .into_iter()
        .map(|f| (f.path.to_string_lossy().into_owned(), f.content))
        .collect();
    (app, files)
}

#[test]
fn a_swapped_facade_comes_back_verbatim() {
    let (app, verbatim) = library_files(&[("extras/sponge.rb", SPONGE)]);
    let source = verbatim
        .iter()
        .find(|(p, _)| p == "app/models/sponge.rb")
        .map(|(_, c)| c.clone())
        .expect("sponge.rb among the library emit");
    let mut files = verbatim.clone();
    apply_extras_facades(&mut files, &app);
    let swapped = files.iter().find(|(p, _)| p == "app/models/sponge.rb").unwrap();
    assert_ne!(swapped.1, source, "the scaffold base swaps in the façade");
    restore_extras_facades(&mut files, &app);
    let restored = files.iter().find(|(p, _)| p == "app/models/sponge.rb").unwrap();
    assert_eq!(restored.1, source);
}

#[test]
fn a_tree_without_a_facade_path_is_left_as_it_was() {
    let (app, files) = library_files(&[("extras/plain.rb", PLAIN)]);
    assert!(files.iter().any(|(p, _)| p == "app/models/plain.rb"));
    let mut after = files.clone();
    restore_extras_facades(&mut after, &app);
    assert_eq!(after, files);
}
