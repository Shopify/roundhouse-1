//! Optional, keyword and named keyword-rest block parameters.
//!
//! Lambda IR held required, rest, post and block parameters only, so
//! Core's `lambda do |item:, context: 1| … end` emitted as
//! `lambda do … end` and `proc { |x, k: 3| }` as `proc { |x| }`, with
//! bodies reading names nothing bound. They are now in
//! `Lambda.signature`. The Ruby family writes it verbatim, so each block
//! below runs as native Ruby runs it; every other target refuses a
//! block that has one, by name, before it emits.
//!
//! What the IR still cannot hold is refused at the block, as before:
//! optional parameters ahead of post parameters (posts are desugared
//! by popping from the rest), whose body raises `NotImplementedError`
//! when called, with an error diagnostic naming them.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::PathBuf;

use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::project::BuildTarget;

const PROBE: &str = r##"module Blockparamlab
  class Probe
    GREETINGS = %i[greet].freeze

    instance_eval do
      GREETINGS.each do |method_name|
        define_method(method_name) do |name:, greeting: "hi", **rest|
          [method_name, greeting, name, rest.keys]
        end
      end
    end

    def self.keywords
      f = lambda do |item:, context: 1|
        [item, context]
      end
      [f.call(item: 2), f.call(item: 2, context: 5)]
    end

    def self.optional
      base = 10
      [
        [1].map { |x, k = 3| x + k },
        [1].map { |x, k = base| x + k },
        [[1, 2]].map { |x, k = 3| [x, k] },
        ->(a, b = a * 2) { [a, b] }.call(3),
        ->(a, b = a * 2) { [a, b] }.call(3, 4),
      ]
    end

    def self.keyword_rest
      [
        proc { |x, **opts| [x, opts.keys, opts.values] }.call(1, a: 2),
        proc { |x, **opts| [x, opts.size] }.call(1),
      ]
    end

    def self.stabby
      add = ->(x, k: 3) { x + k }
      [add.call(1), add.call(1, k: 4), ->(params: {}) { params.size }.call(params: { a: 1 })]
    end

    def self.everything
      ->(a, b = 2, *r, k:, j: 5, **o, &blk) { [a, b, r, k, j, o.keys, blk.call] }
        .call(1, 3, 4, k: 6, z: 7) { :blk }
    end

    def self.defined_method
      [new.greet(name: "a"), new.greet(name: "b", greeting: "yo", x: 1)]
    end

    def self.representable
      [
        [[1, 2], 3].each_slice(2).map { |(a, b), *rest| [a, b, rest] },
        [4].map { |x, **| x },
        ->(*, payload) { payload }.call(5, 6),
      ]
    end
  end
end
"##;

const OBSERVE: &str = "%i[keywords optional keyword_rest stabby everything defined_method representable].each do |m|\n  \
     puts \"#{m}: #{Blockparamlab::Probe.public_send(m).inspect}\"\n\
     end\n";

fn native(program: &str) -> String {
    let out = emit_and_run::ruby()
        .arg("-e")
        .arg(program)
        .output()
        .expect("spawn ruby");
    assert!(
        out.status.success(),
        "native failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn optional_keyword_and_kwrest_block_parameters_run_as_native() {
    let expected = native(&format!("{PROBE}\n{OBSERVE}"));
    let run = emit_and_run::real_blog()
        .write("app/lib/blockparamlab/probe.rb", PROBE)
        .run_ruby(&format!(
            "require_relative \"app/models/blockparamlab/probe\"\n{OBSERVE}"
        ));
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    let source = std::fs::read_to_string(run.emitted.join("app/models/blockparamlab/probe.rb"))
        .expect("emitted probe");
    for written in [
        "|item:, context: 1|",
        "|x, k = 3|",
        "|x, **opts|",
        "->(x, k: 3)",
        "def greet(name:, greeting: \"hi\", **rest)",
    ] {
        assert!(
            source.contains(written),
            "{written} not written verbatim:\n{source}"
        );
    }
}

const POSTS: &str = r##"module Blockparamlab
  class Posts
    def self.optional_before_post
      ->(a, b = 1, *r, c) { [a, b, r, c] }.call(1, 2)
    end
  end
end
"##;

#[test]
fn optional_parameters_ahead_of_posts_are_still_refused_by_name() {
    let run = emit_and_run::real_blog()
        .write("app/lib/blockparamlab/posts.rb", POSTS)
        .run_ruby(
            "require_relative \"app/models/blockparamlab/posts\"\n\
             begin\n  Blockparamlab::Posts.optional_before_post\n  puts \"ran\"\n\
             rescue NotImplementedError\n  puts \"refused\"\nend\n",
        );
    let refusals: Vec<&String> = run
        .errors
        .iter()
        .filter(|e| e.contains("not represented in block IR"))
        .collect();
    assert_eq!(refusals.len(), 1, "{:?}", run.errors);
    assert!(
        refusals[0].contains("`b = …` (an optional parameter ahead of post parameters)"),
        "{refusals:?}"
    );
    assert!(run.success, "{}", run.stderr);
    assert_eq!(run.stdout, "refused\n");
}

#[test]
fn other_targets_refuse_block_signatures_by_name() {
    use roundhouse::diagnostic::Severity;
    use roundhouse::emit::diagnostics::scope;
    use roundhouse::project::target_files;

    let tree = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        (
            "app/models/probe.rb",
            "class Probe\n  def self.run\n    [1].map { |x, k = 3| x + k }\n    ->(params: {}, **args) { params }\n  end\nend\n",
        ),
    ]
    .into_iter()
    .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
    .collect();
    let app = ingest_app_from_tree(tree).expect("ingest");
    for target in [
        BuildTarget::Rust,
        BuildTarget::Crystal,
        BuildTarget::Go,
        BuildTarget::Python,
        BuildTarget::Kotlin,
        BuildTarget::Swift,
        BuildTarget::Elixir,
        BuildTarget::Typescript,
        BuildTarget::CSharp,
    ] {
        let (result, diagnostics) =
            scope(|| target_files(&app, std::path::Path::new("not-a-fixture"), target));
        assert!(
            result.is_err(),
            "{} must refuse block signatures",
            target.as_str()
        );
        let named: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.message.contains("block_signature"))
            .collect();
        assert_eq!(named.len(), 2, "{target:?}: {diagnostics:?}");
        assert!(
            named
                .iter()
                .all(|d| d.severity == Severity::Error && !d.span.is_synthetic()),
            "{named:?}"
        );
        assert!(named[0].message.contains("`k = …`"), "{:?}", named[0]);
        assert!(
            named[1].message.contains("`params: …`, `**args`"),
            "{:?}",
            named[1]
        );
    }
}
