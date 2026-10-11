# Native ActiveModel 8.2.0.alpha oracle for the validator port
# (runtime/spinel/active_model_validator.rb).
#
# Modes:
# - `native` / `emitted` print one line per observation: Validator.kind
#   and #kind, the options reader (`:class` removed, frozen), the
#   NotImplementedError each base raises, EachValidator's `:attributes`
#   handling and check_validity! hook, which values allow_nil /
#   allow_blank (true, false, a Proc, a Symbol) skip, the
#   prepare_value_for_validation hook, BlockValidator, and ancestry.
# - `compare` runs both and requires every observation equal.
#
# Every record answers read_attribute_for_validation, as every
# ActiveModel::Validations includer does natively; the port's `send`
# fallback for a record without it is a runtime-model accommodation and
# is exercised by tests/active_model_validator_runtime.rs's emitted app.

require_relative "support/rails_oracle"

CORE_GEM_HOME = RailsOracle::CORE_GEM_HOME
RAILS_CHECKOUT = RailsOracle::PINNED
RAILS_LIBS = %w[activesupport activemodel].map { |lib| File.join(RAILS_CHECKOUT, lib, "lib") }

def ruby_command(mode)
  return [{}, RbConfig.ruby] unless mode == "native"
  abort "native: Core's Rails checkout #{RAILS_CHECKOUT} is missing (set CORE_GEM_HOME)" unless File.directory?(RAILS_CHECKOUT)
  [RailsOracle.env, RbConfig.ruby, *RAILS_LIBS.flat_map { |lib| ["-I", lib] }]
