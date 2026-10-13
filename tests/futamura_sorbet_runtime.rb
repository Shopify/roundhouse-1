# Runtime probe for tests/futamura.rs (sorbet_runtime): real ActiveRecord on SQLite, with sorbet-runtime.
# usage: ruby sorbet_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
#
# sorbet-runtime replaces a method a `sig` was written for by a wrapper (CallValidation): an
# `abstract` wrapper forwards to `super`, a checked one validates arguments and the return
# value around the original method. A guard that reads the wrapper's own source location sees a
# method that is not ActiveRecord's, though the call only reaches ActiveRecord's reader or
# class method. The guard looks through a wrapper that can only forward, and not through one
# that can raise or change the value, or one that forwards to something else.
#
# sorbet-runtime comes from the gem when it loads (SORBET_RUNTIME_LIB adds its lib directory), else
# from sorbet_reproduction.rb: the shape of the wrappers and the signature API the guard reads.
$LOAD_PATH.unshift(ENV["SORBET_RUNTIME_LIB"]) if ENV["SORBET_RUNTIME_LIB"]
begin
  require "sorbet-runtime"
  sorbet = "gem"
rescue LoadError
  require_relative "futamura_sorbet_reproduction"
  sorbet = "reproduction"
end
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:rooms) { |t| t.string :name }
  create_table(:twigs) { |t| t.string :code; t.integer :room_id }
  create_table(:bits) { |t| t.string :code }
end
class ApplicationRecord < ActiveRecord::Base
  extend T::Sig
  self.abstract_class = true
end
class Twig < ApplicationRecord; end

# An interface declares the reader abstract; the class's own has_many reader (in
# GeneratedAssociationMethods, below every module the class includes) implements it.
module Readable
  extend T::Sig
  extend T::Helpers
  abstract!
  sig { abstract.returns(T.untyped) }
  def twigs; end
end
# A module below the interface that defines the reader itself.
module OwnTwigs
  def twigs = Twig.where(code: "x")
end
# A checked sig around the reader Rails generated: untyped, typed, and void.
module Checked
  extend T::Sig
  def self.included(base)
    base.class_eval do
      reader = instance_method(:twigs)
      sig { returns(T.untyped) }
      define_method(:twigs, reader)
    end
  end
end
module CheckedTyped
  extend T::Sig
  def self.included(base)
    base.class_eval do
      reader = instance_method(:twigs)
      sig { returns(Object) }
      define_method(:twigs, reader)
    end
  end
end
module CheckedVoid
  extend T::Sig
  def self.included(base)
    base.class_eval do
      reader = instance_method(:twigs)
      sig { void }
      define_method(:twigs, reader)
    end
  end
end
class Room < ApplicationRecord
  has_many :twigs
end
class AbstractRoom < ApplicationRecord
  self.table_name = "rooms"
  has_many :twigs, foreign_key: :room_id
  include Readable
end
class OverriddenRoom < ApplicationRecord
  self.table_name = "rooms"
  has_many :twigs, foreign_key: :room_id
  include OwnTwigs
  include Readable
end
class CheckedRoom < ApplicationRecord
  self.table_name = "rooms"
  has_many :twigs, foreign_key: :room_id
  include Checked
end
class TypedRoom < ApplicationRecord
  self.table_name = "rooms"
  has_many :twigs, foreign_key: :room_id
  include CheckedTyped
end
class VoidRoom < ApplicationRecord
  self.table_name = "rooms"
  has_many :twigs, foreign_key: :room_id
  include CheckedVoid
end
# A class method an extended module declares abstract: Rails' `unscoped` is behind it.
module ScopedAbstract
  extend T::Sig
  extend T::Helpers
  abstract!
  sig { abstract.params(block: T.untyped).returns(T.untyped) }
  def unscoped(&block); end
end
class Bit < ApplicationRecord
  extend ScopedAbstract
end

load ARGV[0]
%w[room abstract_room overridden_room checked_room typed_room].each do |name|
  Futamura.define(:"#{name}_twigs", "Twig", one: true, relation: true, via: :where, owner_keys: ["room_id"], owner_columns: ["room_id"]) { |p| Twig.all.where(room_id: p.bind).where(code: p.bind).limit(1) }
end
Futamura.define(:bit_unscoped, "Bit", one: true, unscoped: true, via: :unscoped) { |p| Bit.unscoped.where(code: p.bind).limit(1) }

$fallbacks = 0
def run(key, binds, **opts, &chain)
  Futamura.records(key, binds, **opts) { $fallbacks += 1; chain.call }&.id
end
def counted
  before = $fallbacks
  result = yield
  [result, $fallbacks - before]
end

out = { "sorbet" => sorbet }
Twig.create!(code: "y", room_id: 1)
Twig.create!(code: "x", room_id: 2)
[Room, AbstractRoom, OverriddenRoom, CheckedRoom, TypedRoom, VoidRoom].each_with_index { |k, i| k.create!(name: "r#{i}") if k.count.zero? }
Bit.create!(code: "x")
%w[room abstract_room overridden_room checked_room typed_room].each do |name|
  klass = Object.const_get(name.split("_").map(&:capitalize).join)
  owner = -> { klass.find(1) }
  # Twice: the first call reaches the sig's wrapper before it has run (sorbet's first-call
  # wrapper), the second after it replaced itself.
  out[name] = 2.times.map { counted { run(:"#{name}_twigs", [1, "y"], owner: owner.call, association: :twigs) { owner.call.twigs.where(code: "y").first } } }
end
out["unscoped"] = 2.times.map { counted { run(:bit_unscoped, ["x"]) { Bit.unscoped.where(code: "x").first } } }
# A `void` sig replaces the reader's value with sorbet's VOID, so no chain can run on it: the guard
# refuses the method itself.
out["void_reader_rails"] = Futamura.send(:rails_method?, VoidRoom.instance_method(:twigs))
out.each { |k, v| puts "#{k}=#{v.is_a?(String) ? v : v.inspect}" }
