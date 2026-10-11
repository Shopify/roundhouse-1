# Native ActiveSupport oracle for the core_ext methods Shopify Core sends
# to builtins, against the CRuby overlay
# (runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext.rb):
# `exclude?` on Array/Hash/Range/Set/String, `in?`, `presence_in`,
# `index_with`/`index_by`, `many?`, `pluck`/`pick`/`sole`, `squish`,
# `truncate*`, `starts_with?`, `String#first/last/from/to/at`,
# `in_groups_of`/`in_groups`/`split`, `extract!`/`extract_options!`,
# `slice!`/`except!`/`reverse_merge`, `deep_dup`/`duplicable?`,
# `deep_transform_values`, `compact_blank!`, `instance_values`, `inquiry`.
# The oracle is the Rails native Core runs (tests/support/rails_oracle.rb).
#
#   ruby tests/core_ext_surface_runtime.rb compare
require_relative "support/rails_oracle"
require "set"

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native = run.("native")
  overlay = run.("overlay")
  unless native == overlay
    diff = native.lines.zip(overlay.lines).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "overlay differs:\n" + diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  overlay: #{y}" }.join("\n")
  end
  puts native
  puts "Native ActiveSupport and the overlay core_ext agree (#{RailsOracle.label})"
  exit
end

case ARGV.first
when "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/core_ext"
when "overlay"
  require_relative "../runtime/spinel/active_support_string_inquirer"
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext"
else
  abort "mode: compare | native | overlay"
end

def outcome
  value = yield
  "#{value.class} #{value.inspect}"
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.strip}"
end

def show(label, &block) = puts("#{label}: #{outcome(&block)}")

Point = Struct.new(:x, :y)
class Holder
  def initialize = (@a = 1; @b = [2])
end

COLLECTIONS = {
  "[]" => -> { [] },
  "[1, 2, 3]" => -> { [1, 2, 3] },
  "[nil, false]" => -> { [nil, false] },
  "%w[a b]" => -> { %w[a b] },
  "{a: 1}" => -> { { a: 1 } },
  "1..3" => -> { 1..3 },
  "Set[1, 2]" => -> { Set[1, 2] },
}.freeze
PROBES = [1, 2, 4, nil, false, "a", :a, [:a, 1]].freeze

COLLECTIONS.each do |name, make|
  PROBES.each do |probe|
    show("#{name}.exclude?(#{probe.inspect})") { make.().exclude?(probe) }
    show("#{probe.inspect}.in?(#{name})") { probe.in?(make.()) }
    show("#{probe.inspect}.presence_in(#{name})") { probe.presence_in(make.()) }
  end
  show("#{name}.many?") { make.().many? }
  show("#{name}.many? { truthy }") { make.().many? { |x| x } }
  show("#{name}.index_with(0)") { make.().index_with(0) }
  show("#{name}.index_with { inspect }") { make.().index_with { |x| x.inspect } }
  show("#{name}.index_by { inspect }") { make.().index_by { |x| x.inspect } }
  show("#{name}.index_with.class") { make.().index_with.class }
  show("#{name}.excluding(1)") { make.().excluding(1) }
  show("#{name}.without([1, 2])") { make.().without([1, 2]) }
  show("#{name}.including(9)") { make.().including(9) }
  show("#{name}.sole") { make.().sole }
  show("#{name}.deep_dup") { make.().deep_dup }
  show("#{name}.duplicable?") { make.().duplicable? }
end
show("1.in?(1)") { 1.in?(1) }
show("1.in?(nil)") { 1.in?(nil) }
show("1.5.in?(1..2)") { 1.5.in?(1..2) }
show("'b'.in?('abc')") { "b".in?("abc") }
show("[1].sole") { [1].sole }
show("{a: 1}.sole") { { a: 1 }.sole }
show("[[1, 2]].each.sole") { [[1, 2]].each.sole }
show("Enumerable::SoleItemExpectedError") { Enumerable::SoleItemExpectedError.name }
show("[1, 2].minimum(:-@)") { [1, 2].minimum(:-@) }
show("[1, 2].maximum(:-@)") { [1, 2].maximum(:-@) }

