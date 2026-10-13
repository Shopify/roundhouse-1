# Runtime probe for tests/futamura.rs (loaded_targets): the emitted tree on real ActiveRecord
# (SQLite). usage: ruby probe.rb EMITTED_DIR
# Each scenario runs once through the emitted Finder and once through Rails' own reader on a fresh
# owner; the state after (results, whether the association loaded, its target, the inverse, the
# `Message Load` queries) must be the same. Under FUTAMURA_VERIFY=1 every specialized call is also
# compared with Rails' chain, and the target it set with the one Rails' read set.
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

def state(room, result, name = :messages)
  proxy = room.association(name)
  [
    Array(result).map { |m| m.respond_to?(:id) ? m.id : m },
    proxy.loaded?,
    proxy.loaded? ? proxy.target.map(&:id) : nil,
    proxy.loaded? ? proxy.target.all? { |m| m.association(:room).loaded? && m.room.equal?(room) } : nil,
    result.is_a?(Array) && proxy.loaded? ? !result.equal?(proxy.target) : nil
  ].inspect
end

def totals
  sites = Futamura::Stats.sites.values
  [sites.sum(&:specialized), sites.sum { |s| s.fallback.values.sum }]
end

def scenario(name, finder_call, rails_call, association: :messages, prepare: ->(r) {}, extra: ->(r) { nil })
  lines = %i[finder rails].map do |mode|
    owner = Room.find(1)
    prepare.call(owner)
    result = nil
    before = totals
    seen = queries do
      result = begin
        (mode == :finder ? finder_call : rails_call).call(owner)
      rescue => e
        e.class.name
      end
    end
    after = totals
    "#{state(owner, result, association)} extra=#{extra.call(owner).inspect} queries=#{seen} spec=#{after[0] - before[0]} fall=#{after[1] - before[1]}"
  end
  # Verify mode runs Rails' chain at every call, so only the results and queries are compared.
  same = lines.map { |l| l.split(" spec=").first }.uniq.size == 1
  puts "#{name} same=#{same} #{lines[0]}"
end

twice = ->(r) { Finder.loaded(r); Finder.loaded(r) }
rails_twice = ->(r) { r.messages.to_a; r.messages.to_a }
scenario("twice", twice, rails_twice)
scenario("to_a_then_reads", ->(r) { Finder.loaded(r); [r.messages.size, r.messages.any?, r.messages.first&.id, r.messages.last&.id] },
         ->(r) { r.messages.to_a; [r.messages.size, r.messages.any?, r.messages.first&.id, r.messages.last&.id] })
scenario("first_does_not_load", ->(r) { Finder.first_one(r) }, ->(r) { r.messages.first })
scenario("first_then_to_a", ->(r) { Finder.first_one(r); Finder.loaded(r) }, ->(r) { r.messages.first; r.messages.to_a })
scenario("relation_does_not_load", ->(r) { Finder.linked(r) }, ->(r) { r.messages.order(:id).to_a })
scenario("scope_lambda_loads", ->(r) { Finder.loaded_ordered(r) }, ->(r) { r.ordered_messages.to_a }, association: :ordered_messages)
scenario("new_record_before_load", ->(r) { Finder.loaded(r) }, ->(r) { r.messages.to_a },
         prepare: ->(r) { r.messages.build(body: "unsaved") })
scenario("new_owner", ->(r) { Finder.loaded(Room.new) }, ->(r) { Room.new.messages.to_a })
scenario("strict_loading", ->(r) { Finder.loaded(r) }, ->(r) { r.messages.to_a }, prepare: ->(r) { r.strict_loading!(true) })
scenario("n_plus_one_strict", ->(r) { Finder.loaded(r) }, ->(r) { r.messages.to_a },
         prepare: ->(r) { r.strict_loading!(true, mode: :n_plus_one_only) },
         extra: ->(r) { r.association(:messages).target.map(&:strict_loading?) })
