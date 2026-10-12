# Runtime probe for tests/futamura.rs (default_condition_runtime): real ActiveRecord on SQLite.
# usage: ruby default_condition_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
#
# A default scope's conditions are bound at run time (an equality over a bind). The other shapes
# a static default scope makes are bound the same way: `where.not(c: v)` (`!=`), `where(c: [..])`
# and `where.not(c: [..])` (`IN`/`NOT IN` over a short list, one statement for each list length).
# A longer list, or one with nil, is Rails'.
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  %i[hiddens opens closeds longs mixeds].each { |name| create_table(name) { |t| t.string :status; t.string :code } }
end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
class Hidden < ApplicationRecord; default_scope { where.not(status: "gone") }; end
class Open < ApplicationRecord; default_scope { where(status: %w[a b]) }; end
class Closed < ApplicationRecord; default_scope { where.not(status: %w[a b]) }; end
class Long < ApplicationRecord; default_scope { where(status: %w[p q r s t u v w c]) }; end
# Two conditions in one default scope, one of each kind.
class Mixed < ApplicationRecord; default_scope { where.not(status: "gone").where(code: "x") }; end

load ARGV[0]
[Hidden, Open, Closed, Long, Mixed].each do |klass|
  Futamura.define(klass.name.downcase.to_sym, klass, one: true) { |p| klass.all.where(code: p.bind).send(:ordered_relation).limit(1) }
end

$fallbacks = 0
def look(klass, code)
  before = $fallbacks
  record = Futamura.records(klass.name.downcase.to_sym, [code]) { $fallbacks += 1; klass.where(code: code).first }
  [record&.id, $fallbacks - before]
end
[Hidden, Open, Closed, Long, Mixed].each do |klass|
  klass.unscoped do
    [nil, "gone", "a", "c", "c"].each { |status| klass.create!(status: status, code: "x") }
  end
end

out = {}
%i[Hidden Open Closed Long Mixed].each { |name| klass = Object.const_get(name); out[name.to_s.downcase] = [look(klass, "x"), look(klass, "x"), look(klass, "nothing")] }
out.each { |k, v| puts "#{k}=#{v.inspect}" }
