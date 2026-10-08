# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for the ActiveModel::Validations port of the
# PostgreSQL CRuby tree: runtime/spinel/active_model_validations.rb
# (Validations, its validators and helpers, Validations::Callbacks,
# Errors, Error, NestedError, Translation, ActiveModel::Callbacks,
# ValidationError) and active_support_option_merger.rb (OptionMerger,
# Object#with_options).
#
#   ruby tests/active_model_validations_port.rb compare
#
# runs this script twice: `native` requires the pinned activemodel's
# active_model (with active_support/core_ext and activemodel's English
# locale, as a Rails app has them); `emitted` loads the port with what
# the emitted PostgreSQL tree's runtime/gem_facades.rb loads before it
# (the CRuby overlay's core_ext, Module#delegate, the Inflector, the i18n
# port and the framework locales), on Core's gem home alone
# (bin/run-target's GEM_PATH). Each prints one line per observation and
# `compare` requires them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = {}
    if mode == "emitted"
      env = RailsOracle.core_gems_env.merge("GEM_HOME" => RailsOracle::CORE_GEM_HOME, "GEM_PATH" => RailsOracle::CORE_GEM_HOME)
    end
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "ports differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and the emitted ActiveModel::Validations port agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport", "activemodel")
  require "active_support"
  require "active_support/core_ext"
  require "active_model"
  # What active_model/railtie's `:i18n` hook adds (active_model.rb:84).
  I18n.load_path << File.join(Gem.loaded_specs["activemodel"].full_gem_path, "lib/active_model/locale/en.yml")
else
  runtime = File.expand_path("../runtime/spinel", __dir__)
  require File.join(runtime, "scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(runtime, "module_delegate")
  require File.join(runtime, "active_support_inflector")
  require File.join(runtime, "active_support_i18n")
  require File.join(runtime, "active_support_string_inflections")
  require File.join(runtime, "rails_framework_locales")
  require File.join(runtime, "active_model_validations")
end
I18n.enforce_available_locales = false

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.chomp.gsub(/0x\h+/, "0x").inspect}"
end

def report(label, record)
  valid = outcome { record.valid? }
  show(label, [valid, record.errors.full_messages, record.errors.details, record.errors.attribute_names])
end

class Signup
  include ActiveModel::Validations

  attr_accessor :name, :email, :age, :code, :kind, :terms, :password, :password_confirmation, :starts, :ends, :nick

  def initialize(**attrs)
    attrs.each { |k, v| public_send("#{k}=", v) }
  end

  validates :name, presence: true, length: { maximum: 5 }
  validates :email, format: { with: /\A[^@\s]+@[^@\s]+\z/ }, allow_nil: true
  validates :age, numericality: { only_integer: true, greater_than: 17, less_than_or_equal_to: 120 }, allow_blank: true
  validates :code, length: { is: 3 }, allow_nil: true
  validates :kind, inclusion: { in: %w[basic pro] }, allow_nil: true
  validates :nick, exclusion: { in: %w[admin root] }, absence: { if: -> { kind == "basic" } }
  validates :terms, acceptance: true
  validates :password, confirmation: true, length: { in: 4..8 }, allow_nil: true
  validates :ends, comparison: { greater_than: :starts }, allow_nil: true
  validate :name_not_banned
  validate :email_unique, on: :create
  validate do
    errors.add(:base, "No robots") if name == "robot"
  end

  def name_not_banned
    errors.add(:name, :exclusion, message: "is banned (%{value})") if name == "mallory"
  end

  def email_unique
    errors.add(:email, :taken, message: "has already been taken") if email == "dup@x.io"
  end
end

class VipSignup < Signup
  validates :email, presence: true
  validates_each :code do |record, attr, value|
    record.errors.add(attr, "must start with V") if value && !value.start_with?("V")
  end
end

class HandleValidator < ActiveModel::EachValidator
  def validate_each(record, attribute, value)
    record.errors.add(attribute, options[:message] || :invalid) unless value.to_s.match?(/\A[a-z0-9-]+\z/)
  end
