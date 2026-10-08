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

const NATIVE_PRELUDE: &str = "RailsOracle.activate(\"activesupport\", \"activemodel\", \"activerecord\")\n\
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

/// A Concern this cannot verify (here a `sealed!`, which checks each
/// including file, or an `include` of a module the app does not define,
/// or of a Concern, ahead of the `extend`) does not get its block run on
/// plain includers: an error naming the reason and the includers, not a
/// warning.
#[test]
fn an_unverified_concerns_block_is_an_error_not_a_drop() {
    let mixing = "module Mixing\n  extend ActiveSupport::Concern\n  extend T::Helpers\n  sealed!\n\n  included do\n    class_attribute :limit, default: 3\n  end\nend\n";
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
        ) && ours[0].contains("`sealed!`")
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

    // An app Concern ahead of the extension is included at once, not
    // deferred to the includer: still refused.
    let behind = "module Behind\n  include Mixing\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :size, default: 3\n  end\nend\n";
    let mixing = "module Mixing\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :limit, default: 3\n  end\nend\n";
    let (gaps, _) = refused(&[
        ("app/models/concerns/mixing.rb", mixing),
        ("app/models/concerns/behind.rb", behind),
        ("app/models/one.rb", "class One\n  include Behind\nend\n"),
    ]);
    assert!(
        gaps.iter()
            .any(|g| g.contains("`included do` of Behind is not run")
                && g.contains("precedes its `extend ActiveSupport::Concern`")),
        "{gaps:#?}"
    );
}

// ---- An app module included ahead of the extension --------------------------

/// Core's `CustomCssValidator`, verbatim down to its header: a plain
/// safelist module included before `extend ActiveSupport::Concern`, then
/// `T::Helpers`, `requires_ancestor`, an `attr_reader`, constants and the
/// `included do class_attribute … end`. Ahead of the extension `include`
/// is `Module#include`, which it also is after it for a module that is no
/// Concern, so the block runs on each includer. Only the Loofah, Crass and
/// I18n parts of the bodies are cut (no gem here supplies them); the
/// messages are the I18n keys.
const CSS_SAFELIST: &str = r##"# typed: true
# frozen_string_literal: true

module CssSafelist
  DENYLIST_SELECTORS = /<.*>|.shopify-section/ # shopify-section class and <> tags in the selector
  CSS_IMPORTANT = "!important"
  ADDITIONAL_CSS_PROPERTIES = [
    "accent-color",
    "bottom",
  ].freeze
end
"##;

const CSS_VALIDATOR: &str = r##"# typed: true
# frozen_string_literal: true

module CssValidator
  include CssSafelist
  extend ActiveSupport::Concern
  extend T::Helpers
  requires_ancestor { Kernel }

  #: untyped
  attr_reader :values

  THEME = "theme"
  SECTION = "section"
  SCOPEABLE_AT_RULES = ["media", "container", "layer", "supports"].freeze

  included do
    class_attribute(:character_limit, default: 1500)
    class_attribute(:type)
  end

  #: (untyped errors) -> void
  def validate(errors)
    return if values.nil?

    validate_value_type(errors)
    validate_size_limit(errors) if errors.empty?
    validate_selectors(errors) if errors.empty?
  end

  private

  #: (untyped errors) -> void
  def validate_value_type(errors)
    return if values.is_a?(Array) && values.all? { |value| value.is_a?(String) }

    errors.add(
      :base,
      "online_store.asset_cloud.theme_settings_values_asset.custom_css_must_be_an_array_of_string_values"
    )
  end

  #: (untyped errors) -> void
  def validate_size_limit(errors)
    normalized_values = normalize_values
    return if normalized_values.length <= self.class.character_limit

    errors.add(
      :base,
      "online_store.asset_cloud.theme_settings_values_asset.custom_css_content_limit_exceeded:#{self.class.character_limit}"
    )
  end

  #: -> String
  def normalize_values
    values.join("").gsub(/\s+/, "")
  end

  #: (untyped errors) -> void
  def validate_selectors(errors)
    values.each do |css_value|
      next unless css_value.match?(DENYLIST_SELECTORS)

      errors.add(:base, "custom_css_selector_not_permitted:#{theme_custom_css?}")
    end
  end

  #: -> bool
  def theme_custom_css?
    type == THEME
  end
