# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveRecord::Result against
# runtime/ruby/active_record/connection.rb's port.
#
#   ruby tests/active_record_result_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activerecord;
# `emitted` loads the runtime's connection.rb. Each builds Results the
# way Rails' adapters do, `new(columns, rows, column_types, affected_rows:)`,
# under a prepended `initialize` shaped like Core's
# ActiveRecordShopScopedResult (it reads `columns.index("shop_id")` and
# drops other shops' rows before `super`), and prints one line per
# observation; `compare` requires them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? {} : RailsOracle.env
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "Result differs (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActiveRecord::Result agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport", "activemodel", "activerecord")
  require "active_record"
else
  require_relative "../runtime/spinel/active_model_type"
  require_relative "../runtime/ruby/active_record/connection"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.chomp.inspect}"
end

# Core's lib/patches/active_record_shop_scoped_result.rb, its signature
# and its reads of `columns` and `rows`.
module ShopScoped
  CURRENT = [nil]

  def initialize(columns, rows, column_types = {}, affected_rows: nil)
    if CURRENT[0] && (shop_id_column = columns.index("shop_id"))
      rows.reject! { |row| !row[shop_id_column].nil? && row[shop_id_column] != CURRENT[0] }
    end
    super
  end
end
ActiveRecord::Result.prepend(ShopScoped)

show "ancestors", ActiveRecord::Result.ancestors.take_while { |m| m != Object }.map(&:name) - ["ActiveSupport::ToJsonWithActiveSupportEncoder", "ActiveSupport::Tryable"]
columns = ["id", "shop_id", "title"]
rows = [[1, 5, "a"], [2, 6, "b"], [3, nil, "c"]]
all = ActiveRecord::Result.new(columns, rows.map(&:dup))
show "columns", all.columns
show "columns frozen", all.columns.frozen?
show "rows", all.rows
show "to_a", all.to_a
show "to_ary", all.to_ary
show "first", all.first
show "first 2", all.first(2)
show "last", all.last
show "last 2", all.last(2)
show "[1]", all[1]
show "[9]", all[9]
show "length", all.length
show "count", all.count
show "empty?", all.empty?
show "includes_column? title", all.includes_column?("title")
show "includes_column? nope", all.includes_column?("nope")
show "map", all.map { |row| row["title"] }
show "each returns", outcome { all.each { |row| row }.class }
show "each enumerator", all.each.to_a
show "each_with_index", all.each_with_index.map { |row, i| [i, row["id"]] }
show "select", all.select { |row| row["shop_id"] }.map { |row| row["id"] }
show "affected_rows", all.affected_rows
show "column_types", all.column_types.transform_values { |type| type.class.name }
show "typed column_types", ActiveRecord::Result.new(["id", "n"], [[1, 2]], { 1 => :custom }).column_types.transform_values { |type| type.is_a?(Symbol) ? type : type.class.name }
show "result", all.result.equal?(all)
ShopScoped::CURRENT[0] = 5
scoped = ActiveRecord::Result.new(columns, rows.map(&:dup), {}, affected_rows: 3)
show "scoped rows", scoped.rows
show "scoped to_a", scoped.to_a
show "scoped affected_rows", scoped.affected_rows
unscoped = ActiveRecord::Result.new(["id"], [[1], [2]])
show "no shop_id column", unscoped.rows
ShopScoped::CURRENT[0] = nil
empty = ActiveRecord::Result.new([], [])
show "empty", [empty.columns, empty.rows, empty.to_a, empty.first, empty.empty?, empty.length]
show "symbol columns", outcome { ActiveRecord::Result.new([:a], [[1]]).to_a }
show "one argument", outcome { ActiveRecord::Result.new([{ "id" => 1 }]) }
