# Runtime probe for tests/futamura.rs (hoisted_values): the emitted tree on real ActiveRecord
# (SQLite). usage: ruby probe.rb EMITTED_DIR   (FUTAMURA_VERIFY=1 optional)
#
# A `where` value that is a call is evaluated by Ruby once, when it builds the hash, in source order.
# The site evaluates it once ahead of the call; the fallback (a verify shadow, a redefined `where`,
# a value the statement cannot bind) reads the result back instead of calling again. `Lookup.calls`
# records every evaluation, so a value evaluated twice, or out of order, shows.
require "active_record"
require "concurrent"
require "set"
require "tmpdir"

dir = ARGV[0]
ENV["FUTAMURA_STATS"] = Dir.mktmpdir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(dir, "db/schema.rb")
%w[application_record widget].each { |m| load File.join(dir, "app/models/#{m}.rb") }
module Rails
  Config = Struct.new(:unused) { def to_prepare(&block) = block.call }
  def self.application = Struct.new(:config).new(Config.new)
end
load File.join(dir, "config/initializers/futamura.rb")
load File.join(dir, "app/services/lookup.rb")

%w[a b c].each { |n| Widget.create!(shop_id: 1, kind: "k", name: n) }
Widget.create!(shop_id: 1, kind: "z", name: "d")
Widget.create!(shop_id: 2, kind: "k", name: "e")

def show(name)
  Lookup.calls.clear
  Lookup.instance_variable_set(:@bump, 0)
  result = Array(yield).map { |r| r.respond_to?(:name) ? r.name : r }
  puts "#{name}=#{result.inspect} calls=#{Lookup.calls.inspect}"
end

show("by_kind") { Lookup.by_kind("K") }
show("ordered") { Lookup.ordered("A", "K") }
show("listed") { Lookup.listed(%w[a c]) }
show("block") { Lookup.listed_with_block([[1, "b"], [2, "c"]]) }
show("first_named") { Lookup.first_named("B") }
show("nil_value") { Lookup.by_kind(nil) }
show("by_bump") { Lookup.by_bump }
show("by_shorthand") { Lookup.by_shorthand }
show("paged") { Lookup.paged }
show("with_scope") { Lookup.with_scope }
show("with_scope_twice") { Lookup.with_scope_twice }
show("with_scope_unused") { Lookup.with_scope_unused }
show("captured") { Lookup.captured }
show("captured_shorthand") { Lookup.captured_shorthand }
show("captured_in_block") { Lookup.captured_in_block }
# `where` redefined on the model: the site runs Rails' chain, which reads the hoisted value.
Widget.singleton_class.prepend(Module.new { def where(*args) = super })
show("redefined_where") { Lookup.by_kind("K") }
show("fallback_by_bump") { Lookup.by_bump }
show("fallback_captured") { Lookup.captured }
sites = Futamura::Stats.sites.values
puts "specialized=#{sites.sum(&:specialized)}"
