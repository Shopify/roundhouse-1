//! `validates` options, and `errors.add` in a tableless ActiveModel class,
//! against native ActiveModel.
//!
//! Every `validates` option other than presence, absence and length was
//! dropped at ingest, and nothing reported it: `inclusion:` and
//! `numericality:` rules vanished, so a class whose only rule was
//! `inclusion` had no `valid?` at all. And `errors.add` inside a custom
//! `validate :method` of an `include ActiveModel::Validations` class was
//! left as written, because its `errors` is typed `ActiveModel::Errors`,
//! and the run raised "undefined method 'add' for an instance of Array".
//!
//! The same model files run natively on Core's Rails checkout (the
//! oracle tests/active_model_validator_runtime.rb uses) and on the emitted
//! tree, and every observation (`valid?` and `errors.full_messages`)
//! must be equal: Core's `Inventory::RestockCalculationSchema::
//! RestockRequest` verbatim (a numericality lambda `message:` reading
//! `data[:value]`, `allow_nil`, an inclusion over a constant), and a
//! class covering the other options. Unmodeled options are `check` errors.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

/// components/inventory/app/models/inventory/restock_calculation_schema.rb,
/// verbatim.
const RESTOCK_SCHEMA: &str = r##"# typed: strict
# frozen_string_literal: true

module Inventory
  module RestockCalculationSchema
    NO_RESTOCK_TYPE = "no_restock"
    RETURN_TYPE = "return"
    CANCEL_TYPE = "cancel"
    LEGACY_TYPE = "legacy_restock"

    RESTOCKING_RESTOCK_TYPES = [CANCEL_TYPE, RETURN_TYPE, LEGACY_TYPE].freeze #: Array[String]
    ALL_RESTOCK_TYPES = RESTOCKING_RESTOCK_TYPES + [NO_RESTOCK_TYPE] #: Array[String]
    RESTOCK_TYPES = ALL_RESTOCK_TYPES - [LEGACY_TYPE] #: Array[String]

    class RestockRequest
      include ActiveModel::Validations

      # POST /orders/:id/refunds/calculate returns this wording in its `restock` errors for a location_id of 0.
      UNTRUSTED_ID_MESSAGE = ->(_record, data) do
        value = data[:value]
        "can't convert #{value.class}[#{value}] to a positive Integer to use as an untrusted id"
      end #: ^(RestockRequest, { value: Integer? }) -> String

      #: Integer?
      attr_reader :quantity

      #: Integer?
      attr_reader :location_id

      #: String?
      attr_reader :restock_type

      #: Integer?
      attr_reader :line_item_id

      validates :quantity, numericality: { greater_than: 0 }
      validates :location_id, numericality: { greater_than: 0, message: UNTRUSTED_ID_MESSAGE }, allow_nil: true
      validates :restock_type, inclusion: { in: RESTOCK_TYPES }
      validates :line_item_id, numericality: { greater_than: 0, message: UNTRUSTED_ID_MESSAGE }

      #: (quantity: Integer?, restock_type: String?, line_item_id: Integer?, ?location_id: Integer?) -> void
      def initialize(quantity:, restock_type:, line_item_id:, location_id: nil)
        super()
        @quantity = quantity
        @location_id = location_id
        @restock_type = restock_type
        @line_item_id = line_item_id
      end

      #: -> { quantity: Integer?, location_id: Integer?, restock_type: String?, line_item_id: Integer? }
      def to_h
        { quantity:, location_id:, restock_type:, line_item_id: }
      end

      #: -> bool
      def restock?
        RESTOCKING_RESTOCK_TYPES.include?(restock_type)
      end

      #: -> bool
      def return?
        restock_type == RefundLineItem::RETURN_TYPE
      end
    end
  end
end
"##;

/// `RefundLineItem::RETURN_TYPE`, which `RestockRequest#return?` reads.
const REFUND_LINE_ITEM: &str = "class RefundLineItem\n  RETURN_TYPE = \"return\"\nend\n";