end

class RecordShape < ActiveModel::Validator
  def validate(record)
    record.errors.add(:base, :invalid) if record.handle.to_s.length > 10
  end
end

class Change
  include ActiveModel::Validations

  attr_accessor :handle, :breaking, :summary, :reconnaissance, :team, :version

  def initialize(**attrs)
    attrs.each { |k, v| public_send("#{k}=", v) }
  end

  validates :handle, presence: true, handle: true
  validates_with RecordShape
  validates :reconnaissance, inclusion: { in: [true, false] }, allow_nil: false
  validates :breaking, inclusion: { in: [true] }, allow_nil: false, if: :reconnaissance
  with_options unless: :reconnaissance do
    validates :breaking, inclusion: { in: [true, false] }, allow_nil: false
    validates :summary, presence: true
    validate :summary_is_sentence
    validates :team, presence: true, unless: -> { handle == "solo" }
  end
  validates :version, numericality: { odd: true, in: 1..9 }, allow_nil: true

  def summary_is_sentence
    errors.add(:summary, "must end with a period") if summary && !summary.end_with?(".")
  end
end

class Hooked
  include ActiveModel::Validations
  include ActiveModel::Validations::Callbacks

  attr_accessor :title, :log

  before_validation { (self.log ||= []) << "before" ; self.title = title.to_s.strip }
  after_validation { log << "after #{errors.count}" }
  validates :title, presence: true
end

class Frozen
  include ActiveModel::Validations

  attr_reader :handle

  def initialize(handle)
    @handle = handle
    freeze
  end

  validates :handle, presence: true
end

class Strict
  include ActiveModel::Validations

  attr_accessor :id

  validates! :id, presence: true
end

# ---- the rules, the messages and the errors
report("signup ok", Signup.new(name: "ann", email: "a@b.io", age: "30", terms: "1"))
report("signup blank", Signup.new(name: "", terms: false))
report("signup long", Signup.new(name: "annabelle", email: "nope", age: "17.5", code: "ab", kind: "gold", terms: "1"))
report("signup numbers", Signup.new(name: "ann", age: "15", terms: "1"))
report("signup numbers high", Signup.new(name: "ann", age: 200, terms: "1"))
report("signup not a number", Signup.new(name: "ann", age: "abc", terms: "1"))
report("signup nick", Signup.new(name: "ann", nick: "root", kind: "basic"))
report("signup password", Signup.new(name: "ann", password: "abc", password_confirmation: "abd"))
report("signup comparison", Signup.new(name: "ann", starts: 5, ends: 3))
report("signup comparison blank", Signup.new(name: "ann", ends: 3))
report("signup banned", Signup.new(name: "mallory"))
report("signup robot", Signup.new(name: "robot"))
s = Signup.new(name: "ann", email: "dup@x.io")
show("context", [s.valid?, s.valid?(:create), s.errors.full_messages, s.validation_context, s.invalid?(:create)])
s = Signup.new(name: "", email: "x")
s.valid?
e = s.errors
show("errors api", [e.size, e.count, e.empty?, e.any?, e[:name], e[:nope], e.include?(:name), e.key?(:email), e.messages, e.to_hash(true)])
show("errors where", [e.where(:name).map(&:type), e.where(:name, :blank).size, e.added?(:name, :blank), e.added?(:name, "can't be blank"), e.of_kind?(:email, :invalid), e.of_kind?(:email, :blank)])
show("errors full_messages_for", [e.full_messages_for(:name), e.messages_for(:email), e.full_message(:name, "is odd"), e.generate_message(:name, :blank), e.as_json, e.as_json(full_messages: true)])
first = e.first
show("error", [first.class, first.attribute, first.type, first.raw_type, first.options, first.message, first.full_message, first.details, first == e.first, first.match?(:name), first.strict_match?(:name, :blank)])
show("errors delete", [e.delete(:name), e.delete(:nope), e.size, e.to_a])
other = Signup.new(name: "")
other.valid?
s.errors.merge!(other.errors)
nested = s.errors.objects.last
show("errors merge", [s.errors.size, nested.class, nested.attribute, nested.message, nested.inner_error.base.equal?(other)])
s.errors.import(other.errors.first, attribute: "nick", type: "custom")
show("errors import", [s.errors.objects.last.attribute, s.errors.objects.last.type, s.errors.full_messages.last])
s.errors.add(:age, :greater_than, count: 18)
s.errors.add(:code, "is weird", strict: false)
s.errors.add(:base, :invalid)
show("errors add", [s.errors.full_messages, s.errors.group_by_attribute.keys])
dup = s.dup
show("dup", [dup.errors.equal?(s.errors), dup.errors.size])
show("inspect", [e.first.inspect.gsub(/0x\h+/, "0x").include?("@attribute=:email"), e.inspect.include?("ActiveModel::Errors")])
s.errors.clear
show("clear", [s.errors.size, s.errors.to_hash])

