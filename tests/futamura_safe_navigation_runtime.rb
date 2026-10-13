# Runtime probe for tests/futamura.rs (safe_navigation): real ActiveRecord on SQLite, running the
# emitted initializer and the finder. A nil owner must behave as the source says: `&.` skips only
# its own call, so `nil&.messages&.to_a` is nil and `nil&.messages.where(..)` raises.
# usage: ruby futamura_safe_navigation_runtime.rb <emitted tree dir>   (FUTAMURA_VERIFY=1 optional)
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(ARGV[0], "db/schema.rb")
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
module Rails
  PREPARE = Object.new.tap { |o| o.define_singleton_method(:to_prepare) { |&block| block.call } }
  def self.application = Struct.new(:config).new(PREPARE)
end
load File.join(ARGV[0], "config/initializers/futamura.rb")
%w[room message].each { |m| load File.join(ARGV[0], "app/models/#{m}.rb") }
load File.join(ARGV[0], "app/services/finder.rb")

room = Room.create!(name: "r")
other = Room.create!(name: "o")
%w[a b c].each { |body| room.messages.create!(body: body) }
other.messages.create!(body: "z")
# Creating through the association filled its target, which Rails answers a terminal on the reader from.
room, other = Room.find(room.id), Room.find(other.id)
fallbacks = 0
Futamura.singleton_class.prepend(Module.new do
  define_method(:records) { |key, binds, **o, &fb| super(key, binds, **o) { fallbacks += 1; $stderr.puts "fallback #{key}" if ENV["PROBE_TRACE"]; fb.call } }
end)

def outcome
  yield.then { |v| Array(v).map { |m| m.respond_to?(:body) ? m.body : m }.then { |a| v.nil? ? :nil : a } }
rescue NoMethodError => e
  [:no_method_error, e.name]
end

reader = {
  to_a: ->(r) { r&.messages&.to_a },
  link_then_first: ->(r) { r&.messages&.where(body: %w[a b])&.first },
  mid_chain: ->(r) { r&.messages.where(body: %w[b c]).first },
  terminal: ->(r) { r.messages.where(body: "c")&.first },
}
reader.each do |name, want|
  [room, other, nil].each do |r|
    got = outcome { Finder.public_send(name, r) }
    expected = outcome { want.call(r) }
    puts "#{name} #{r.nil? ? "nil" : r.id} #{got == expected ? "ok" : "MISMATCH #{got.inspect} #{expected.inspect}"} #{got.inspect}"
  end
end
[room, nil].each do |r|
  got = outcome { Finder.by_value(r) }
  expected = outcome { Message.find_by(room_id: r&.id) }
  puts "by_value #{r.nil? ? "nil" : r.id} #{got == expected ? "ok" : "MISMATCH #{got.inspect} #{expected.inspect}"} #{got.inspect}"
end
got = outcome { Finder.after_load(room) }
puts "after_load #{got == ["a"] ? "ok" : "MISMATCH #{got.inspect}"}"
puts "fallbacks=#{fallbacks}"
