# Runtime probe for tests/futamura.rs (strict_loading_terminals): the emitted tree on real
# ActiveRecord (SQLite). usage: ruby probe.rb EMITTED_DIR
# Every terminal the specializer models on an association reader, in every strict-loading mode
# and target state, runs once through the emitted Finder and once through Rails' own reader on a
# fresh owner. The results (or the error class), the loaded flag, the target and the `Message Load`
# queries must be the same. Under FUTAMURA_VERIFY=1 every specialized call is also compared with
# Rails' chain.
require "active_record"
require "concurrent"
require "set"
require "tmpdir"

dir = ARGV[0]
ENV["FUTAMURA_STATS"] = Dir.mktmpdir
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(dir, "db/schema.rb")
%w[application_record room message].each { |m| load File.join(dir, "app/models/#{m}.rb") }
module Rails
  Config = Struct.new(:unused) { def to_prepare(&block) = block.call }
  def self.application = Struct.new(:config).new(Config.new)
end
load File.join(dir, "config/initializers/futamura.rb")
load File.join(dir, "app/services/finder.rb")

room = Room.create!(name: "a")
Room.create!(name: "b")
3.times { |i| Message.create!(room_id: room.id, body: "m#{i}") }
Message.create!(room_id: 2, body: "other")

def queries
  count = 0
  subscriber = ActiveSupport::Notifications.subscribe("sql.active_record") { |*, payload| count += 1 if payload[:name] == "Message Load" }
  yield
  count
ensure
  ActiveSupport::Notifications.unsubscribe(subscriber)
end

def totals
  sites = Futamura::Stats.sites.values
  [sites.sum(&:specialized), sites.sum { |s| s.fallback.values.sum }]
end

def ids(result)
  Array(result).map { |m| m.respond_to?(:id) ? m.id : m }
end

# The state after a call: result, loaded flag, target, whether the records read were strict.
def state(owner, name, result)
  proxy = owner.association(name)
  strict = result.is_a?(Array) ? result.map(&:strict_loading?) : (result.respond_to?(:strict_loading?) ? [result.strict_loading?] : nil)
  [ids(result), proxy.loaded?, proxy.loaded? ? proxy.target.map(&:id) : nil, strict]
end

TERMINALS = {
  # name => [association, finder call, rails call]
  "first" => [:messages, ->(r) { Finder.first_one(r) }, ->(r) { r.messages.first }],
  "last" => [:messages, ->(r) { Finder.last_one(r) }, ->(r) { r.messages.last }],
  "take" => [:messages, ->(r) { Finder.take_one(r) }, ->(r) { r.messages.take }],
  "first_n" => [:messages, ->(r) { Finder.first_two(r) }, ->(r) { r.messages.first(2) }],
  "last_n" => [:messages, ->(r) { Finder.last_two(r) }, ->(r) { r.messages.last(2) }],
  "find_by" => [:messages, ->(r) { Finder.found_by(r) }, ->(r) { r.messages.find_by(body: "m1") }],
  "linked_first" => [:messages, ->(r) { Finder.linked_first(r) }, ->(r) { r.messages.order(:id).first }],
  "to_a" => [:messages, ->(r) { Finder.loaded(r) }, ->(r) { r.messages.to_a }],
  "reflection_first" => [:strict_messages, ->(r) { Finder.reflection_first(r) }, ->(r) { r.strict_messages.first }],
  "reflection_last" => [:strict_messages, ->(r) { Finder.reflection_last(r) }, ->(r) { r.strict_messages.last }],
  "reflection_take" => [:strict_messages, ->(r) { Finder.reflection_take(r) }, ->(r) { r.strict_messages.take }],
  "reflection_to_a" => [:strict_messages, ->(r) { Finder.reflection_to_a(r) }, ->(r) { r.strict_messages.to_a }],
  "reflection_find_by" => [:strict_messages, ->(r) { Finder.reflection_found_by(r) }, ->(r) { r.strict_messages.find_by(body: "m1") }]
}

OWNERS = {
  "plain" => ->(r) {},
  "strict_all" => ->(r) { r.strict_loading!(true) },
  "strict_n_plus_one" => ->(r) { r.strict_loading!(true, mode: :n_plus_one_only) },
  "strict_all_off" => ->(r) { r.strict_loading!(false) }
}

TARGETS = {
  "unloaded" => ->(r, name) {},
  "loaded" => ->(r, name) { r.association(name).send(:skip_strict_loading) { r.association(name).load_target } },
  "new_record" => ->(r, name) { r.association(name).build(body: "unsaved") },
  "changed_record" => ->(r, name) { record = Message.find(1); record.body = "changed"; r.association(name).add_to_target(record) }
}

def run(mode, owner_prep, target_prep, association, finder, rails, owner_class = Room)
  owner = owner_class.find(1)
  owner_prep.call(owner)
  target_prep.call(owner, association)
  result = nil
  before = totals
  seen = queries do
    result = begin
      (mode == :finder ? finder : rails).call(owner)
    rescue => e
      e.class.name
    end
  end
  after = totals
  "#{state(owner, association, result).inspect} queries=#{seen} spec=#{after[0] - before[0]}"
end

def scenario(name, association, finder, rails, owner_prep, target_prep)
  lines = %i[finder rails].map { |mode| run(mode, owner_prep, target_prep, association, finder, rails) }
  same = lines.map { |l| l.split(" spec=").first }.uniq.size == 1
  puts "#{name} same=#{same} #{lines[0]}"
end

TERMINALS.each do |tname, (association, finder, rails)|
  OWNERS.each do |oname, owner_prep|
    TARGETS.each do |gname, target_prep|
      next if gname == "changed_record" && oname == "strict_all" # loading the target to change it is the strict read itself
      scenario("#{tname}/#{oname}/#{gname}", association, finder, rails, owner_prep, target_prep)
    end
  end
end

# A new owner reads its target; a strict-loading-by-default class has strict owners from `find`.
scenario("first/new_owner", :messages, ->(_) { Finder.first_one(Room.new) }, ->(_) { Room.new.messages.first }, ->(r) {}, ->(r, n) {})
scenario("to_a/new_owner", :messages, ->(_) { Finder.loaded(Room.new) }, ->(_) { Room.new.messages.to_a }, ->(r) {}, ->(r, n) {})
Room.strict_loading_by_default = true
scenario("first/by_default", :messages, ->(r) { Finder.first_one(r) }, ->(r) { r.messages.first }, ->(r) {}, ->(r, n) {})
scenario("last/by_default", :messages, ->(r) { Finder.last_one(r) }, ->(r) { r.messages.last }, ->(r) {}, ->(r, n) {})
scenario("to_a/by_default", :messages, ->(r) { Finder.loaded(r) }, ->(r) { r.messages.to_a }, ->(r) {}, ->(r, n) {})
Room.strict_loading_by_default = false
Room.strict_loading_by_default = true
Room.strict_loading_mode = :n_plus_one_only
scenario("first/by_default_n_plus_one", :messages, ->(r) { Finder.first_one(r) }, ->(r) { r.messages.first }, ->(r) {}, ->(r, n) {})
scenario("to_a/by_default_n_plus_one", :messages, ->(r) { Finder.loaded(r) }, ->(r) { r.messages.to_a }, ->(r) {}, ->(r, n) {})