const RESTOCK_PROBE: &str = r##"class RestockProbe
  #: (untyped) -> String
  def self.observe(record)
    valid = record.valid?
    "#{valid} #{record.errors.full_messages.inspect} #{record.restock?} #{record.return?}"
  end

  #: -> Array[String]
  def self.lines
    rr = Inventory::RestockCalculationSchema::RestockRequest
    [
      observe(rr.new(quantity: 1, restock_type: "return", line_item_id: 5)),
      observe(rr.new(quantity: 2, restock_type: "no_restock", line_item_id: 5, location_id: 3)),
      observe(rr.new(quantity: 0, restock_type: "legacy_restock", line_item_id: -1, location_id: 0)),
      observe(rr.new(quantity: nil, restock_type: nil, line_item_id: nil)),
      observe(rr.new(quantity: "3", restock_type: "cancel", line_item_id: "0", location_id: "abc")),
      observe(rr.new(quantity: 1.5, restock_type: "cancel", line_item_id: 2.0, location_id: "0x1A")),
    ]
  end
end
"##;

const RESTOCK_EXPECTED: [&str; 6] = [
    "true [] true true",
    "true [] false false",
    r#"false ["Quantity must be greater than 0", "Location can't convert Integer[0] to a positive Integer to use as an untrusted id", "Restock type is not included in the list", "Line item can't convert Integer[-1] to a positive Integer to use as an untrusted id"] true false"#,
    r#"false ["Quantity is not a number", "Restock type is not included in the list", "Line item can't convert NilClass[] to a positive Integer to use as an untrusted id"] false false"#,
    r#"false ["Location can't convert String[abc] to a positive Integer to use as an untrusted id", "Line item can't convert Integer[0] to a positive Integer to use as an untrusted id"] true false"#,
    r#"false ["Location can't convert String[0x1A] to a positive Integer to use as an untrusted id"] true false"#,
];

/// numericality (`only_integer`, every comparison, `odd`, a String
/// `message:` with `%{value}` / `%{model}` / `%{attribute}`), format
/// (`with`, `without`), exclusion, inclusion over a Range and the Array
/// shorthand, presence and length with `message:`, `allow_nil`,
/// `allow_blank`, Symbol `if:` / `unless:`, and a custom `validate`
/// that calls `errors.add`.
const GAUGE: &str = r##"class Gauge
  include ActiveModel::Validations

  attr_reader :level, :count, :ratio, :code, :tag, :label, :size, :mode, :name, :strict

  validates :level, numericality: { only_integer: true, greater_than_or_equal_to: 1, less_than_or_equal_to: 9, odd: true }
  validates :count, numericality: { equal_to: 3, other_than: 4, less_than: 2, message: "%{value} is off" }, allow_nil: true
  validates :ratio, numericality: { greater_than: 0.5, message: "of %{model} is too small (%{attribute})" }, allow_blank: true
  validates :code, format: { with: /\A[A-Z]{3}\z/ }, exclusion: { in: %w[XXX] }
  validates :tag, format: { without: /\s/, message: "has %{attribute} spaces" }, allow_nil: true
  validates :label, inclusion: { in: 1..5, message: "is out" }, allow_blank: true
  validates :size, presence: { message: "must be given" }, length: { in: 2..4, message: "needs %{count}" }
  validates :mode, inclusion: %w[on off], if: :strict
  validates :name, presence: true, unless: :strict
  validate :name_not_reserved

  def initialize(level: 1, count: nil, ratio: nil, code: "ABC", tag: nil, label: nil, size: "abc", mode: nil, name: "n", strict: false)
    @level = level
    @count = count
    @ratio = ratio
    @code = code
    @tag = tag
    @label = label
    @size = size
    @mode = mode
    @name = name
    @strict = strict
  end

  def name_not_reserved
    errors.add(:name, "is reserved") if name == "admin"
    errors.add(:base, "Strict gauges need a name") if strict && name.nil?
  end
end
"##;

/// A tableless class whose only rule is `inclusion`.
const SWITCH: &str = r##"class Switch
  include ActiveModel::Validations

  STATES = %w[on off].freeze

  attr_reader :state

  validates :state, inclusion: { in: STATES }

  def initialize(state)
    @state = state
  end
end
"##;

