# Runtime probe for tests/futamura.rs (guards_runtime): real ActiveRecord on SQLite.
# usage: ruby guard_probe.rb futamura.rb   (FUTAMURA_VERIFY=1 optional)
#
# A statement is built once from the chain as written. It may stand for the chain only while
# every method the chain calls is ActiveRecord's own: a class method an extended module or the
# class redefines, a scope a double stubs, an association reader a double stubs, or a Relation
# method patched in (all of which may answer differently on every call) is Rails' to run.
require "active_record"
require "concurrent"
require "set"
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
ActiveRecord::Schema.define do
  create_table(:rooms) { |t| t.string :name }
  %i[pieces parts bits tags twigs].each { |name| create_table(name) { |t| t.integer :zone; t.string :code; t.integer :room_id } }
end
class Current; class << self; attr_accessor :zone; end; end
class ApplicationRecord < ActiveRecord::Base; self.abstract_class = true; end

# A class-level `unscoped` and `where` an extended module redefines: the added condition reads
# request state on every call.
module ZoneClassMethods
  def unscoped(&block)
    relation = super(&nil)
    relation = relation.where(zone: Current.zone)
    block ? relation.scoping(&block) : relation
  end
end
class Piece < ApplicationRecord
  extend ZoneClassMethods
end
module WhereOverride
  def where(*args, **opts)
    args.empty? && opts.empty? ? super : super.where(zone: Current.zone)
  end
end
class Part < ApplicationRecord
  extend WhereOverride
end

class Plain < ApplicationRecord
  self.table_name = "bits"
end
class Bit < ApplicationRecord
  scope :recent, -> { order(:id) }
  scope :later, -> { recent }
end
class Twig < ApplicationRecord; self.table_name = "twigs"; end
class Room < ApplicationRecord
  has_many :pieces
  has_many :twigs
  has_many :tags
end
class Tag < ApplicationRecord
  scope :recent, -> { order(:id) }
  # A default scope extension that defines a method a scope has the name of.
  default_scope { extending(Tagged).where(zone: 1) }
end
module Tagged
  def recent = order(id: :desc)
end

load ARGV[0]
Futamura.define(:piece_unscoped, "Piece", one: true, unscoped: true, via: :unscoped) { |p| Piece.unscoped.where(code: p.bind).limit(1) }
Futamura.define(:part_where, "Part", one: true, via: :where) { |p| Part.all.where(code: p.bind).limit(1) }
Futamura.define(:bit_later, "Bit", one: true, via: [:later, :recent]) { |p| Bit.all.where(code: p.bind).later.limit(1) }
Futamura.define(:room_twigs, "Twig", one: true, relation: true, via: :where, owner_keys: ["room_id"], owner_columns: ["room_id"]) { |p| Twig.all.where(room_id: p.bind).where(code: p.bind).limit(1) }
Futamura.define(:room_pieces, "Piece", one: true, relation: true, via: :where, owner_keys: ["room_id"], owner_columns: ["room_id"]) { |p| Piece.all.where(room_id: p.bind).where(code: p.bind).limit(1) }
Futamura.define(:plain_where, "Plain", one: true, via: :where) { |p| Plain.all.where(code: p.bind).limit(1) }
Futamura.define(:tag_recent, "Tag", one: true, via: [:recent]) { |p| Tag.all.where(code: p.bind).recent.limit(1) }

$fallbacks = 0
def run(key, binds, **opts, &chain)
  Futamura.records(key, binds, **opts) { $fallbacks += 1; chain.call }&.id
end
def counted
  before = $fallbacks
  result = yield
  [result, $fallbacks - before]
end

out = {}
[Piece, Part, Bit, Tag].each { |m| m.unscoped { m.create!(zone: 1, code: "x"); m.create!(zone: 2, code: "x") } rescue m.insert_all([{ zone: 1, code: "x" }, { zone: 2, code: "x" }]) }
room = Room.create!(name: "r")
Piece.insert_all([{ zone: 1, code: "y", room_id: room.id }, { zone: 2, code: "y", room_id: room.id }])
Twig.create!(code: "y", room_id: room.id)
Twig.create!(code: "x")

