//! ActiveSupport's `Module#concerning` and `Module#concern`
//! (`active_support/core_ext/module/concerning`).
//!
//! `concerning :Topic do … end` is `include concern(:Topic) { … }`, and
//! `concern` is `const_set :Topic, Module.new { extend
//! ActiveSupport::Concern; module_eval(&block) }`. The block is a closure:
//! its `def`s land in the new module, which the body then includes (or
//! prepends), while the constants it writes bind in the ENCLOSING body.
//! Core's `DraftOrders::Measures::AvailableMeasures` is a module of
//! nothing but such blocks of constants, read everywhere as
//! `AvailableMeasures::ACTION_TOPIC`; before, the blocks were opaque
//! captured calls, the module was not even ingested, and every read was
//! "constant not supported".
//!
//! Native CRuby with activesupport on the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::ingest::ingest_library_classes;

const MEASURES: &str = r##"module Demo
  module Measures
    module AvailableMeasures
      extend self

      concerning :Behaviour do
        ACTION_TOPIC = "Demo.action"
        BACKGROUND_TOPIC = "Demo.background"

        def topics
          [ACTION_TOPIC, BACKGROUND_TOPIC]
        end
      end

      concerning :TtlDeletion do
      end

      concerning :Timing do
        # A comment inside the block.
        TIMING_SUFFIX = ".Timing"

        def timed(name)
          "#{name}#{TIMING_SUFFIX}"
        end
      end
    end
  end
end
"##;

const LEDGER: &str = r##"module Demo
  class Ledger
    concerning :Entries do
      LIMIT = 3

      included do
        def self.kind
          "ledger"
        end
      end

      class_methods do
        def limit
          LIMIT
        end
      end

      def entries
        (1..LIMIT).map { |i| "e#{i}" }
      end
    end

    concerning :Summary do
      SEPARATOR = ","

      def summary
        entries.join(SEPARATOR)
      end
    end

    concerning :Audit, prepend: true do
      def summary
        "[" + super + "]"
      end
    end

    concern :Detached do
      DETACHED = "detached"

      def detached
        DETACHED
      end
    end
  end
end
"##;

const READER: &str = r##"module Demo
  class MeasureReader
    def self.report
      [Demo::Measures::AvailableMeasures::ACTION_TOPIC, Demo::Measures::AvailableMeasures::TIMING_SUFFIX, Demo::Ledger::SEPARATOR, Demo::Ledger::DETACHED]
    end
  end
end
"##;

const SCRIPT: &str = r##"m = Demo::Measures::AvailableMeasures
p [m::ACTION_TOPIC, m::BACKGROUND_TOPIC, m::TIMING_SUFFIX]
p [m.topics, m.timed("Draft")]
p m.ancestors.map(&:name)
p [m.const_defined?(:ACTION_TOPIC, false), m::Behaviour.const_defined?(:ACTION_TOPIC, false), m::Behaviour.instance_methods(false)]
p [Demo::Ledger.kind, Demo::Ledger.limit, Demo::Ledger.new.entries, Demo::Ledger.new.summary, Demo::Ledger::LIMIT]
p Demo::Ledger.ancestors.take(4).map(&:name)
p [Demo::Ledger.include?(Demo::Ledger::Detached), Demo::Ledger::Detached.instance_methods(false), Demo::Ledger.const_defined?(:DETACHED, false)]
p Demo::MeasureReader.report
"##;