const GAUGE_PROBE: &str = r##"class ValidationProbe
  #: (String, untyped) -> String
  def self.observe(label, record)
    valid = record.valid?
    "#{label}: #{valid} #{record.errors.full_messages.inspect}"
  end

  #: -> Array[String]
  def self.lines
    out = []
    out << observe("gauge default", Gauge.new)
    [0, 2, 3, 11, 1.0, "5", "7.0", "x", nil, "0x1A", " 3", "1e1"].each { |v| out << observe("level #{v.inspect}", Gauge.new(level: v)) }
    [nil, 3, 4, 1, "3", "abc"].each { |v| out << observe("count #{v.inspect}", Gauge.new(count: v)) }
    [nil, "", "  ", 0.25, 0.75, "0.5", 1, "zz"].each { |v| out << observe("ratio #{v.inspect}", Gauge.new(ratio: v)) }
    ["ABC", "XXX", "abc", nil, "ABCD", 123].each { |v| out << observe("code #{v.inspect}", Gauge.new(code: v)) }
    [nil, "", "a b", "ab"].each { |v| out << observe("tag #{v.inspect}", Gauge.new(tag: v)) }
    [nil, "", 3, 9, 2.5, "3"].each { |v| out << observe("label #{v.inspect}", Gauge.new(label: v)) }
    [nil, "", "a", "ab", "abcd", "abcde", %w[a b]].each { |v| out << observe("size #{v.inspect}", Gauge.new(size: v)) }
    [nil, "on", "up"].each { |v| out << observe("strict mode #{v.inspect}", Gauge.new(strict: true, mode: v)) }
    out << observe("lax mode", Gauge.new(mode: "up"))
    [nil, "", "admin"].each { |v| out << observe("name #{v.inspect}", Gauge.new(name: v)) }
    out << observe("strict nameless", Gauge.new(strict: true, mode: "on", name: nil))
    ["on", "off", "dim", nil].each { |v| out << observe("switch #{v.inspect}", Switch.new(v)) }
    out
  end
end
"##;

fn app(files: &[(&str, &str)]) -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n");
    for (path, source) in files {
        app = app.write(path, source);
    }
    app
}

/// The files loaded natively with Core's ActiveModel (the checkout
/// tests/active_model_validator_runtime.rb runs as its oracle), then
/// `script`.
fn native(files: &[(&str, &str)], script: &str) -> String {
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let rails = std::path::Path::new(&gem_home).join("bundler/gems/rails-5813559f4aed");
    assert!(rails.is_dir(), "Core's Rails checkout {} is missing (set CORE_GEM_HOME)", rails.display());
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_am_validates_native_{}_{seq}", std::process::id()));
    // ActiveRecord's en.yml (`taken`, `required`) is loaded in a Rails
    // app, so a Symbol `message:` naming one of them resolves.
    // A Rails app loads all of ActiveSupport's core extensions
    // (`in?`, which numericality's `in:` asks, among them).
    let mut program = String::from("require \"active_support/all\"\nrequire \"active_model\"\nrequire \"active_record\"\n");
    for (path, source) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, source).unwrap();
        program.push_str(&format!("require_relative {:?}\n", path.trim_end_matches(".rb")));
    }
    program.push_str(script);
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let mut command = Command::new("ruby");
    for lib in ["activesupport", "activemodel", "activerecord"] {
        command.arg("-I").arg(rails.join(lib).join("lib"));
    }
    let out = command
        .arg("native.rb")
        .current_dir(&dir)
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        // bigdecimal's extension needs the static TLS room jemalloc takes.
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

fn emitted(files: &[(&str, &str)], script: &str) -> String {
    let run = app(files).run_ruby(script);
    run.assert_passes();
    run.stdout.clone()
}

fn assert_same(native: &str, emitted: &str) {
    let (n, e): (Vec<_>, Vec<_>) = (native.lines().collect(), emitted.lines().collect());
    let diff: Vec<String> = n
        .iter()
        .zip(e.iter())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| format!("  native:  {a}\n  emitted: {b}"))
        .collect();
    assert!(
        n.len() == e.len() && diff.is_empty(),
        "{} native lines, {} emitted; differing:\n{}\n=== emitted ===\n{emitted}",
        n.len(),
        e.len(),
        diff.join("\n")
    );
}

#[test]
fn core_restock_request_answers_like_activemodel() {
    let files = [
        ("app/models/refund_line_item.rb", REFUND_LINE_ITEM),
        ("app/models/inventory/restock_calculation_schema.rb", RESTOCK_SCHEMA),
        ("app/models/restock_probe.rb", RESTOCK_PROBE),
    ];
    let script = "puts RestockProbe.lines\n";
    let native = native(&files, script);
    assert_eq!(native.lines().collect::<Vec<_>>(), RESTOCK_EXPECTED, "native:\n{native}");
    assert_same(&native, &emitted(&files, script));
}

