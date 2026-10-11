# Native ActiveModel / ActiveRecord 8.2.0.alpha oracle for the type
# registries the port carries (the end of runtime/spinel/active_model_type.rb):
# `ActiveModel::Type.register` / `lookup` and `ActiveRecord::Type.register`
# / `lookup` / `add_modifier` with adapter-specific registrations, the
# shapes Core's initializers use (uuid_config.rb, normalized_string_type.rb,
# order_id_attribute_type.rb's block registrations).
#
# Modes: `native` / `emitted` print one line per observation; `compare`
# runs both and requires every line equal. The adapter is always passed:
# its default reads `ActiveRecord::Base.connection_db_config`, which needs a
# configured database.

require_relative "support/rails_oracle"

CORE_GEM_HOME = RailsOracle::CORE_GEM_HOME
RAILS_CHECKOUT = RailsOracle::PINNED
RAILS_LIBS = %w[activesupport activemodel activerecord].map { |lib| File.join(RAILS_CHECKOUT, lib, "lib") }

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
    abort "registries differ (#{a.length} native lines, #{b.length} emitted):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native and emitted type registries agree"
  exit
end

if ARGV.first == "native"
  require "active_record"
  # A booted app has it; the conflict message calls it.
  require "active_support/core_ext/string/filters"
  abort "native: ActiveRecord #{ActiveRecord.gem_version}" unless
    ActiveRecord.gem_version.to_s == "8.2.0.alpha" && $LOADED_FEATURES.any? { |f| f.start_with?(RAILS_CHECKOUT) }
else
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(root, "runtime/spinel/active_model_type")
end

# An app type class, like Core's Shopify::ModelUuidAsInteger.
class ProbeType < ActiveModel::Type::Value
  attr_reader :args, :kwargs

  def initialize(*args, **kwargs)
    @args = args
    @kwargs = kwargs
    super()
  end

  def type
    :probe
  end
end

# A decorator registered with add_modifier, like PostgreSQL's OID::Array.
class ProbeArray
  attr_reader :subtype

  def initialize(subtype)
    @subtype = subtype
  end
end

def show(type)
  case type
  when ProbeType then "ProbeType(args=#{type.args.inspect}, kwargs=#{type.kwargs.inspect})"
  when ProbeArray then "ProbeArray(#{show(type.subtype)})"
  else "#{type.class.name}(type=#{type.type.inspect}, limit=#{type.limit.inspect}, precision=#{type.precision.inspect}, scale=#{type.scale.inspect})"
  end
end

def observe(label)
  puts "#{label}: #{show(yield)}"
rescue StandardError => e
  puts "#{label}: #{e.class}: #{e.message}"
end

ADAPTERS = %i[postgresql trilogy sqlite3]

%i[big_integer binary boolean datetime decimal integer immutable_string string text].each do |name|
  ADAPTERS.each { |adapter| observe("AR #{name} on #{adapter}") { ActiveRecord::Type.lookup(name, adapter: adapter) } }
  observe("AR #{name} limit: 2") { ActiveRecord::Type.lookup(name, limit: 2, adapter: :postgresql) }
  observe("AR #{name} precision: 6") { ActiveRecord::Type.lookup(name, precision: 6, adapter: :postgresql) }
  observe("AM #{name}") { ActiveModel::Type.lookup(name) }
  observe("AM #{name} limit: 4") { ActiveModel::Type.lookup(name, limit: 4) }
end
observe("AR unknown") { ActiveRecord::Type.lookup(:no_such_type, adapter: :postgresql) }
observe("AM unknown") { ActiveModel::Type.lookup(:no_such_type) }

# uuid_config.rb's shape: one name, one class per adapter.
ActiveRecord::Type.register(:probe_uuid, ProbeType, adapter: :postgresql)
ActiveRecord::Type.register(:probe_uuid, ActiveRecord::Type::BigInteger, adapter: :trilogy)
ADAPTERS.each { |adapter| observe("probe_uuid on #{adapter}") { ActiveRecord::Type.lookup(:probe_uuid, adapter: adapter) } }
observe("probe_uuid with args") { ActiveRecord::Type.lookup(:probe_uuid, 7, :x, flag: true, adapter: :postgresql) }

