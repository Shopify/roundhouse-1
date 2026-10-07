//! The block a `super` passes is part of the call. Ruby's `super` without
//! one hands on the method's own block, so a compiler that drops a
//! written block binds a different one, silently. Each case runs on CRuby
//! as written and then through the Ruby tree, and the outputs must agree.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

fn native(source: &str, script: &str) -> String {
    let output = Command::new("ruby").args(["-e", &format!("{source}\n{script}")]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every block shape a `super` can be written with, each distinguishable
/// from the method's own block (`v - 1`) by its result.
const CHILD_BODY: &str = r#"
    other = proc { |v| v * 7 }
    [
      super { |v| v + 10 },
      super do |v| v * 100 end,
      super(x) { |v| v + 20 },
      super(x, &blk),
      super(x, &other),
      super(&nil),
      super,
      super(x),
    ]
"#;

const EXPECTED: &str = "[12, 200, 22, 1, 14, :none, 1, 1]\n";

#[test]
fn super_blocks_in_a_library_class_bind_as_written() {
    let source = format!(
        "class BlockParent\n  def run(x = 1)\n    block_given? ? yield(x) : :none\n  end\n  def anon(x)\n    block_given? ? yield(x) : :none\n  end\nend\n\
         class BlockChild < BlockParent\n  def run(x = 1, &blk)\n{CHILD_BODY}  end\n  def anon(x, &)\n    super(x + 1, &)\n  end\nend\n"
    );
    let script = "p BlockChild.new.run(2) { |v| v - 1 }; p BlockChild.new.anon(4) { |v| v * 2 }; p BlockChild.new.anon(4)";
    let expected = format!("{EXPECTED}10\n:none\n");
    assert_eq!(native(&source, script), expected);
    let run = emit_and_run::real_blog().write("app/lib/block_child.rb", &source).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

#[test]
fn super_blocks_in_a_model_bind_as_written() {
    let parent = "  def probe_yield(x = 1)\n    block_given? ? yield(x) : :none\n  end\n";
    let child = format!("  def probe_yield(x = 1, &blk)\n{CHILD_BODY}  end\n");
    let script = "p Article.new.probe_yield(2) { |v| v - 1 }";
    let native_source = format!(
        "class Base0\n{parent}end\nclass Article < Base0\n{child}end\n"
    );
    assert_eq!(native(&native_source, script), EXPECTED);
    let run = emit_and_run::real_blog()
        .edit("app/models/application_record.rb", "  primary_abstract_class\n",
            &format!("  primary_abstract_class\n\n{parent}"))
        .edit("app/models/article.rb", "  validates :title, presence: true\n",
            &format!("  validates :title, presence: true\n\n{child}\n"))
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}

#[test]
fn a_target_that_cannot_write_a_super_block_refuses_it_by_name() {
    let source = "class BlockParent\n  def run(x)\n    yield(x)\n  end\nend\nclass BlockChild < BlockParent\n  def run(x)\n    super(x) { |v| v + 1 }\n  end\nend\n";
    let (_, errors) = emit_and_run::real_blog()
        .write("app/lib/block_child.rb", source)
        .emit(roundhouse::project::BuildTarget::Typescript);
    assert!(errors.iter().any(|e| e.contains("super with a block")), "{errors:?}");
    // A super with no written block is not this refusal.
    let source = source.replace("super(x) { |v| v + 1 }", "super(x)");
    let (_, errors) = emit_and_run::real_blog()
        .write("app/lib/block_child.rb", &source)
        .emit(roundhouse::project::BuildTarget::Typescript);
    assert!(!errors.iter().any(|e| e.contains("super with a block")), "{errors:?}");
}