records = [{ id: 1, name: "a" }, { id: 2, name: "b" }, { id: 3, name: "a" }]
show("records.pluck(:name)") { records.pluck(:name) }
show("records.pluck(:id, :name)") { records.pluck(:id, :name) }
show("records.pick(:name)") { records.pick(:name) }
show("records.pick(:id, :name)") { records.pick(:id, :name) }
show("[].pick(:id)") { [].pick(:id) }
show("points.in_order_of(:x, [3, 1])") { [Point.new(1, 0), Point.new(2, 0), Point.new(3, 0)].in_order_of(:x, [3, 1]) }
show("points.in_order_of(:x, [3, 1], filter: false)") { [Point.new(1, 0), Point.new(2, 0), Point.new(3, 0)].in_order_of(:x, [3, 1], filter: false) }

STRINGS = ["", "ab", "  a \n\t b  c ", "hello world, how are you", "héllo wörld", "a", "abc def  ghi"].freeze
STRINGS.each do |s|
  show("#{s.inspect}.exclude?('b')") { s.exclude?("b") }
  show("#{s.inspect}.exclude?('')") { s.exclude?("") }
  show("#{s.inspect}.squish") { s.squish }
  show("#{s.inspect}.squish!") { s.dup.squish! }
  show("#{s.inspect}.remove(/l/, 'o')") { s.remove(/l/, "o") }
  [0, 1, 3, 5, 8, 30].each do |n|
    show("#{s.inspect}.truncate(#{n})") { s.truncate(n) }
    show("#{s.inspect}.truncate(#{n}, separator: ' ')") { s.truncate(n, separator: " ") }
    show("#{s.inspect}.truncate(#{n}, omission: '…')") { s.truncate(n, omission: "…") }
    show("#{s.inspect}.truncate_bytes(#{n})") { s.truncate_bytes(n) }
    show("#{s.inspect}.truncate_words(#{n})") { s.truncate_words(n) }
    show("#{s.inspect}.first(#{n})") { s.first(n) }
    show("#{s.inspect}.last(#{n})") { s.last(n) }
    show("#{s.inspect}.from(#{n})") { s.from(n) }
    show("#{s.inspect}.to(#{n})") { s.to(n) }
    show("#{s.inspect}.at(#{n})") { s.at(n) }
  end
  show("#{s.inspect}.first") { s.first }
  show("#{s.inspect}.last") { s.last }
  show("#{s.inspect}.to(-2)") { s.to(-2) }
  show("#{s.inspect}.first(-1)") { s.first(-1) }
  show("#{s.inspect}.starts_with?('a')") { s.starts_with?("a") }
  show("#{s.inspect}.ends_with?('u', 'b')") { s.ends_with?("u", "b") }
  show("#{s.inspect}.deep_dup.equal?") { s.deep_dup.equal?(s) }
  show("#{s.inspect}.duplicable?") { s.duplicable? }
end
show("'production'.inquiry.production?") { "production".inquiry.production? }
show("'production'.inquiry.test?") { "production".inquiry.test? }
show("'a b c'.truncate_words(2, separator: 'b')") { "a b c".truncate_words(2, separator: "b") }

(0..7).each do |n|
  arr = (1..n).to_a
  [1, 2, 3].each do |k|
    show("#{arr}.in_groups_of(#{k})") { arr.in_groups_of(k) }
    show("#{arr}.in_groups_of(#{k}, false)") { arr.in_groups_of(k, false) }
    show("#{arr}.in_groups_of(#{k}, 0)") { arr.in_groups_of(k, 0) }
    show("#{arr}.in_groups(#{k})") { arr.in_groups(k) }
    show("#{arr}.in_groups(#{k}, false)") { arr.in_groups(k, false) }
  end
  show("#{arr}.split(3)") { arr.split(3) }
  show("#{arr}.split(&:even?)") { arr.split(&:even?) }
  show("#{arr}.extract!(&:odd?)") { a = arr.dup; [a.extract!(&:odd?), a] }
  show("#{arr}.second_to_last") { arr.second_to_last }
  show("#{arr}.third_to_last") { arr.third_to_last }
  show("#{arr}.from(2)") { arr.from(2) }
  show("#{arr}.to(2)") { arr.to(2) }