#[test]
fn validates_options_answer_like_activemodel() {
    let files = [
        ("app/models/gauge.rb", GAUGE),
        ("app/models/switch.rb", SWITCH),
        ("app/models/validation_probe.rb", GAUGE_PROBE),
    ];
    let script = "puts ValidationProbe.lines\n";
    let native = native(&files, script);
    // Spot-check that the oracle ran the options it is meant to.
    for seen in [
        r#"count 4: false ["Count 4 is off", "Count 4 is off", "Count 4 is off"]"#,
        r#"level " 3": false ["Level must be an integer"]"#,
        r#"size nil: false ["Size must be given", "Size needs 2"]"#,
        r#"name "admin": false ["Name is reserved"]"#,
        r#"strict nameless: false ["Strict gauges need a name"]"#,
        r#"switch "dim": false ["State is not included in the list"]"#,
    ] {
        assert!(native.contains(seen), "native run lacks {seen:?}:\n{native}");
    }
    assert_eq!(native.lines().count(), 62, "{native}");
    assert_same(&native, &emitted(&files, script));
}

/// One line per observation: `valid?` (or `valid?(context)`) and the
/// full messages, for each record a probe builds.
const OBSERVE: &str = r##"class Observe
  #: (String, untyped, ?untyped) -> String
  def self.line(label, record, context = nil)
    valid = context.nil? ? record.valid? : record.valid?(context)
    "#{label}: #{valid} #{record.errors.full_messages.inspect}"
  end
end
"##;

fn same_as_activemodel(model: (&str, &str), probe: &str, expect: &[&str]) {
    let files = [("app/models/observe.rb", OBSERVE), model, ("app/models/probe.rb", probe)];
    let script = "puts Probe.lines\n";
    let native = native(&files, script);
    for seen in expect {
        assert!(native.contains(seen), "native run lacks {seen:?}:\n{native}");
    }
    assert_same(&native, &emitted(&files, script));
}

/// `length:` with a constant, method (Symbol) or Proc bound, `is:`, a
/// count of 1 (the singular message), `allow_nil: false` (nil then fails
/// `maximum:`), `too_short:` / `too_long:`, a Symbol `message:` with the
/// count, and a value with no `length` (its `to_s`).
const SIZER: &str = r##"class Sizer
  include ActiveModel::Validations

  MAX = 5

  attr_reader :title, :code, :body, :pin, :initial, :nick, :tags, :blurb, :limit

  validates :title, length: { maximum: MAX }
  validates :code, length: { is: 3 }, allow_nil: true
  validates :body, length: { minimum: :min_body, maximum: ->(r) { r.limit } }
  validates :pin, length: { is: 1 }
  validates :initial, length: { maximum: 1, allow_nil: false }
  validates :nick, length: { minimum: 2, too_short: "needs %{count} letters" }, allow_blank: true
  validates :tags, length: { within: 1..2, too_long: "has too many (%{count})" }
  validates :blurb, length: { maximum: MAX, message: :too_short }

  def initialize(title: "t", code: nil, body: "bb", pin: "p", initial: "i", nick: nil, tags: ["a"], blurb: nil, limit: 4)
    @title = title
    @code = code
    @body = body
    @pin = pin
    @initial = initial
    @nick = nick
    @tags = tags
    @blurb = blurb
    @limit = limit
  end

  def min_body
    2
  end
end
"##;

const SIZER_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = [Observe.line("default", Sizer.new)]
    ["abcdef", 123456, %w[a b c d e f], 12345].each { |v| out << Observe.line("title #{v.inspect}", Sizer.new(title: v)) }
    [nil, "ab", "abc", 12, %w[a b c]].each { |v| out << Observe.line("code #{v.inspect}", Sizer.new(code: v)) }
    ["", "b", "bbbbb", nil].each { |v| out << Observe.line("body #{v.inspect}", Sizer.new(body: v)) }
    out << Observe.line("body over a limit of 1", Sizer.new(body: "bb", limit: 1))
    ["", "pp", nil].each { |v| out << Observe.line("pin #{v.inspect}", Sizer.new(pin: v)) }
    [nil, "ab"].each { |v| out << Observe.line("initial #{v.inspect}", Sizer.new(initial: v)) }
    [nil, "", "a", "ab"].each { |v| out << Observe.line("nick #{v.inspect}", Sizer.new(nick: v)) }
    [[], %w[a b c], nil, "abc"].each { |v| out << Observe.line("tags #{v.inspect}", Sizer.new(tags: v)) }
    out << Observe.line("blurb", Sizer.new(blurb: "abcdef"))
    out
  end