end

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode|
    stdout, stderr, status = Open3.capture3(*ruby_command(mode), __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  outputs = [run.call("native"), run.call("emitted")]
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(12)
    abort "validators differ (#{a.length} native lines, #{b.length} emitted):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native ActiveModel and the emitted validators agree"
  exit
end

if ARGV.first == "native"
  require "active_model"
  abort "native: ActiveModel #{ActiveModel.gem_version}" unless ActiveModel.gem_version.to_s == "8.2.0.alpha" &&
    $LOADED_FEATURES.any? { |f| f.start_with?(RAILS_CHECKOUT) && f.end_with?("active_model/version.rb") }
  ActiveModel::BlockValidator
else
  # The tree's load order: rails.rb, the CRuby overlay's core
  # extensions, then the port (gem_facades.rb requires it where the app
  # names a validator).
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/active_support_ext")
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(root, "runtime/spinel/active_model_validator")
end

def outcome
  yield.inspect
rescue Exception => e # rubocop:disable Lint/RescueException
  "#{e.class}: #{e.message}"
end

def show(label, value)
  puts "#{label}: #{value.is_a?(String) ? value : value.inspect}"
end

class Rec
  attr_reader :log

  def initialize(values)
    @values = values
    @log = []
  end

  def read_attribute_for_validation(attribute)
    @log << [:read, attribute]
    @values[attribute]
  end

  def flag?
    @values[:flag]
  end
end

class PresenceProbeValidator < ActiveModel::EachValidator
  def validate_each(record, attribute, value)
    record.log << [attribute, value]
  end
end

module Probes
  class HTTPURLValidator < ActiveModel::Validator
    def validate(record)
      record.log << [:whole, options]
    end
  end

  class StrictValidator < ActiveModel::EachValidator
    def check_validity!
      raise ArgumentError, "need :max" unless options.key?(:max)
    end

    def validate_each(record, attribute, value)
      record.log << [attribute, value, options[:max]]
    end
  end

  class PreparingValidator < ActiveModel::EachValidator
    def validate_each(record, attribute, value)
      record.log << [attribute, value]
    end

    private

    def prepare_value_for_validation(value, record, attr_name)
      "#{attr_name}=#{value.inspect}"
    end
  end
end

show "kind", [PresenceProbeValidator.kind, Probes::HTTPURLValidator.kind, Probes::StrictValidator.kind,
  ActiveModel::Validator.kind, ActiveModel::EachValidator.kind, ActiveModel::BlockValidator.kind,
  Class.new(ActiveModel::Validator).kind, Class.new(PresenceProbeValidator).kind]
show "instance kind", [Probes::HTTPURLValidator.new.kind, PresenceProbeValidator.new(attributes: [:a]).kind]
show "ancestors", [ActiveModel::EachValidator.superclass, ActiveModel::BlockValidator.superclass,
  PresenceProbeValidator.ancestors.take_while { |c| c != Object }]

opts = { max: 3, class: String, if: :flag? }
v = Probes::HTTPURLValidator.new(opts)
show "options", [v.options, v.options.frozen?, opts, v.options.equal?(opts)]
show "options default", outcome { ActiveModel::Validator.new.options }
show "options frozen write", outcome { v.options[:x] = 1 }
show "Validator#validate", outcome { ActiveModel::Validator.new.validate(Rec.new({})) }
show "validate_each base", outcome { ActiveModel::EachValidator.new(attributes: [:a]).validate(Rec.new(a: 1)) }
show "validate whole", outcome { r = Rec.new({}); v.validate(r); r.log }

passed = { attributes: [:a, :b], max: 2 }
ev = Probes::StrictValidator.new(passed)
show "each attributes", [ev.attributes, ev.options, ev.options.frozen?, passed]
show "each single attribute", outcome { PresenceProbeValidator.new(attributes: :a).attributes }
show "each attributes blank", outcome { PresenceProbeValidator.new(attributes: []) }
show "each attributes missing", outcome { PresenceProbeValidator.new({}) }
show "each attributes nil", outcome { PresenceProbeValidator.new(attributes: nil) }
show "each no options", outcome { PresenceProbeValidator.new }
show "check_validity!", outcome { Probes::StrictValidator.new(attributes: [:a]) }
show "check_validity! ok", outcome { Probes::StrictValidator.new(attributes: [:a], max: 1).options }
show "each validate", outcome { r = Rec.new(a: 1, b: nil); ev.validate(r); r.log }

VALUES = [nil, "", "  ", "\t\n", "x", 0, 1.5, [], [nil], {}, { a: 1 }, false, true, :sym]
OPTION_SETS = [
  {},
  { allow_nil: true },
  { allow_nil: false },
  { allow_blank: true },
  { allow_blank: false },
  { allow_nil: true, allow_blank: false },
  { allow_nil: false, allow_blank: true },
  { allow_nil: :flag? },
  { allow_blank: :flag? },
  { allow_nil: ->(record) { record.flag? } },
  { allow_blank: proc { |record| !record.flag? } },
  { allow_nil: "yes" },
]
OPTION_SETS.each do |set|
  [false, true].each do |flag|
    VALUES.each do |value|
      show "allow #{set.transform_values { |o| o.is_a?(Proc) ? (o.lambda? ? :a_lambda : :a_proc) : o }.inspect} flag=#{flag} value=#{value.inspect}", outcome {
        r = Rec.new(a: value, flag: flag)
        PresenceProbeValidator.new(set.merge(attributes: [:a])).validate(r)
        r.log
      }
    end
  end
end

show "prepare_value_for_validation", outcome { r = Rec.new(a: 5, b: nil); Probes::PreparingValidator.new(attributes: [:a, :b]).validate(r); r.log }
show "prepare_value_for_validation allow_nil", outcome { r = Rec.new(a: 5, b: nil); Probes::PreparingValidator.new(attributes: [:a, :b], allow_nil: true).validate(r); r.log }

seen = []
bv = ActiveModel::BlockValidator.new(attributes: [:a, :b], allow_blank: true) { |record, attribute, value| seen << [record.class, attribute, value] }
show "block validate", outcome { bv.validate(Rec.new(a: "x", b: " ")); seen }
show "block options", [bv.options, bv.attributes, bv.kind]
show "block validate_each private", [ActiveModel::BlockValidator.private_method_defined?(:validate_each),
  ActiveModel::EachValidator.public_method_defined?(:validate_each),
  ActiveModel::EachValidator.private_method_defined?(:allow_nil?),
  ActiveModel::EachValidator.private_method_defined?(:prepare_value_for_validation)]
show "block no block", outcome { b = ActiveModel::BlockValidator.new(attributes: [:a]); b.validate(Rec.new(a: 1)) }