end
"##;

const CSS_SECTION: &str = r##"# typed: strict
# frozen_string_literal: true

class Theme
  module Sections
    class CustomCss
      include CssValidator

      self.character_limit = 500
      self.type = SECTION

      #: (untyped values) -> void
      def initialize(values)
        @values = values
      end
    end
  end
end
"##;

const CSS_THEME: &str = r##"class Theme
  class CustomCss
    include CssValidator

    self.type = THEME

    def initialize(values)
      @values = values
    end
  end
end
"##;

const CSS_SCRIPT: &str = r##"class Errs
  def initialize = @list = []
  def add(*entry) = @list << entry
  def empty? = @list.empty?
  def to_a = @list
end
check = ->(css) { Errs.new.tap { |e| css.validate(e) }.to_a }
section = Theme::Sections::CustomCss
theme = Theme::CustomCss
p [section.character_limit, section.type, theme.character_limit, theme.type]
p check.(section.new(["a{color:red}"]))
p check.(section.new(["a" * 501]))
p check.(theme.new(["a" * 501]))
p check.(section.new([".shopify-section{}"]))
p check.(theme.new([".shopify-section{}"]))
p check.(section.new([1]))
p check.(section.new(nil))
p [section.new([]).values, CssValidator::CSS_IMPORTANT, section.ancestors.take_while { |m| m != Object }]
"##;