end
"##;

#[test]
fn length_with_resolved_bounds_answers_like_activemodel() {
    same_as_activemodel(
        ("app/models/sizer.rb", SIZER),
        SIZER_PROBE,
        &[
            r#"title 123456: false ["Title is too long (maximum is 5 characters)"]"#,
            r#"code 12: false ["Code is the wrong length (should be 3 characters)"]"#,
            r#"body over a limit of 1: false ["Body is too long (maximum is 1 character)"]"#,
            r#"pin "": false ["Pin is the wrong length (should be 1 character)"]"#,
            r#"initial nil: false ["Initial is too long (maximum is 1 character)"]"#,
            r#"nick "a": false ["Nick needs 2 letters"]"#,
            r#"tags nil: false ["Tags is too short (minimum is 1 character)"]"#,
            r#"blurb: false ["Blurb is too short (minimum is 5 characters)"]"#,
        ],
    );
}

/// `format:` with a constant regexp and a Proc (with and without the
/// record).
const FORMER: &str = r##"class Former
  include ActiveModel::Validations

  ZIP = /\A\d{5}\z/
  SPACE = /\s/

  attr_reader :zip, :slug, :ref, :pattern, :word

  validates :zip, format: { with: ZIP }
  validates :slug, format: { without: SPACE, message: "has spaces" }, allow_nil: true
  validates :ref, format: { with: ->(r) { r.pattern } }
  validates :word, format: { with: -> { /\A[a-z]+\z/ } }, allow_blank: true

  def initialize(zip: "12345", slug: nil, ref: "ab", pattern: /\Aa/, word: nil)
    @zip = zip
    @slug = slug
    @ref = ref
    @pattern = pattern
    @word = word
  end
end
"##;

const FORMER_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = [Observe.line("default", Former.new)]
    ["1234", 12345, nil, "12345\n"].each { |v| out << Observe.line("zip #{v.inspect}", Former.new(zip: v)) }
    ["a b", "ab", ""].each { |v| out << Observe.line("slug #{v.inspect}", Former.new(slug: v)) }
    out << Observe.line("ref ba", Former.new(ref: "ba"))
    out << Observe.line("ref by its own pattern", Former.new(ref: "ba", pattern: /\Ab/))
    ["", "abc", "Abc", 7].each { |v| out << Observe.line("word #{v.inspect}", Former.new(word: v)) }
    out
  end
end
"##;

#[test]
fn format_with_resolved_regexps_answers_like_activemodel() {
    same_as_activemodel(
        ("app/models/former.rb", FORMER),
        FORMER_PROBE,
        &[
            r#"zip 12345: true []"#,
            r#"slug "a b": false ["Slug has spaces"]"#,
            r#"ref ba: false ["Ref is invalid"]"#,
            r#"ref by its own pattern: true []"#,
            r#"word "Abc": false ["Word is invalid"]"#,
        ],
    );
}

/// `if:` / `unless:` as a lambda, a Proc taking the record, and an Array
/// mixing them with Symbols.
const GATE: &str = r##"class Gate
  include ActiveModel::Validations

  attr_reader :a, :b, :c, :level, :strict, :lax

  validates :a, presence: true, if: -> { strict }
  validates :b, presence: true, if: [:strict, ->(g) { g.level > 1 }]
  validates :c, presence: true, unless: [-> { lax }, :strict]
  validates :level, numericality: { greater_than: 0 }, unless: proc { |g| g.lax }

  def initialize(a: nil, b: nil, c: nil, level: 1, strict: false, lax: false)
    @a = a
    @b = b
    @c = c
    @level = level
    @strict = strict
    @lax = lax
  end
end
"##;

const GATE_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = []
    [false, true].each do |strict|
      [false, true].each do |lax|
        [0, 1, 2].each do |level|
          out << Observe.line("strict #{strict} lax #{lax} level #{level}", Gate.new(strict: strict, lax: lax, level: level))
        end
      end
    end
    out << Observe.line("all given", Gate.new(a: 1, b: 2, c: 3, level: 2, strict: true))
    out
  end
