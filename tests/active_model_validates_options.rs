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
    let mut program = String::from("require \"active_model\"\n");
    for (path, source) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, source).unwrap();
        program.push_str(&format!("require_relative {:?}\n", path.trim_end_matches(".rb")));
    }
    program.push_str(script);
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let mut command = Command::new("ruby");
    for lib in ["activesupport", "activemodel"] {
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

/// What ingest does not model is a `check` error at the call, never a
/// dropped check.
#[test]
fn unmodeled_validates_options_are_check_errors() {
    let ticket = r##"class Ticket
  include ActiveModel::Validations

  attr_reader :code, :seats, :email, :kind, :note

  validates :code, uniqueness: true
  validates :seats, numericality: { in: 1..3 }
  validates :email, presence: true, on: :create
  validates :kind, inclusion: { in: ->(t) { %w[a b] } }
  validates :note, length: { is: 4 }
  validates :email, format: { with: /^x/ }
  validates :seats, numericality: { greater_than: 0, message: "must exceed %{count}" }

  def initialize(code)
    @code = code
  end
end
"##;
    let (_dir, errors) = app(&[("app/models/ticket.rb", ticket)]).emit(roundhouse::project::BuildTarget::Ruby);
    for expected in [
        "`validates uniqueness:` not supported (all targets): on `code`, a uniqueness check",
        "`validates numericality:` not supported (all targets): on `seats`, `in:` option",
        "`validates presence:` not supported (all targets): on `email`, `on:` validation context",
        "`validates inclusion:` not supported (all targets): on `kind`, an `in:` lambda is not modeled",
        "`validates length:` not supported (all targets): on `note`, `is:` option",
        "`validates format:` not supported (all targets): on `email`, a regexp with a `^`/`$` anchor",
        "`validates numericality:` not supported (all targets): on `seats`, a `message:` interpolating %{count}",
    ] {
        assert!(errors.iter().any(|e| e.contains(expected)), "missing {expected:?} in:\n{}", errors.join("\n"));
    }
    assert_eq!(errors.len(), 7, "{}", errors.join("\n"));
}