end
show("[].in_groups_of(0)") { [].in_groups_of(0) }
show("[1].in_groups(0)") { [1].in_groups(0) }
show("[1, 2].in_groups_of(1) { }") { [1, 2].in_groups_of(1) { |g| g } }
show("[1, {a: 1}].extract_options!") { a = [1, { a: 1 }]; [a.extract_options!, a] }
show("[1, 2].extract_options!") { a = [1, 2]; [a.extract_options!, a] }
show("{}.extractable_options?") { {}.extractable_options? }
show("[1, nil, '', [], 2].compact_blank!") { [1, nil, "", [], 2].compact_blank! }
show("{a: nil, b: 1, c: ' '}.compact_blank!") { { a: nil, b: 1, c: " " }.compact_blank! }

h = -> { { a: 1, b: 2, c: 3 } }
show("{a,b,c}.slice!(:a, :z)") { x = h.(); [x.slice!(:a, :z), x] }
show("{a,b,c}.extract!(:a, :z)") { x = h.(); [x.extract!(:a, :z), x] }
show("{a,b,c}.except!(:a, :z)") { x = h.(); [x.except!(:a, :z), x] }
show("{a,b,c}.reverse_merge(a: 9, d: 4)") { h.().reverse_merge(a: 9, d: 4) }
show("{a,b,c}.with_defaults(d: 4)") { h.().with_defaults(d: 4) }
show("{a,b,c}.reverse_merge!(a: 9, d: 4)") { x = h.(); [x.reverse_merge!(a: 9, d: 4), x] }
show("{a,b,c}.with_defaults!(e: 5)") { x = h.(); x.with_defaults!(e: 5); x }
show("{a,b,c}.reverse_update(e: 5)") { x = h.(); x.reverse_update(e: 5); x }
show("default slice!") { x = Hash.new(7).merge(a: 1, b: 2); y = x.slice!(:a); [y, x, x[:zz]] }
nested = -> { { a: [1, { b: 2 }], c: { d: "x" } } }
show("deep_transform_values(&:to_s)") { nested.().deep_transform_values(&:to_s) }
show("deep_transform_values!(&:to_s)") { x = nested.(); x.deep_transform_values!(&:to_s); x }
show("deep_dup independent") { x = nested.(); y = x.deep_dup; y[:a][1][:b] = 9; y[:c][:d] << "y"; [x, y] }
show("deep_dup non-string keys") { x = { [1] => [2] }; y = x.deep_dup; [y, y.keys.first.equal?(x.keys.first)] }
show("Module deep_dup") { String.deep_dup.equal?(String) }
show("method(:puts).duplicable?") { method(:puts).duplicable? }
show("1.duplicable?") { 1.duplicable? }
show("nil.deep_dup") { nil.deep_dup }
show("Holder.new.instance_values") { Holder.new.instance_values }
show("Holder.new.instance_variable_names") { Holder.new.instance_variable_names }

class WithProbe
  attr_accessor :a, :b
  def initialize; @a = 1; @b = 2; end
end
show("Object#with sets and restores") { x = WithProbe.new; seen = x.with(a: 10, b: 20) { |o| [o.equal?(x), o.a, o.b] }; [seen, x.a, x.b] }
show("Object#with restores on raise") { x = WithProbe.new; begin; x.with(a: 5) { raise "boom" }; rescue => e; [e.message, x.a]; end }
show("Object#with no attributes") { x = WithProbe.new; x.with { |o| o.a } }
show("Object#with unknown writer") { x = WithProbe.new; begin; x.with(zz: 1) { }; rescue NoMethodError => e; e.class; end }
show("1.respond_to?(:with)") { [1.respond_to?(:with), nil.respond_to?(:with), :a.respond_to?(:with), "".respond_to?(:with)] }
