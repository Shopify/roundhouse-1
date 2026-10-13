# Runtime probe for tests/futamura.rs (printed_values): the emitted tree on real ActiveRecord
# (SQLite). usage: ruby probe.rb EMITTED_DIR   (FUTAMURA_VERIFY=1 optional)
#
# A constant `limit` is read per call (a test may `stub_const` it), so the site passes it fixed and the
# runtime builds one statement per value: the probe changes the constants and runs the sites again.
# The finder's constants are `private_constant`: Ruby reads one only by its bare name from inside
# the class body (`Lookup::Finder::KIND` is a NameError anywhere), so a value the site binds must
# be printed the way the source wrote it, and a constant in the statement's block must be bound.
require "active_record"
require "concurrent"
require "set"
require "tmpdir"

dir = ARGV[0]
ENV["FUTAMURA_STATS"] = Dir.mktmpdir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(dir, "db/schema.rb")
%w[application_record widget owner].each { |m| load File.join(dir, "app/models/#{m}.rb") }
module Rails
  Config = Struct.new(:unused) { def to_prepare(&block) = block.call }
  def self.application = Struct.new(:config).new(Config.new)
end
load File.join(dir, "config/initializers/futamura.rb")
load File.join(dir, "app/services/lookup/finder.rb")

owner = Owner.create!(name: "o")
other = Owner.create!(name: "p")
Widget.create!(shop_id: 1, kind: "k", name: "a")
Widget.create!(shop_id: 1, kind: "k", name: "b")
Widget.create!(shop_id: 1, kind: "k", name: "c")
Widget.create!(shop_id: 1, kind: "z", name: "d", owner_type: "Owner", owner_id: 0)
Widget.create!(shop_id: 2, kind: "k", name: "e")
Widget.create!(kind: "x", name: "f", owner_type: "Owner", owner_id: owner.id)
Widget.create!(kind: "x", name: "g", owner_type: "Owner", owner_id: other.id)

def show(name)
  puts "#{name}=#{Array(yield).map { |r| r.respond_to?(:name) ? r.name : r }.inspect}"
end

show("by_kind") { Lookup::Finder.by_kind(1) }
show("limited") { Lookup::Finder.limited(1) }
show("by_model") { Lookup::Finder.by_model(Owner) }
show("paged") { Lookup::Finder.paged(1) }
show("capped") { Lookup::Finder.capped_for(1) }
def stub_const(mod, name, value)
  old = mod.const_get(name)
  mod.send(:remove_const, name)
  mod.const_set(name, value)
  yield
ensure
  mod.send(:remove_const, name)
  mod.const_set(name, old)
end
stub_const(Lookup::Finder, :LIMIT, 1) { show("limited_stubbed") { Lookup::Finder.limited(1) } }
stub_const(Lookup::Finder, :PAGE, 3) { show("paged_stubbed") { Lookup::Finder.paged(1) } }
stub_const(Widget, :CAP, 1) { show("capped_stubbed") { Lookup::Finder.capped_for(1) } }
show("limited_restored") { Lookup::Finder.limited(1) }
sites = Futamura::Stats.sites.values
puts "specialized=#{sites.sum(&:specialized)}"