# For every adapter.
ActiveRecord::Type.register(:probe_any, ProbeType)
ADAPTERS.each { |adapter| observe("probe_any on #{adapter}") { ActiveRecord::Type.lookup(:probe_any, adapter: adapter) } }
ActiveRecord::Type.register(:probe_any, ActiveRecord::Type::String, adapter: :sqlite3)
ADAPTERS.each { |adapter| observe("probe_any + sqlite3 on #{adapter}") { ActiveRecord::Type.lookup(:probe_any, adapter: adapter) } }

# order_id_attribute_type.rb's shape: a block registration.
ActiveRecord::Type.register(:probe_block, adapter: :postgresql) do |name, *args, **kwargs|
  ProbeType.new(name, *args, **kwargs)
end
observe("probe_block") { ActiveRecord::Type.lookup(:probe_block, 1, limit: 8, adapter: :postgresql) }
observe("probe_block on trilogy") { ActiveRecord::Type.lookup(:probe_block, adapter: :trilogy) }

# Shadowing a native name: a conflict, unless override says who wins.
ActiveRecord::Type.register(:integer, ProbeType, adapter: :postgresql)
ADAPTERS.each { |adapter| observe("integer shadowed on #{adapter}") { ActiveRecord::Type.lookup(:integer, adapter: adapter) } }
ActiveRecord::Type.register(:string, ProbeType, adapter: :postgresql, override: true)
ADAPTERS.each { |adapter| observe("string override: true on #{adapter}") { ActiveRecord::Type.lookup(:string, adapter: adapter) } }
ActiveRecord::Type.register(:text, ProbeType, adapter: :postgresql, override: false)
ADAPTERS.each { |adapter| observe("text override: false on #{adapter}") { ActiveRecord::Type.lookup(:text, adapter: adapter) } }
ActiveRecord::Type.register(:boolean, ProbeType, override: true)
ADAPTERS.each { |adapter| observe("boolean override: true everywhere on #{adapter}") { ActiveRecord::Type.lookup(:boolean, adapter: adapter) } }

# add_modifier: a decoration matched by options, on the subtype the
# other options look up.
ActiveRecord::Type.add_modifier({ array: true }, ProbeArray, adapter: :postgresql)
observe("big_integer array on postgresql") { ActiveRecord::Type.lookup(:big_integer, array: true, adapter: :postgresql) }
observe("big_integer array: false") { ActiveRecord::Type.lookup(:big_integer, array: false, adapter: :postgresql) }
observe("big_integer array on trilogy") { ActiveRecord::Type.lookup(:big_integer, array: true, adapter: :trilogy) }
observe("probe_uuid array") { ActiveRecord::Type.lookup(:probe_uuid, 3, array: true, adapter: :postgresql) }

# A copy is independent of later registrations.
copy = ActiveRecord::Type.registry.dup
ActiveRecord::Type.register(:probe_late, ProbeType)
observe("late in global") { ActiveRecord::Type.lookup(:probe_late, adapter: :postgresql) }
observe("late in copy") { copy.lookup(:probe_late, adapter: :postgresql) }

ActiveModel::Type.register(:probe_am, ProbeType)
observe("AM probe_am") { ActiveModel::Type.lookup(:probe_am, 1, a: 2) }
ActiveModel::Type.register(:probe_am_block) { |name, *args| ProbeType.new(name, *args) }
observe("AM probe_am_block") { ActiveModel::Type.lookup(:probe_am_block, :y) }
ActiveModel::Type.register(:integer, ProbeType)
observe("AM integer replaced") { ActiveModel::Type.lookup(:integer) }
observe("AR integer after AM replaced") { ActiveRecord::Type.lookup(:integer, adapter: :trilogy) }
