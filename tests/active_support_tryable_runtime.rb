# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveSupport::Tryable, `Object#try`/`#try!`,
# `Delegator#try` and `NilClass#try` against
# runtime/spinel/active_support_tryable.rb.
#
#   ruby tests/active_support_tryable_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# core_ext/object/try; `emitted` loads the port as the emitted PostgreSQL
# tree's boot.rb does. Each prints one line per observation (a module
# that includes ActiveSupport::Tryable itself, as api_versioning's
# VisibilityOwner does, a SimpleDelegator, nil, private methods, blocks
# of both arities, keyword arguments); `compare` requires them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "try differs (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActiveSupport::Tryable agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/core_ext/object/try"
else
  require_relative "../runtime/spinel/active_support_tryable"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.chomp.inspect}"
end

class Shop
  attr_reader :name

  def initialize(name)
    @name = name
  end

  def greet(greeting, punctuation: "!")
    "#{greeting} #{name}#{punctuation}"
  end

  def each_letter
    name.each_char.map { |c| yield c }
  end

  private

  def secret
    "hidden"
  end
end

# api_versioning's VisibilityOwner: a module that includes Tryable itself.
module VisibilityOwner
  include ActiveSupport::Tryable

  def element_identity
    "owner"
  end
end

class BareOwner < BasicObject
  include ::Kernel
  include ::VisibilityOwner
end

shop = Shop.new("acme")
show("Tryable instance methods", ActiveSupport::Tryable.instance_methods(false).sort)
show("Tryable arities", [ActiveSupport::Tryable.instance_method(:try).arity, ActiveSupport::Tryable.instance_method(:try!).arity])
show("Object includes Tryable", Object.include?(ActiveSupport::Tryable))
show("Delegator includes Tryable", Delegator.include?(ActiveSupport::Tryable))
show("Object#try owner", Object.instance_method(:try).owner.name)
show("NilClass#try owner", NilClass.instance_method(:try).owner.name)
show("NilClass#try arity", [NilClass.instance_method(:try).arity, NilClass.instance_method(:try!).arity])
show("try symbol", outcome { shop.try(:name) })
show("try string", outcome { shop.try("name") })
show("try args and kwargs", outcome { shop.try(:greet, "hi", punctuation: "?") })
show("try default kwarg", outcome { shop.try(:greet, "hi") })
show("try missing", outcome { shop.try(:nope) })
show("try private", outcome { shop.try(:secret) })
show("try with block arg", outcome { shop.try(:each_letter) { |c| c.upcase } })
show("try block arity 1", outcome { shop.try { |s| s.name.reverse } })
show("try block arity 0", outcome { shop.try { name.upcase } })
show("try no args no block", outcome { shop.try })
show("try kwargs only", outcome { shop.try(punctuation: "?") })
show("try! symbol", outcome { shop.try!(:name) })
show("try! missing", outcome { shop.try!(:nope) })
show("try! private", outcome { shop.try!(:secret) })
show("try! block arity 0", outcome { shop.try! { name.length } })
show("try! no args no block", outcome { shop.try! })
show("nil try", outcome { nil.try(:name) })
show("nil try block", outcome { nil.try { |x| x.boom } })
show("nil try!", outcome { nil.try!(:boom) })
show("nil try kwargs", outcome { nil.try(:greet, punctuation: "?") })
show("false try", outcome { false.try(:to_s) })
show("integer try", outcome { 3.try(:+, 4) })
show("integer try missing", outcome { 3.try(:upcase) })
show("hash try", outcome { { a: 1 }.try(:fetch, :a) })
show("array try block", outcome { [1, 2].try { |a| a.sum } })
delegator = SimpleDelegator.new(shop)
show("delegator try", outcome { delegator.try(:name) })
show("delegator try missing", outcome { delegator.try(:nope) })
show("delegator try block 0", outcome { delegator.try { __getobj__.name } })
show("delegator try! missing", outcome { delegator.try!(:nope) })
show("delegator try owner", Delegator.instance_method(:try).owner.name)
owner = BareOwner.new
show("bare owner try", outcome { owner.try(:element_identity) })
show("bare owner try missing", outcome { owner.try(:visibility) })
show("bare owner try!", outcome { owner.try!(:element_identity) })
show("bare owner ancestors", BareOwner.ancestors.map(&:name).take(4))