# 1. `unscoped` is the extended module's: the zone is read on every call.
out["unscoped"] = [1, 2, 1].map { |zone| Current.zone = zone; counted { run(:piece_unscoped, ["x"]) { Piece.unscoped.where(code: "x").first } } }
# 2. `where` too, whichever relation it is first called on.
out["where"] = [1, 2].map { |zone| Current.zone = zone; counted { run(:part_where, ["x"]) { Part.where(code: "x").first } } }
Current.zone = nil

# 3. Scopes: Rails' own, then redefined by the class, then stubbed by a double (an anonymous
# module prepended to the singleton class, as Mocha does), then restored.
bit = -> { run(:bit_later, ["x"]) { Bit.all.where(code: "x").later.first } }
out["scope"] = [counted(&bit)]
Bit.singleton_class.prepend(stub = Module.new { def recent = order(id: :desc) })
out["scope"] << counted(&bit)
stubbed_later = Module.new { def later = order(id: :desc) }
Bit.singleton_class.prepend(stubbed_later)
out["scope"] << counted(&bit)
# A scope redefined in the class body after `scope :recent` (the later definition wins).
class Bit
  def self.recent = order(id: :desc)
end
out["scope_redefined"] = [counted(&bit)]

# 4. The association reader: Rails' own, redefined on the owner by a double, redefined by the
# owner's class.
reader = ->(owner) { run(:room_twigs, [owner.id, "y"], owner: owner, association: :twigs) { owner.twigs.where(code: "y").first } }
out["reader"] = [counted { reader.call(room) }]
doubled = Room.find(room.id)
doubled.singleton_class.prepend(Module.new { def twigs = Twig.where(code: "x") })
out["reader"] << counted { reader.call(doubled) }
out["reader"] << counted { reader.call(Room.find(room.id)) }
class Room
  def twigs = Twig.where(code: "x")
end
out["reader_overridden"] = [counted { reader.call(Room.find(room.id)) }]
# Rails starts an association's scope from `klass.unscoped`: a class that redefines it (Piece) has
# its own condition in every read of the association, which the statement does not have.
Current.zone = 1
out["reader_unscoped_redefined"] = [counted { run(:room_pieces, [room.id, "y"], owner: room, association: :pieces) { room.pieces.where(code: "y").first } }]
Current.zone = nil

# 5. A default scope's extension defines a method a scope has the name of: the chain calls it.
Current.zone = 1
out["extension_scope"] = [counted { run(:tag_recent, ["x"]) { Tag.all.where(code: "x").recent.first } }]
Current.zone = nil

# 5b. Rails' own `where`, reached through another path to the same files (a gem home whose
# entries are symlinks): its source_location names the other path, but it is still Rails'. The
# same method defined from a file outside Rails is the app's.
require "tmpdir"
require "fileutils"
alias_home = Dir.mktmpdir("fut-guard")
real_rails = File.realpath(Futamura.send(:rails_dir))
File.symlink(real_rails, "#{alias_home}/active_record")
%w[Linked Elsewhere].each do |name|
  Object.const_set(name, Class.new(ApplicationRecord) { self.table_name = "tags" })
  Futamura.define(:"#{name.downcase}_where", name, one: true, via: :where) { |p| Object.const_get(name).all.where(code: p.bind).limit(1) }
end
Linked.singleton_class.class_eval("def where(*args) = super", "#{alias_home}/active_record/querying.rb", 1)
Elsewhere.singleton_class.class_eval("def where(*args) = super", "#{alias_home}/not_rails.rb", 1)
out["aliased_rails_path"] = [counted { run(:linked_where, ["x"]) { Linked.where(code: "x").first } }]
out["outside_rails"] = [counted { run(:elsewhere_where, ["x"]) { Elsewhere.where(code: "x").first } }]
FileUtils.rm_rf(alias_home)

# 6. A Relation builder patched in (a module prepended to ActiveRecord::Relation) is not built once.
ActiveRecord::Relation.prepend(Module.new { def limit(*) = super })
out["patched"] = [counted { run(:plain_where, ["x"]) { Plain.all.where(code: "x").first } }]

out.each { |k, v| puts "#{k}=#{v.inspect}" }
