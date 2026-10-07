//! An applied stdlib generic is the stdlib's, whatever the scope holds.
//!
//! Core declares `Metafields::Set` (a model) and, inside `module
//! Metafields`, annotates `#: -> Set[String]`. Sorbet, which Core's
//! annotations are checked by, reads that as `T::Set[String]`. Resolving
//! it lexically made the receiver a `Metafields::Set` instance, and
//! `exposed_attributes.include?(name)` was refused as Module#include?.
//! A bare `Set` with no type arguments is still a lexical reference.
//! Native Ruby on the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SHELF: &str = r#"module Shelf
  class Set
    def label = "shelf set"
  end

  module Deep
    class Owner
      #: -> Set[String]
      def tags = ::Set["vip", "gold"]

      #: (String) -> bool
      def tag?(name) = tags.include?(name)

      #: -> bool
      def seen?
        seen = ::Set.new #: Set[String]
        seen << "x"
        seen.include?("x")
      end

      #: -> Set
      def shelf = Shelf::Set.new

      #: -> String
      def shelf_label = shelf.label
    end
  end

  def self.report
    owner = Deep::Owner.new
    [owner.tag?("gold"), owner.tag?("new"), owner.seen?, owner.shelf_label]
  end
end
"#;

const REPORT: &str = "p Shelf.report\n";

#[test]
fn an_applied_set_inside_a_namespace_with_its_own_set_is_the_stdlib_set() {
    let dir = std::env::temp_dir().join(format!("roundhouse-shadowed-generic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("shelf.rb");
    std::fs::write(&file, SHELF).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require \"set\"\nrequire {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected, "[true, false, true, \"shelf set\"]\n", "native control");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/shelf.rb", SHELF)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"set\"\nrequire \"./app/models/shelf\"\nrequire \"./app/models/shelf/set\"\nrequire \"./app/models/shelf/deep/owner\"\n{REPORT}"
        ))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}
