//! A Concern's `included do class_attribute … end` on a plain (non-model)
//! includer.
//!
//! `ActiveSupport::Concern#append_features` runs the block with
//! `base.class_eval`, so `class_attribute` defines its class-side and
//! instance methods on the includer and seeds the default there. A
//! subclass reads its parent's value until it writes its own; a write
//! never reaches the parent. The block's `self.x = v` and `include M`
//! run on the includer too. Any other statement the block holds is a
//! reported gap, never a silent drop.
//!
//! Each case runs the emitted app against the same classes under native
//! Rails (the pinned oracle, tests/support/rails_oracle.rb). Fixtures are
//! written into the Overlay's scratch copy, never into `fixtures/real-blog`
//! (a link shared by every worktree).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");

/// Runs `program` (a file: the oracle re-executes it under jemalloc).
fn native(name: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rh_concern_library_class_attribute_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.rb"),
        format!("require {ORACLE:?}\n{program}"),
    )
    .unwrap();
    let out = Command::new("ruby")
        .arg("main.rb")
        .current_dir(&dir)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "native {name} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

const NATIVE_PRELUDE: &str =
    "RailsOracle.activate(\"activesupport\", \"activemodel\", \"activerecord\")\n\
     require \"active_support\"\nrequire \"active_support/concern\"\n\
     require \"active_support/core_ext/class/attribute\"\n";

const TUNABLE: &str = r##"module Tunable
  extend ActiveSupport::Concern

  included do
    class_attribute :level, default: 1
    class_attribute :mode, instance_writer: false
    class_attribute :flags, instance_predicate: false, default: [:a].freeze
    class_attribute :secret, instance_reader: false
    self.mode = :auto
  end

  class_methods do
    def tune(n)
      self.level = n
    end
  end

  def describe
    "#{self.class.name}:#{level}"
  end
end
"##;

/// A plain module the block includes.
const LABELLED: &str = r##"module Labelled
  def label = "#{self.class.name.downcase}-#{level}"
end
"##;

/// A Concern depending on Tunable (deferred to its includer), whose own
/// block includes a module and declares one more attribute.
const CALIBRATED: &str = r##"module Calibrated
  extend ActiveSupport::Concern
  include Tunable

  included do
    include Labelled
    class_attribute :offset, default: 0
  end
end
"##;

const KNOB: &str = "class Knob\n  include Tunable\nend\n";
/// Core's `self.level = 5` subclass shape.
const FINE_KNOB: &str = "class FineKnob < Knob\n  self.level = 5\nend\n";
const COARSE_KNOB: &str = "class CoarseKnob < Knob\n  tune 3\nend\n";
const PLAIN_KNOB: &str = "class PlainKnob < Knob\nend\n";
/// Already in the superclass's ancestors: the block does not run again.
const RE_KNOB: &str = "class ReKnob < FineKnob\n  include Tunable\nend\n";
const LOUD_KNOB: &str = "class LoudKnob\n  include Tunable\n  self.level = 11\nend\n";
const GAUGE: &str = "class Gauge\n  include Calibrated\n  self.offset = 2\nend\n";

const SCRIPT: &str = r##"p [Knob.level, FineKnob.level, CoarseKnob.level, PlainKnob.level, ReKnob.level, LoudKnob.level]
Knob.level = 2
p [Knob.level, FineKnob.level, CoarseKnob.level, PlainKnob.level, ReKnob.level]
k = Knob.new
p [k.level, k.level?, Knob.level?, FineKnob.new.describe]
k.level = 9
p [k.level, Knob.level, Knob.new.level]
p [Knob.mode, k.mode, k.respond_to?(:mode=), Knob.respond_to?(:mode=)]
p [Knob.flags, k.flags, k.respond_to?(:flags?), Knob.respond_to?(:flags?)]
p [k.respond_to?(:secret), k.respond_to?(:secret=), Knob.secret]
p [Gauge.level, Gauge.offset, Gauge.mode, Gauge.new.label, Gauge.new.describe]
p Gauge.ancestors.take_while { |m| m != Object }
p Knob.instance_method(:level).owner
p Knob.method(:level).owner
p FineKnob.method(:level).owner
p ReKnob.method(:level).owner
p [Knob.public_method_defined?(:level), Tunable.method_defined?(:level), Tunable.respond_to?(:level)]
"##;

