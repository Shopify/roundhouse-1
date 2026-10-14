//! A bare `return` guard inside an `unless` / `if` arm.
//!
//! graphql-easy-testing's `validate_selections!` opens with
//!
//!     unless parent_type.is_a?(HasFields)
//!       return if selections.empty?
//!       raise ArgumentError, "..."
//!     end
//!
//! so an enum parent (no fields) with no selections leaves the method.
//! Ingest rewrote a block that starts with `return if c` as
//! `if c then nil else <rest> end`, which holds only when the block is
//! the whole method body. In an arm, the code after the arm still ran:
//! `parent_type.fields` raised "undefined method 'fields' for class
//! ...RedactionState".

use roundhouse::det::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::{target_files, BuildTarget};

const MODEL: &str = r#"class Gen
  def unless_arm(selections, kind)
    unless kind.is_a?(Integer)
      return if selections.empty?

      raise ArgumentError, "bad"
    end

    selections.map { |s| s.to_s }
  end

  def if_arm(selections, kind)
    if kind.is_a?(Integer)
      selections.size
    else
      return if selections.empty?

      raise ArgumentError, "bad"
    end

    selections.map { |s| s.to_s }
  end

  def when_arm(selections, kind)
    case kind
    when 1
      return if selections.empty?

      raise ArgumentError, "bad"
    end

    selections.map { |s| s.to_s }
  end

  def whole_body(selections)
    return if selections.empty?

    selections.map { |s| s.to_s }
  end
end
"#;

fn emitted(target: BuildTarget) -> String {
    let mut tree: HashMap<PathBuf, Vec<u8>> = HashMap::default();
    tree.insert(PathBuf::from("db/schema.rb"), b"ActiveRecord::Schema.define(version: 1) do\nend\n".to_vec());
    tree.insert(PathBuf::from("app/models/gen.rb"), MODEL.as_bytes().to_vec());
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
fn a_guard_in_an_arm_leaves_the_method() {
    let src = emitted(BuildTarget::Ruby);
    let script = format!(
        "{src}\ng = Gen.new\n\
         p [g.unless_arm([], \"enum\"), (g.unless_arm([:a], \"enum\") rescue $!.class), g.unless_arm([:a], 1)]\n\
         p [g.if_arm([], \"enum\"), (g.if_arm([:a], \"enum\") rescue $!.class), g.if_arm([:a], 1)]\n\
         p [g.when_arm([], 1), (g.when_arm([:a], 1) rescue $!.class), g.when_arm([:a], 2)]\n\
         p [g.whole_body([]), g.whole_body([:a])]\n"
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
        "[nil, ArgumentError, [\"a\"]]\n[nil, ArgumentError, [\"a\"]]\n[nil, ArgumentError, [\"a\"]]\n[nil, [\"a\"]]",
        "{src}"
    );
}