# ---- inheritance and introspection
report("vip blank", VipSignup.new(name: "vic", code: "AB1"))
report("vip ok", VipSignup.new(name: "vic", email: "v@x.io", code: "V12", terms: "1"))
show("validators", [Signup.validators.map(&:class).map(&:name).uniq.sort, VipSignup.validators.size - Signup.validators.size, Signup.validators_on(:name).map(&:kind), VipSignup.validators_on(:email).map(&:kind), Signup.validators_on(:age).first.options])
show("attribute_method?", [Signup.attribute_method?(:name), Signup.attribute_method?(:nope)])
show("translation", [Signup.i18n_scope, Signup.human_attribute_name(:email_address), Signup.human_attribute_name("user.first_name"), Signup.lookup_ancestors, Signup.model_name.human, VipSignup.lookup_ancestors])

# ---- validate!, strict and the ValidationError
show("validate!", outcome { Signup.new(name: "").validate! })
show("validate! ok", outcome { Signup.new(name: "ann").validate! })
show("strict", outcome { Strict.new.valid? })
err = begin; Signup.new(name: "").validate!; rescue ActiveModel::ValidationError => x; x; end
show("validation error", [err.class.superclass, err.message, err.model.class])

# ---- custom validators, with_options, validates_with
report("change blank", Change.new)
report("change recon", Change.new(handle: "x-1", reconnaissance: true, breaking: false))
report("change recon ok", Change.new(handle: "x-1", reconnaissance: true, breaking: true))
report("change full", Change.new(handle: "Bad Handle!!", reconnaissance: false, breaking: nil, summary: "Adds a thing", team: nil))
report("change solo", Change.new(handle: "solo", reconnaissance: false, breaking: false, summary: "Ok."))
report("change version", Change.new(handle: "v", reconnaissance: true, breaking: true, version: 4))
report("change version range", Change.new(handle: "v", reconnaissance: true, breaking: true, version: 11))

# ---- callbacks, frozen records
h = Hooked.new
h.title = "  "
show("hooked", [h.valid?, h.log, h.title, h.errors.full_messages])
h.title = " Hi "
show("hooked ok", [h.valid?, h.log, h.title])
f = Frozen.new(nil)
show("frozen", [f.frozen?, f.valid?, f.errors.full_messages, f.valid?, Frozen.new("x").valid?])

# ---- with_options outside validations
class Opts
  def self.call(*args, **opts) = [args, opts]
end
show("with_options", [Opts.with_options(a: 1, b: { c: 2 }) { |o| o.call(:x, b: { d: 3 }) }, Opts.with_options(a: 1).call(:y)])
show("unknown key", outcome { Class.new { include ActiveModel::Validations; validate :x, bogus: true } })
show("unknown validator", outcome { Class.new { include ActiveModel::Validations; validates :x, bogus: true } })
show("no validation", outcome { Class.new { include ActiveModel::Validations; validates :x } })

if MODE == "emitted" && $LOADED_FEATURES.any? { |f| f.include?("/activemodel") }
  abort "emitted: the pinned activemodel was loaded"
end