#[test]
fn included_class_attribute_is_the_includers_as_native() {
    let classes = [
        ("app/models/concerns/tunable.rb", TUNABLE),
        ("app/models/concerns/labelled.rb", LABELLED),
        ("app/models/concerns/calibrated.rb", CALIBRATED),
        ("app/models/knob.rb", KNOB),
        ("app/models/fine_knob.rb", FINE_KNOB),
        ("app/models/coarse_knob.rb", COARSE_KNOB),
        ("app/models/plain_knob.rb", PLAIN_KNOB),
        ("app/models/re_knob.rb", RE_KNOB),
        ("app/models/loud_knob.rb", LOUD_KNOB),
        ("app/models/gauge.rb", GAUGE),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native("tunable", &format!("{NATIVE_PRELUDE}{program}{SCRIPT}"));
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            "[1, 5, 3, 1, 5, 11]",
            "[2, 5, 3, 2, 5]",
            r#"[2, true, true, "FineKnob:5"]"#,
            "[9, 2, 2]",
            "[:auto, :auto, false, true]",
            "[[:a], [:a], false, false]",
            "[false, true, nil]",
            r#"[1, 2, :auto, "gauge-1", "Gauge:1"]"#,
            "[Gauge, Labelled, Calibrated, Tunable]",
            "Knob",
            "#<Class:Knob>",
            "#<Class:Knob>",
            "#<Class:Knob>",
            "[true, false, false]",
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

// ---- Gaps stay explicit ----------------------------------------------------

fn tree(files: &[(&str, &str)]) -> roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> {
    let schema = (
        "db/schema.rb",
        "ActiveRecord::Schema.define { create_table(:messages) { |t| t.string :body } }",
    );
    files
        .iter()
        .chain([&schema])
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect()
}

/// Strict ingest fails with the first refusal; survey ingest records
/// every one and keeps the app.
fn refused(files: &[(&str, &str)]) -> (Vec<String>, roundhouse::App) {
    use roundhouse::ingest::{ingest_app_from_tree, survey};
    let error = ingest_app_from_tree(tree(files))
        .expect_err("the block is refused")
        .to_string();
    survey::activate();
    let result = ingest_app_from_tree(tree(files));
    let gaps: Vec<String> = survey::drain().iter().map(|g| g.to_string()).collect();
    let app = result.expect("survey keeps the app");
    assert_eq!(gaps.first(), Some(&error));
    (gaps, app)
}

fn methods<'a>(app: &'a roundhouse::App, owner: &str) -> Vec<String> {
    let lc = app
        .library_classes
        .iter()
        .find(|lc| lc.name.0.as_str() == owner)
        .expect(owner);
    let mut names: Vec<String> = lc
        .methods
        .iter()
        .map(|m| match m.receiver {
            roundhouse::dialect::MethodReceiver::Class => format!("self.{}", m.name.as_str()),
            _ => m.name.as_str().to_string(),
        })
        .collect();
    names.sort();
    names
}

/// A statement the plain includer gets nothing for is reported once, at
/// the statement, naming its includers; what the block does model is
/// still supplied.
#[test]
fn an_unmodeled_included_statement_is_an_error_not_a_drop() {
    let concern = "module Counted\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :limit, default: 3\n    cattr_accessor :count\n    validates :name, presence: true\n  end\nend\n";
    let (gaps, app) = refused(&[
        ("app/models/concerns/counted.rb", concern),
        (
            "app/models/counter.rb",
            "class Counter\n  include Counted\nend\n",
        ),
        (
            "app/models/tally.rb",
            "class Tally\n  include Counted\nend\n",
        ),
    ]);
    assert_eq!(gaps.len(), 2, "{gaps:#?}");
    assert!(
        gaps[0].contains("the generated method `count` (and any others its macro defines) in `included do` of Counted")
            && gaps[0].contains("Counter, Tally would run without it"),
        "{}",
        gaps[0]
    );
    assert!(
        gaps[1].contains("`validates` in `included do` of Counted"),
        "{}",
        gaps[1]
    );
    assert_eq!(
        methods(&app, "Counter"),
        [
            "limit",
            "limit=",
            "limit?",
            "self.limit",
            "self.limit=",
            "self.limit?"
        ]
    );
}

/// The includer's own definition of a generated name is ordered against
/// the `include` in a body the IR no longer keeps in sequence; and a
/// module base has no `class_attribute` at all.
#[test]
fn a_clashing_or_module_base_class_attribute_is_refused() {
    let concern = "module Leveled\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :level, default: 1\n  end\nend\n";
    let (gaps, app) = refused(&[
        ("app/models/concerns/leveled.rb", concern),
        (
            "app/models/clash.rb",
            "class Clash\n  include Leveled\n\n  def self.level = 0\nend\n",
        ),
    ]);
    assert_eq!(gaps.len(), 1, "{gaps:#?}");
    assert!(
        gaps[0].contains("class_attribute :level on Clash: `level` is also defined"),
        "{}",
        gaps[0]
    );
    assert_eq!(methods(&app, "Clash"), ["self.level"]);

    let (gaps, _) = refused(&[
        ("app/models/concerns/leveled.rb", concern),
        (
            "app/models/concerns/carrier.rb",
            "module Carrier\n  include Leveled\nend\n",
        ),
    ]);
    assert_eq!(gaps.len(), 1, "{gaps:#?}");
    assert!(
        gaps[0].contains("runs on the module Carrier, which has no `class_attribute`"),
        "{}",
        gaps[0]
    );
}

// ---- sorbet-runtime alongside the Concern -----------------------------------

/// Core's `CustomCssValidator` shape: the Concern also extends
/// `T::Sig` and `T::Helpers` and declares `requires_ancestor` and
/// `abstract!`. None of these changes what `include` does (sorbet-runtime
/// defines no `included`/`extended` on them; `abstract!`'s
/// `append_features` calls `super`), so the block still runs on each
/// includer, and an includer's `self.x = v` writes its own value.
const SIZED: &str = r##"module Sized
  extend ActiveSupport::Concern
  extend T::Sig
  extend T::Helpers
  requires_ancestor { Kernel }
  abstract!

  included do
    class_attribute(:limit, default: 1500)
    class_attribute(:kind)
  end

  sig { returns(String) }
  def summary
    "#{kind}:#{limit}"
  end
end
"##;
const SECTION: &str =
    "class Section\n  include Sized\n\n  self.limit = 500\n  self.kind = \"section\"\nend\n";
const PAGE: &str = "class Page\n  include Sized\nend\n";
const SUB_SECTION: &str = "class SubSection < Section\nend\n";
const SIZED_SCRIPT: &str = r##"p [Section.limit, Page.limit, SubSection.limit, Section.kind, Page.kind]
p [Section.new.summary, Page.new.summary, SubSection.new.summary]
Page.limit = 7
p [Page.limit, Section.limit]
"##;

#[test]
fn a_sorbet_helpers_concern_runs_its_block_as_native() {
    let classes = [
        ("app/models/concerns/sized.rb", SIZED),
        ("app/models/section.rb", SECTION),
        ("app/models/page.rb", PAGE),
        ("app/models/sub_section.rb", SUB_SECTION),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native(
        "sized",
        &format!("{NATIVE_PRELUDE}require \"sorbet-runtime\"\n{program}{SIZED_SCRIPT}"),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#"[500, 1500, 500, "section", nil]"#,
            r#"["section:500", ":1500", "section:500"]"#,
            "[7, 500]",
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .run_ruby(SIZED_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

/// A Concern this cannot verify (here a `mixes_in_class_methods`, which
/// extends each includer, or an `include` ahead of the `extend`) does not
/// get its block run on plain includers: an error naming the reason and
/// the includers, not a warning.
#[test]
fn an_unverified_concerns_block_is_an_error_not_a_drop() {
    let mixing = "module Mixing\n  extend ActiveSupport::Concern\n  extend T::Helpers\n\n  module ClassMethods\n    def mixed = true\n  end\n  mixes_in_class_methods(ClassMethods)\n\n  included do\n    class_attribute :limit, default: 3\n  end\nend\n";
    let (gaps, app) = refused(&[
        ("app/models/concerns/mixing.rb", mixing),
        ("app/models/one.rb", "class One\n  include Mixing\nend\n"),
        ("app/models/two.rb", "class Two\n  include Mixing\nend\n"),
    ]);
    let ours: Vec<&String> = gaps
        .iter()
        .filter(|g| g.contains("`included do` of Mixing"))
        .collect();
    assert_eq!(ours.len(), 1, "{gaps:#?}");
    assert!(
        ours[0].contains(
            "is not run on a plain (non-model) includer: it also extends a module other than"
        ) && ours[0].contains("`mixes_in_class_methods`")
            && ours[0].contains("One, Two would run without it"),
        "{}",
        ours[0]
    );
    assert!(!methods(&app, "One").iter().any(|m| m.contains("limit")));

    let early = "module Early\n  include Comparable\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :limit, default: 3\n  end\nend\n";
    let (gaps, _) = refused(&[
        ("app/models/concerns/early.rb", early),
        ("app/models/one.rb", "class One\n  include Early\nend\n"),
    ]);
    assert!(
        gaps.iter()
            .any(|g| g.contains("`included do` of Early is not run")
                && g.contains("precedes its `extend ActiveSupport::Concern`")
                && g.contains("One would run without it")),
        "{gaps:#?}"
    );
}