#[test]
fn concerning_constants_bind_in_the_enclosing_body_and_methods_in_the_topic_module() {
    let classes = ingest_library_classes(MEASURES.as_bytes(), "available_measures.rb").expect("ingest");
    let named = |name: &str| classes.iter().find(|c| c.name.0.as_str() == name).unwrap_or_else(|| {
        panic!("no {name}: {:?}", classes.iter().map(|c| c.name.0.as_str()).collect::<Vec<_>>())
    });
    let host = named("Demo::Measures::AvailableMeasures");
    let constants: Vec<&str> = host.constants.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(constants, ["ACTION_TOPIC", "BACKGROUND_TOPIC", "TIMING_SUFFIX"]);
    let includes: Vec<&str> = host.includes.iter().map(|i| i.0.as_str()).collect();
    assert_eq!(
        includes,
        [
            "Demo::Measures::AvailableMeasures::Behaviour",
            "Demo::Measures::AvailableMeasures::TtlDeletion",
            "Demo::Measures::AvailableMeasures::Timing",
        ]
    );
    for topic in ["Behaviour", "TtlDeletion", "Timing"] {
        let module = named(&format!("Demo::Measures::AvailableMeasures::{topic}"));
        assert!(module.is_module && module.constants.is_empty(), "{topic}: {:?}", module.constants);
        assert!(
            format!("{:?}", module.unknown_calls).contains("Concern"),
            "{topic} extends ActiveSupport::Concern: {:?}",
            module.unknown_calls
        );
    }
    let behaviour = named("Demo::Measures::AvailableMeasures::Behaviour");
    let methods: Vec<&str> = behaviour.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(methods, ["topics"]);
    assert!(host.methods.iter().all(|m| m.name.as_str() != "topics"), "the host defines no `topics` of its own");
}

#[test]
fn emitted_concerning_blocks_match_native_activesupport() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_support\"\nrequire \"active_support/core_ext/module/concerning\"\n{MEASURES}\n{LEDGER}\n{READER}\n{SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected: Vec<String> = String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect();
    for probe in [
        r#"["Demo.action", "Demo.background", ".Timing"]"#,
        r#"[["Demo.action", "Demo.background"], "Draft.Timing"]"#,
        r#"[true, false, [:topics]]"#,
        r#"["ledger", 3, ["e1", "e2", "e3"], "[e1,e2,e3]", 3]"#,
        r#"["Demo::Ledger::Audit", "Demo::Ledger", "Demo::Ledger::Summary", "Demo::Ledger::Entries"]"#,
        r#"[false, [:detached], true]"#,
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }
    let run = emit_and_run::real_blog()
        .write("app/lib/demo/measures/available_measures.rb", MEASURES)
        .write("app/lib/demo/ledger.rb", LEDGER)
        .write("app/lib/demo/measure_reader.rb", READER)
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from native activesupport line for line");
}

/// The same block in an ActiveRecord model body: the constants are the
/// model's, the module is included, and its `class_methods` reach the
/// model's class side.
const EXCERPTS: &str = r##"
  concerning :Excerpts do
    EXCERPT = 5

    class_methods do
      def excerpt_limit
        EXCERPT
      end
    end

    def excerpt
      body.to_s[0, EXCERPT]
    end
  end
"##;

const MODEL_SCRIPT: &str = r##"p [Article::EXCERPT, Article.excerpt_limit, Article.include?(Article::Excerpts)]
p [Article.instance_method(:excerpt).owner.name, Article.const_defined?(:EXCERPT, false), Article::Excerpts.const_defined?(:EXCERPT, false)]
"##;

#[test]
fn concerning_in_a_model_body_matches_native_activerecord() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_record\"\nclass ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\nclass Article < ApplicationRecord\n{EXCERPTS}end\n{MODEL_SCRIPT}"
        ))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected: Vec<String> = String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect();
    assert_eq!(expected, [r#"[5, 5, true]"#, r#"["Article::Excerpts", true, false]"#], "native control");
    let run = emit_and_run::real_blog()
        .edit("app/models/article.rb", "  validates :title, presence: true\n", &format!("  validates :title, presence: true\n{EXCERPTS}"))
        .run_ruby(&format!("{MODEL_SCRIPT}p Article.new(title: \"t\", body: \"abcdefgh\").excerpt\n"));
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual[..expected.len()], expected, "emitted Ruby diverges from ActiveRecord line for line");
    assert_eq!(actual[expected.len()..], [r#""abcde""#]);
}