end
"##;

#[test]
fn lambda_and_array_conditions_answer_like_activemodel() {
    same_as_activemodel(
        ("app/models/gate.rb", GATE),
        GATE_PROBE,
        &[
            r#"strict false lax false level 0: false ["C can't be blank", "Level must be greater than 0"]"#,
            r#"strict true lax false level 2: false ["A can't be blank", "B can't be blank"]"#,
            r#"strict false lax true level 0: true []"#,
        ],
    );
}

/// A Symbol `message:`: a built-in error type's English text (with its
/// count where it has one, and ActiveRecord's `required`), and for a
/// type nothing translates, the text I18n answers for the missing key.
const SPEAKER: &str = r##"class Speaker
  include ActiveModel::Validations

  attr_reader :name, :code, :num, :word, :pick

  validates :name, presence: { message: :required }
  validates :code, length: { maximum: 2, message: :too_short }
  validates :num, numericality: { message: :blank }
  validates :word, format: { with: /\A\w+\z/, message: :not_cool }
  validates :pick, inclusion: { in: %w[a], message: :taken }

  def initialize(name: "n", code: "c", num: 1, word: "w", pick: "a")
    @name = name
    @code = code
    @num = num
    @word = word
    @pick = pick
  end
end
"##;

const SPEAKER_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    [
      Observe.line("default", Speaker.new),
      Observe.line("all wrong", Speaker.new(name: nil, code: "ccc", num: "x", word: "a b", pick: "b")),
    ]
  end
end
"##;

#[test]
fn symbol_messages_answer_like_activemodel() {
    same_as_activemodel(
        ("app/models/speaker.rb", SPEAKER),
        SPEAKER_PROBE,
        &[
            "Name must exist",
            "Code is too short (minimum is 2 characters)",
            "Num can't be blank",
            r#"Word Translation missing. Options considered were:\n- en.activemodel.errors.models.speaker.attributes.word.not_cool"#,
            "Pick has already been taken",
        ],
    );
}

/// numericality bounds from a constant, a method (Symbol) and a Proc,
/// and `in:` a Range (literal or constant).
const COUNTER: &str = r##"class Counter
  include ActiveModel::Validations

  LIMIT = 10
  BAND = 0..100

  attr_reader :qty, :pct, :amt, :floor, :lvl

  validates :qty, numericality: { greater_than: :floor, less_than_or_equal_to: LIMIT }
  validates :pct, numericality: { in: BAND }, allow_nil: true
  validates :amt, numericality: { greater_than_or_equal_to: ->(r) { r.floor }, other_than: -> { 7 } }
  validates :lvl, numericality: { in: 1..3, only_integer: true }

  def initialize(qty: 5, pct: nil, amt: 5, floor: 1, lvl: 2)
    @qty = qty
    @pct = pct
    @amt = amt
    @floor = floor
    @lvl = lvl
  end
end
"##;

const COUNTER_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = [Observe.line("default", Counter.new)]
    [1, 0, 11, 10, "10", 2.5].each { |v| out << Observe.line("qty #{v.inspect}", Counter.new(qty: v)) }
    out << Observe.line("qty over a floor of 6", Counter.new(qty: 5, floor: 6))
    [0, 100, 101, -1, "50", 50.5].each { |v| out << Observe.line("pct #{v.inspect}", Counter.new(pct: v)) }
    [7, 0, 1].each { |v| out << Observe.line("amt #{v.inspect}", Counter.new(amt: v)) }
    [0, 3, 4, 2.0, "x"].each { |v| out << Observe.line("lvl #{v.inspect}", Counter.new(lvl: v)) }
    out
  end
end
"##;

#[test]
fn numericality_with_resolved_bounds_answers_like_activemodel() {
    same_as_activemodel(
        ("app/models/counter.rb", COUNTER),
        COUNTER_PROBE,
        &[
            r#"qty 11: false ["Qty must be less than or equal to 10"]"#,
            r#"qty over a floor of 6: false ["Qty must be greater than 6", "Amt must be greater than or equal to 6"]"#,
            r#"pct 101: false ["Pct must be in 0..100"]"#,
            r#"amt 7: false ["Amt must be other than 7"]"#,
            r#"lvl 4: false ["Lvl must be in 1..3"]"#,
        ],
    );
}

