# Runtime probe for tests/futamura.rs (association_scopes_and_conditions): the
# emitted tree on real ActiveRecord (SQLite). usage: ruby probe.rb EMITTED_DIR
# Every call goes through Futamura.records, which under FUTAMURA_VERIFY=1 raises
# when the specialized statement and Rails' own chain disagree.
require "active_record"
require "concurrent"
require "set"
require "tmpdir"

dir = ARGV[0]
ENV["FUTAMURA_STATS"] = Dir.mktmpdir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(dir, "db/schema.rb")
%w[application_record room message note remark].each { |m| load File.join(dir, "app/models/#{m}.rb") }
# The initializer registers its statements in a to_prepare hook; with no
# application booted the hook has nothing to wait for, so it runs at once.
module Rails
  Config = Struct.new(:unused) { def to_prepare(&block) = block.call }
  def self.application = Struct.new(:config).new(Config.new)
end
load File.join(dir, "config/initializers/futamura.rb")
load File.join(dir, "app/services/finder.rb")

room = Room.create!(name: "a", shop_id: 1)
other = Room.create!(name: "b", shop_id: 1)
[[room, "alpha", "visible", true], [room, nil, "hidden", false], [room, "abc", "visible", true], [other, "alpha", "hidden", true]].each do |r, body, kind, pinned|
  Message.create!(room_id: r.id, shop_id: 1, body: body, kind: kind, pinned: pinned)
end
Note.create!(subject_id: room.id, subject_type: "Room", text: "mine")
Note.create!(subject_id: room.id, subject_type: "Other", text: "not mine")
Remark.unscoped { Remark.create!(room_id: room.id, pinned: true); Remark.create!(room_id: room.id, pinned: false) }

def totals
  sites = Futamura::Stats.sites.values
  [sites.sum(&:specialized), sites.sum { |s| s.fallback.values.sum }]
end

def show(name)
  before = totals
  result = yield
  after = totals
  ids = Array(result).map { |r| r.respond_to?(:id) ? r.id : r }
  puts "#{name}=#{ids.inspect} spec=#{after[0] - before[0]} fall=#{after[1] - before[1]}"
end

show("not_blank") { Finder.not_blank(room) }
show("pinned") { Finder.pinned(room) }
show("first_pinned") { Finder.first_pinned(room) }
show("newest") { Finder.newest(room) }
show("notes") { Finder.notes(room) }
show("same_column") { Finder.same_column(room) }
show("fragment") { Finder.fragment(room.id) }
show("kinds") { Finder.kinds }
show("kinds_not") { Finder.kinds_not }
show("not_fragment") { Finder.not_fragment }
# The association's own scope replaces the default scope's condition on the
# column both name (Association#scope merges them), so the chain is Rails'.
show("remarks") { Finder.remarks(room) }
# A loaded association answers from memory, and one with an unsaved record
# merges it in: CollectionProxy does not query, so neither does the statement.
loaded = Room.find(room.id)
loaded.pinned.load
show("loaded") { Finder.pinned(loaded) }
building = Room.find(room.id)
building.pinned.build(room_id: room.id, body: "unsaved", pinned: true)
show("unsaved") { Finder.pinned(building) }
puts "unsaved_size=#{Finder.pinned(building).size}"
# A has_many the scope of which reads the owner stays Rails'.
puts "scoped=#{Finder.scoped(room).map(&:id).inspect}"