#[test]
fn an_app_module_ahead_of_the_extension_still_runs_the_block_as_native() {
    let classes = [
        ("app/models/concerns/css_safelist.rb", CSS_SAFELIST),
        ("app/models/concerns/css_validator.rb", CSS_VALIDATOR),
        ("app/models/theme/sections/custom_css.rb", CSS_SECTION),
        ("app/models/theme/custom_css.rb", CSS_THEME),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native(
        "css",
        &format!("{NATIVE_PRELUDE}require \"sorbet-runtime\"\n{program}{CSS_SCRIPT}"),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#"[500, "section", 1500, "theme"]"#,
            "[]",
            r#"[[:base, "online_store.asset_cloud.theme_settings_values_asset.custom_css_content_limit_exceeded:500"]]"#,
            "[]",
            r#"[[:base, "custom_css_selector_not_permitted:false"]]"#,
            r#"[[:base, "custom_css_selector_not_permitted:true"]]"#,
            r#"[[:base, "online_store.asset_cloud.theme_settings_values_asset.custom_css_must_be_an_array_of_string_values"]]"#,
            "[]",
            r#"[[], "!important", [Theme::Sections::CustomCss, CssValidator, CssSafelist]]"#,
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .run_ruby(CSS_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

// ---- A re-include below an override -----------------------------------------

/// `ReDial < FineDial` includes Tunable again. Ruby skips a module the
/// ancestors already have, so `append_features` neither extends
/// `ClassMethods` again nor runs the block: `ReDial.tune` is FineDial's
/// override, and `ReDial.level` is the value FineDial set.
const DIAL: &str = "class Dial\n  include Tunable\nend\n";
const FINE_DIAL: &str = "class FineDial < Dial\n  self.level = 5\n\n  def self.tune(n)\n    self.level = n * 10\n  end\nend\n";
const RE_DIAL: &str = "class ReDial < FineDial\n  include Tunable\nend\n";
const DIAL_SCRIPT: &str = r##"p [Dial.level, FineDial.level, ReDial.level]
ReDial.tune(2)
p [Dial.level, FineDial.level, ReDial.level]
Dial.tune(3)
p [Dial.level, FineDial.level, ReDial.level]
p [ReDial.method(:tune).owner == FineDial.singleton_class, ReDial.singleton_methods(false)]
"##;

#[test]
fn a_reinclude_below_an_override_keeps_the_override_as_native() {
    let classes = [
        ("app/models/concerns/tunable.rb", TUNABLE),
        ("app/models/dial.rb", DIAL),
        ("app/models/fine_dial.rb", FINE_DIAL),
        ("app/models/re_dial.rb", RE_DIAL),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native("dial", &format!("{NATIVE_PRELUDE}{program}{DIAL_SCRIPT}"));
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        ["[1, 5, 5]", "[1, 5, 20]", "[3, 5, 20]", "[true, []]"],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .run_ruby(DIAL_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

// ---- `mixes_in_class_methods` -------------------------------------------------

/// sorbet-runtime's `mixes_in_class_methods` adds an `included` that
/// extends the includer and calls `super`, so ActiveSupport::Concern's
/// `append_features` still runs the block on it. (Declared ahead of the
/// `included do`, its one-argument `included` shadows the Concern's and
/// the module fails to load natively, so Core declares it after.)
const MIXED: &str = r##"module Mixed
  extend ActiveSupport::Concern
  extend T::Helpers

  module Definitions
    def mixed(n) = "#{name}:#{n}:#{limit}"
  end

  included do
    class_attribute :limit, default: 3
  end

  mixes_in_class_methods(Definitions)
end
"##;
const MIXER: &str = "class Mixer\n  include Mixed\n  self.limit = 4\nend\n";
const PLAIN_MIXER: &str = "class PlainMixer\n  include Mixed\nend\n";
const MIXED_SCRIPT: &str = r##"p [Mixer.mixed(1), PlainMixer.mixed(2), Mixer.limit, PlainMixer.new.limit]
p [Mixer.singleton_class.include?(Mixed::Definitions), Mixer.method(:mixed).owner]
"##;

#[test]
fn a_mixes_in_class_methods_concern_runs_its_block_as_native() {
    let classes = [
        ("app/models/concerns/mixed.rb", MIXED),
        ("app/models/mixer.rb", MIXER),
        ("app/models/plain_mixer.rb", PLAIN_MIXER),
    ];
    let program: String = classes.iter().map(|(_, source)| *source).collect();
    let expected = native(
        "mixed",
        &format!("{NATIVE_PRELUDE}require \"sorbet-runtime\"\n{program}{MIXED_SCRIPT}"),
    );
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected,
        [
            r#"["Mixer:1:4", "PlainMixer:2:3", 4, 3]"#,
            "[true, Mixed::Definitions]",
        ],
        "native control"
    );
    let run = classes
        .iter()
        .fold(emit_and_run::real_blog(), |overlay, (path, source)| {
            overlay.write(path, source)
        })
        .run_ruby(MIXED_SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        actual, expected,
        "emitted Ruby diverges from native line for line"
    );
}

// ---- The drop warning names only includers without the block ----------------

/// The Concern's own `included do` is not emitted on the module: it is
/// lowered into each includer. The emit's "not modelled and is dropped"
/// warning at the block is true only when some includer (a controller,
/// say) does not get it.
#[test]
fn the_included_drop_warning_fires_only_for_an_includer_without_the_block() {
    use roundhouse::ingest::ingest_app_from_tree;
    use roundhouse::project::BuildTarget;
    let shared = "module Shared\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :limit, default: 3\n  end\nend\n";
    let served = "module Served\n  extend ActiveSupport::Concern\n\n  included do\n    class_attribute :limit, default: 3\n  end\nend\n";
    let files = [
        ("app/models/concerns/shared.rb", shared),
        ("app/models/concerns/served.rb", served),
        ("app/models/one.rb", "class One\n  include Shared\nend\n"),
        ("app/models/two.rb", "class Two\n  include Served\nend\n"),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\n  include Served\nend\n",
        ),
    ];
    let ((app, emitted), diags) = roundhouse::emit::diagnostics::scope(|| {
        let mut app = ingest_app_from_tree(tree(&files)).expect("ingest");
        roundhouse::session::analyze_and_lower(&mut app);
        let emitted = roundhouse::project::target_files(
            &app,
            std::path::Path::new("not-a-fixture"),
            BuildTarget::Ruby,
        );
        (app, emitted)
    });
    emitted.expect("ruby files");
    assert_eq!(
        app.carried_included_blocks,
        [roundhouse::ident::ClassId(roundhouse::ident::Symbol::from(
            "Shared"
        ))]
    );
    let drops: Vec<String> = diags
        .iter()
        .map(|d| d.message.clone())
        .filter(|m| m.contains("`included` in the body of"))
        .collect();
    assert!(!drops.iter().any(|m| m.contains("`Shared`")), "{drops:#?}");
    assert!(drops.iter().any(|m| m.contains("`Served`")), "{drops:#?}");
}