/// `inclusion:` / `exclusion:` with `in:` a Proc or a method (Symbol),
/// and the shorthand `inclusion: CONSTANT`.
const CHOOSER: &str = r##"class Chooser
  include ActiveModel::Validations

  SIZES = %w[s m l].freeze

  attr_reader :kind, :size, :mode, :tier, :allowed

  validates :kind, inclusion: { in: ->(r) { r.allowed } }
  validates :size, inclusion: SIZES
  validates :mode, exclusion: { in: -> { %w[off] } }, allow_nil: true
  validates :tier, inclusion: { in: :tiers, message: "%{value} is not a tier" }

  def initialize(kind: "a", size: "s", mode: nil, tier: 1, allowed: %w[a b])
    @kind = kind
    @size = size
    @mode = mode
    @tier = tier
    @allowed = allowed
  end

  def tiers
    [1, 2]
  end
end
"##;

const CHOOSER_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = [Observe.line("default", Chooser.new)]
    ["c", nil].each { |v| out << Observe.line("kind #{v.inspect}", Chooser.new(kind: v)) }
    out << Observe.line("kind from its own list", Chooser.new(kind: "c", allowed: %w[c]))
    ["xl", nil].each { |v| out << Observe.line("size #{v.inspect}", Chooser.new(size: v)) }
    ["off", "on"].each { |v| out << Observe.line("mode #{v.inspect}", Chooser.new(mode: v)) }
    [3, "1"].each { |v| out << Observe.line("tier #{v.inspect}", Chooser.new(tier: v)) }
    out
  end
end
"##;

#[test]
fn inclusion_with_resolved_lists_answers_like_activemodel() {
    same_as_activemodel(
        ("app/models/chooser.rb", CHOOSER),
        CHOOSER_PROBE,
        &[
            r#"kind "c": false ["Kind is not included in the list"]"#,
            r#"kind from its own list: true []"#,
            r#"size "xl": false ["Size is not included in the list"]"#,
            r#"mode "off": false ["Mode is reserved"]"#,
            r#"tier 3: false ["Tier 3 is not a tier"]"#,
        ],
    );
}

/// Messages computed where the class body writes them (a String
/// constant, an interpolated String, a Proc constant) and `length` over a
/// constant or computed Range.
const PHRASER: &str = r##"class Phraser
  include ActiveModel::Validations

  KINDS = %w[a b].freeze
  PICK = "must be picked from the list"
  SAY = ->(_record, data) { "rejects #{data[:value].inspect}" }
  SPAN = 2..4
  MAX = 6
  MIN = 2

  attr_reader :kind, :mode, :tone, :code, :pass, :word, :tag

  validates :kind, inclusion: { in: KINDS, message: "must be one of: #{KINDS.join(", ")}" }
  validates :mode, inclusion: { in: KINDS, message: PICK }
  validates :tone, inclusion: { in: KINDS, message: SAY }
  validates :code, length: { in: SPAN }
  validates :pass, length: { within: 3..MAX }, allow_nil: true
  validates :word, length: { in: SPAN, message: "has %{count} as a bound, not %{value}" }
  validates :tag, length: { within: (MIN..) }

  def initialize(kind: "a", mode: "a", tone: "a", code: "abc", pass: nil, word: "ab", tag: "abc")
    @tag = tag
    @kind = kind
    @mode = mode
    @tone = tone
    @code = code
    @pass = pass
    @word = word
  end
end
"##;

const PHRASER_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = [Observe.line("default", Phraser.new)]
    out << Observe.line("kind", Phraser.new(kind: "z"))
    out << Observe.line("mode", Phraser.new(mode: "z"))
    out << Observe.line("tone", Phraser.new(tone: "z"))
    ["a", "abcde", nil, 12].each { |v| out << Observe.line("code #{v.inspect}", Phraser.new(code: v)) }
    ["ab", "abcdefg", "abc"].each { |v| out << Observe.line("pass #{v.inspect}", Phraser.new(pass: v)) }
    ["a", "abcde"].each { |v| out << Observe.line("word #{v.inspect}", Phraser.new(word: v)) }
    ["a", "a" * 40].each { |v| out << Observe.line("tag #{v.inspect}", Phraser.new(tag: v)) }
    out
  end
