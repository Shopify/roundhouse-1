# Runtime probe for tests/futamura.rs (preload_runtime): real ActiveRecord on SQLite.
# usage: ruby futamura_preload_runtime.rb futamura.rb   (FUTAMURA_VERIFY=0|1, FUTAMURA_STATS=<dir>)
#
# The runtime preloader plans a preload entry once. A target with a default scope is planned too,
# but its scope is read from the class at every call (Preloader::Association#build_scope starts
# from `klass.scope_for_association`), so a default scope that reads state that changes between
# calls loads what Rails loads. Under FUTAMURA_VERIFY=1 the specialized call is compared to Rails'
# own Preloader, so the probe passing is also the check.
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:authors) { |t| t.string :name }
  create_table(:books) { |t| t.string :title; t.integer :author_id; t.integer :tenant_id; t.boolean :hidden, default: false }
  create_table(:tags) { |t| t.string :label; t.integer :book_id }
  create_table(:reviews) { |t| t.string :body; t.integer :book_id; t.integer :reviewer_id }
end
module Current; class << self; attr_accessor :tenant; end; end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end
class Author < ApplicationRecord
  has_many :books
  has_many :tags, through: :books
  has_many :reviews, through: :books
end
class Book < ApplicationRecord
  # Reads state that differs between calls, returns nil without it, and is not plain equality.
  default_scope { Current.tenant ? where(tenant_id: Current.tenant).where.not(hidden: true).order(:id) : nil }
  belongs_to :author
  has_many :tags
  has_many :reviews, -> { order(:id) }
end
class Tag < ApplicationRecord; belongs_to :book; end
class Review < ApplicationRecord; belongs_to :book; end

load ARGV[0]

Futamura.define(:authors_books, Author, via: :where) { |p| Author.all.preload(:books).where(name: p.bind) }
Futamura.define(:authors_nested, Author, via: :where) { |p| Author.all.preload(books: :tags).where(name: p.bind) }
Futamura.define(:authors_tags, Author, via: :where) { |p| Author.all.preload(:tags).where(name: p.bind) }
Futamura.define(:tags_book, Tag, via: :where) { |p| Tag.all.preload(book: :author).where(label: p.bind) }

ann = Author.create!(name: "ann")
Author.create!(name: "bob")
[[1, false], [1, true], [2, false], [nil, false]].each_with_index do |(tenant, hidden), i|
  book = Book.unscoped.create!(title: "b#{i}", author: ann, tenant_id: tenant, hidden: hidden)
  Tag.create!(label: "x", book: book)
  Review.create!(body: "r#{i}", book: book)
end

$fallbacks = 0
$queries = []
ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
  $queries << payload[:sql] if payload[:name] == "Book Load"
end

def titles(key, model, arg)
  $queries.clear
  records = Futamura.records(key, [arg]) { $fallbacks += 1; yield }
  [records, $queries.dup]
end

# One preload entry, the same call at tenants 1, 2, none and 1 again: each loads what Rails does.
[1, 2, nil, 1].each do |tenant|
  Current.tenant = tenant
  authors, queries = titles(:authors_books, Author, "ann") { Author.where(name: "ann").preload(:books).to_a }
  loaded = authors.flat_map { |a| a.association(:books).loaded? ? a.books.map(&:title) : [:not_loaded] }
  rails = Author.where(name: "ann").to_a.flat_map { |a| a.books.map(&:title) }
  raise "tenant #{tenant.inspect}: #{loaded.inspect} != #{rails.inspect}" unless loaded == rails
  puts "books[tenant=#{tenant.inspect}]=#{loaded.inspect} queries=#{queries.size} tenant_in_sql=#{queries.any? { |q| q.include?("tenant_id") }}"
end

# A nested entry whose first level has the default scope.
Current.tenant = 1
authors, = titles(:authors_nested, Author, "ann") { Author.where(name: "ann").preload(books: :tags).to_a }
puts "nested=#{authors.flat_map { |a| a.books.map { |b| [b.title, b.tags.map(&:label)] } }.inspect}"

# A default-scoped target at the second level, reached through belongs_to.
tags, = titles(:tags_book, Tag, "x") { Tag.where(label: "x").preload(book: :author).to_a }
puts "tags=#{tags.map { |t| [t.id, t.association(:book).loaded? ? t.book&.title : :not_loaded] }.inspect}"

plans = Futamura::Preload.instance_variable_get(:@plans)
shape = plans.each_pair.map { |(spec, root), plan| [spec, root.name, String === plan ? plan : plan.map { |n| [n.name, n.dynamic, n.children.map(&:name)] }] }.sort_by(&:inspect)
puts "plans=#{shape.inspect}"

# A `through:` entry is Rails'; the refusal is counted by reason when FUTAMURA_STATS is on.
Current.tenant = 1
titles(:authors_tags, Author, "ann") { Author.where(name: "ann").preload(:tags).to_a }
puts "refusals=#{Futamura::Stats.preload_refusals.sort.to_h.inspect}" if defined?(Futamura::Stats)
