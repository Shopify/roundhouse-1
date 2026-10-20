//! An abstract module still declares its methods in emitted Ruby.
//!
//! Core's `AccessAndAuth::AccessPolicyHelper#validate_actions!` reads the
//! actions a policy module supports off `abstract_policy.instance_methods(false)`:
//!
//! ```ruby
//! module CustomersAccessPolicy
//!   extend T::Helpers
//!   abstract!
//!
//!   sig { abstract.returns(T::Boolean) }
//!   def create_and_edit?; end
//! end
//! ```
//!
//! The slicer drops an empty `def` under `sig { abstract… }` from the IR (so a
//! concern spliced into a controller does not shadow the inherited `params`
//! with nil), which left the emitted module empty: "Unknown actions
//! [:create_and_edit?], must be one of []". The Ruby emit writes the
//! declaration back as a stub that defers up the ancestor chain.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

fn emitted(files: &[(&str, &str)]) -> String {
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_library(&app)
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "rb"))
        .map(|f| format!("# {}\n{}", f.path.display(), f.content))
        .collect::<Vec<_>>()
        .join("\n")
}

const POLICY: &str = "module AccessPolicy\n  extend T::Helpers\n  abstract!\n\n  sig { abstract.returns(T::Boolean) }\n  def create_and_edit?; end\n\n  sig { abstract.returns(T::Boolean) }\n  def view?; end\n\n  class << self\n    def for(user)\n      user\n    end\n  end\nend\n";

#[test]
fn an_abstract_module_lists_its_methods_in_the_emit() {
    let out = emitted(&[("app/models/access_policy.rb", POLICY)]);
    assert!(out.contains("def create_and_edit?"), "abstract method not declared:\n{out}");
    assert!(out.contains("def view?"), "abstract method not declared:\n{out}");
    assert!(out.contains("if defined?(super)\n      super\n    else\n      raise(NotImplementedError"), "stub does not defer:\n{out}");
}

#[test]
fn a_module_that_only_declares_abstract_methods_still_emits_them() {
    let out = emitted(&[(
        "app/models/access_policy.rb",
        "module AccessPolicy\n  extend T::Helpers\n  interface!\n\n  sig { abstract.returns(T::Boolean) }\n  def view?; end\nend\n",
    )]);
    assert!(out.contains("def view?"), "interface module lost its declaration:\n{out}");
}

#[test]
fn an_implemented_method_is_not_replaced_by_a_stub() {
    let out = emitted(&[(
        "app/models/access_policy.rb",
        "module AccessPolicy\n  extend T::Helpers\n  abstract!\n\n  sig { abstract.returns(T::Boolean) }\n  def view?; end\n\n  def helper\n    1\n  end\nend\n",
    )]);
    assert_eq!(out.matches("def view?").count(), 1, "{out}");
    assert!(out.contains("def helper"), "{out}");
}