end
"##;

#[test]
fn computed_messages_and_ranges_answer_like_activemodel() {
    same_as_activemodel(
        ("app/models/phraser.rb", PHRASER),
        PHRASER_PROBE,
        &[
            r#"kind: false ["Kind must be one of: a, b"]"#,
            r#"mode: false ["Mode must be picked from the list"]"#,
            r#"tone: false ["Tone rejects \"z\""]"#,
            r#"code "a": false ["Code is too short (minimum is 2 characters)"]"#,
            r#"code nil: false ["Code is too short (minimum is 2 characters)"]"#,
            r#"pass "abcdefg": false ["Pass is too long (maximum is 6 characters)"]"#,
            r#"word "a": false ["Word has 2 as a bound, not a"]"#,
            r#"tag "a": false ["Tag is too short (minimum is 2 characters)"]"#,
        ],
    );
}

/// `on:` contexts on a tableless class: `valid?(context)` runs the rules
/// for that context, and `valid?` (no context; ActiveModel has no
/// `:create` / `:update` default) runs none of them.
const DRAFT: &str = r##"class Draft
  include ActiveModel::Validations

  attr_reader :title, :body

  validates :title, presence: true, on: :publish
  validates :body, length: { minimum: 3 }, on: [:publish, :review]
  validates :title, length: { maximum: 3 }, on: :create

  def initialize(title: nil, body: "b")
    @title = title
    @body = body
  end
end
"##;

const DRAFT_PROBE: &str = r##"class Probe
  #: -> Array[String]
  def self.lines
    out = []
    [nil, :publish, :review, :create, :other].each do |context|
      out << Observe.line("empty #{context.inspect}", Draft.new, context)
      out << Observe.line("long #{context.inspect}", Draft.new(title: "long", body: "body"), context)
    end
    draft = Draft.new
    draft.valid?(:publish)
    out << "context after: #{draft.validation_context.inspect}"
    out
  end
end
"##;

#[test]
fn validation_contexts_answer_like_activemodel() {
    same_as_activemodel(
        ("app/models/draft.rb", DRAFT),
        DRAFT_PROBE,
        &[
            "empty nil: true []",
            r#"empty :publish: false ["Title can't be blank", "Body is too short (minimum is 3 characters)"]"#,
            r#"empty :review: false ["Body is too short (minimum is 3 characters)"]"#,
            r#"long :create: false ["Title is too long (maximum is 3 characters)"]"#,
            "context after: nil",
        ],
    );
}

/// What ingest does not model is a `check` error at the call, never a
/// dropped check.
#[test]
fn unmodeled_validates_options_are_check_errors() {
    let ticket = r##"class Ticket
  include ActiveModel::Validations

  attr_reader :code, :seats, :email, :kind, :note

  SPAN = 1..4

  validates :code, uniqueness: true
  validates :seats, numericality: { only_numeric: true }
  validates :email, presence: true, strict: true
  validates :kind, comparison: { greater_than: 1 }
  validates :note, length: { in: SPAN.to_a }
  validates :email, format: { with: /^x/ }
  validates :seats, numericality: { greater_than: 0, message: "must exceed %{count}" }

  def initialize(code)
    @code = code
  end
end
"##;
    let (_dir, errors) = app(&[("app/models/ticket.rb", ticket)]).emit(roundhouse::project::BuildTarget::Ruby);
    for expected in [
        "`validates uniqueness:` not supported (all targets): on `code`, a uniqueness check on a class outside ActiveRecord",
        "`validates numericality:` not supported (all targets): on `seats`, `only_numeric:`",
        "`validates presence:` not supported (all targets): on `email`, `strict:`",
        "`validates comparison:` not supported (all targets): on `kind`",
        "`validates length:` not supported (all targets): on `note`, `in:` that is not a Range literal or a constant",
        "`validates format:` not supported (all targets): on `email`, a regexp with a `^`/`$` anchor",
        "`validates numericality:` not supported (all targets): on `seats`, a `message:` interpolating %{count}",
    ] {
        assert!(errors.iter().any(|e| e.contains(expected)), "missing {expected:?} in:\n{}", errors.join("\n"));
    }
    assert_eq!(errors.len(), 7, "{}", errors.join("\n"));
}
