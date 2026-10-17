//! Rails 8.1.4's non-mutating, no-block Hash#deep_merge pilot.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"class HashDeepMergeProbe
  def self.run
    left = {
      nested: { keep: "left", conflict: "left", subtree: { old: true } },
      array: [1, 2],
      hash_to_scalar: { old: true },
      scalar_to_hash: "old",
      left_only: :left
    }
    right = {
      nested: { conflict: "right", added: "right", subtree: { new: true } },
      array: [3],
      hash_to_scalar: "right",
      scalar_to_hash: { added: true },
      right_only: :right
    }
    merged = left.deep_merge(right)
    expected = {
      nested: { keep: "left", conflict: "right", added: "right", subtree: { old: true, new: true } },
      array: [3],
      hash_to_scalar: "right",
      scalar_to_hash: { added: true },
      left_only: :left,
      right_only: :right
    }
    raise "wrong merged value: #{merged.inspect}" unless merged == expected
    raise "left input mutated" unless left[:nested][:conflict] == "left" && left[:array] == [1, 2]
    raise "right input mutated" unless right[:nested][:conflict] == "right" && right[:array] == [3]
    raise "empty-right merge changed content" unless left.deep_merge({}) == left
    raise "empty-left merge changed content" unless ({}).deep_merge(right) == right
    raise "empty merge changed content" unless ({}).deep_merge({}) == {}
    raise "merge returned receiver" if left.deep_merge(right).equal?(left)
    defaulted_left = Hash.new({ nested: { default: true } })
    defaulted_right = { missing: { nested: { right: true } } }
    raise "merged an absent key's default" unless defaulted_left.deep_merge(defaulted_right) == defaulted_right
    puts "Hash#deep_merge caller contract passed"
  end
end
"#;

const ASSERTIONS: &str = r#"HashDeepMergeProbe.run
"#;

#[test]
fn emitted_cruby_caller_observes_deep_merge_contract() {
    let run = emit_and_run::real_blog()
        .write("app/models/hash_deep_merge_probe.rb", SOURCE)
        .run_ruby(ASSERTIONS);
    run.assert_passes();
    assert!(
        run.stdout
            .contains("Hash#deep_merge caller contract passed")
    );
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/hash_deep_merge_probe.rb"))
        .expect("emitted HashDeepMergeProbe");
    assert!(
        emitted.contains("ActiveSupport.deep_merge(left, right)"),
        "{emitted}"
    );
    assert!(!emitted.contains("left.deep_merge(right)"), "{emitted}");
}

#[test]
#[ignore = "requires the Spinel toolchain"]
fn emitted_spinel_caller_observes_deep_merge_contract_natively() {
    let run = emit_and_run::real_blog()
        .write("app/models/hash_deep_merge_probe.rb", SOURCE)
        .run_spinel(ASSERTIONS);
    run.assert_passes();
    assert!(
        run.stdout
            .contains("Hash#deep_merge caller contract passed")
    );
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/hash_deep_merge_probe.rb"))
        .expect("emitted HashDeepMergeProbe");
    assert!(
        emitted.contains("ActiveSupport.deep_merge(left, right)"),
        "{emitted}"
    );
    assert!(!emitted.contains("left.deep_merge(right)"), "{emitted}");
}

const BLOCK_AND_BANG: &str = r#"class HashDeepMergeForms
  def self.run
    left = { a: { b: 1, c: 2 }, n: 1, keep: [1] }
    right = { a: { b: 3 }, n: 4, keep: [2] }
    seen = []
    summed = left.deep_merge(right) { |key, old, new| seen << key; old + new }
    raise "wrong block merge: #{summed.inspect}" unless summed == { a: { b: 4, c: 2 }, n: 5, keep: [1, 2] }
    raise "block saw #{seen.inspect}" unless seen == [:b, :n, :keep]
    raise "block merge mutated the receiver" unless left == { a: { b: 1, c: 2 }, n: 1, keep: [1] }
    target = { a: { b: 1 } }
    answered = target.deep_merge!({ a: { c: 2 }, d: 3 })
    raise "wrong bang merge: #{target.inspect}" unless target == { a: { b: 1, c: 2 }, d: 3 }
    raise "bang answered a copy" unless answered.equal?(target)
    nested = { a: { b: { c: 1 } } }
    nested.deep_merge!({ a: { b: { c: 2 } } }) { |_key, old, new| old + new }
    raise "wrong nested bang block merge: #{nested.inspect}" unless nested == { a: { b: { c: 3 } } }
    puts "Hash#deep_merge block and bang contract passed"
  end
end
"#;

#[test]
fn deep_merge_block_and_bang_forms_run_like_rails() {
    let run = emit_and_run::real_blog()
        .write("app/models/hash_deep_merge_forms.rb", BLOCK_AND_BANG)
        .run_ruby("HashDeepMergeForms.run\n");
    run.assert_passes();
    assert!(run.stdout.contains("Hash#deep_merge block and bang contract passed"), "{}", run.stdout);
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/hash_deep_merge_forms.rb"))
        .expect("emitted HashDeepMergeForms");
    assert!(emitted.contains("ActiveSupport.deep_merge!(target,"), "{emitted}");
    assert!(emitted.contains("ActiveSupport.deep_merge(left, right)"), "{emitted}");
}

fn deep_merge_errors(source: &str) -> Vec<String> {
    let (_, _, diagnostics) = emit_and_run::real_blog()
        .write("app/models/hash_deep_merge_unsupported.rb", source)
        .emit_with_app(roundhouse::project::BuildTarget::Ruby);
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == roundhouse::diagnostic::Severity::Error)
        .filter_map(|diagnostic| match &diagnostic.kind {
            roundhouse::diagnostic::DiagnosticKind::Unsupported { construct, .. }
                if construct.as_str().starts_with("Hash#deep_merge") =>
            {
                Some(construct.to_string())
            }
            _ => None,
        })
        .collect()
}

/// An argument known not to be a Hash is refused by name; one whose type is
/// not known (an untyped parameter) is left to dispatch at run time.
#[test]
fn deep_merge_refuses_only_an_argument_known_not_to_be_a_hash() {
    let untyped = "class HashDeepMergeUnsupported\n  def self.run(other)\n    left = { key: 1 }\n    left.deep_merge!(other)\n  end\nend\n";
    assert!(deep_merge_errors(untyped).is_empty());
    let string = "class HashDeepMergeUnsupported\n  def self.run\n    left = { key: 1 }\n    left.deep_merge!(\"x\")\n  end\nend\n";
    assert_eq!(deep_merge_errors(string), ["Hash#deep_merge argument"]);
}

#[test]
fn deep_merge_with_an_untyped_argument_runs_like_rails() {
    let run = emit_and_run::real_blog()
        .write(
            "app/models/hash_deep_merge_untyped.rb",
            "class HashDeepMergeUntyped\n  def self.run(other)\n    left = { a: { b: 1 }, c: 1 }\n    left.deep_merge(other)\n  end\nend\n",
        )
        .run_ruby("p HashDeepMergeUntyped.run({ a: { d: 2 }, c: 3 })\n");
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "{a: {b: 1, d: 2}, c: 3}", "{}", run.stdout);
}
