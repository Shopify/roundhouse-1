# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveModel::Attribute against the port in
# runtime/spinel/active_model_attribute.rb.
#
#   ruby tests/active_model_attribute_runtime.rb compare
#
# Modes:
# - `native` loads activemodel from the pinned Rails; `emitted` loads the
#   runtime as the emitted PostgreSQL tree does (runtime/ruby/rails.rb,
#   the CRuby overlay's core extensions, the attribute-method port that
#   declares MissingAttributeError, the type port, then this port). Each
#   prints one line per observation: what each subclass answers for
#   value, original value, database value, change tracking, the copies
#   (`with_value_from_user`, `forgetting_assignment`, `with_type`,
#   `dup_or_share`, `dup`), equality, the coder round trip, the method
#   tables, the private constants by name, and the error each failure
#   raises (class and message).
# - `compare` runs both and requires every observation equal.
#
# Types are printed by class name: the attribute holds the type object
# the caller passed, and the type port is checked against activemodel by
# tests/active_model_type_runtime.rb.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  run = lambda do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  outputs = [run.call("native"), run.call("emitted")]
  native, emitted = outputs.map(&:lines)
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(12)
    abort "Attributes differ (#{native.length} native lines, #{emitted.length} emitted):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{native.length}"
  puts "Native ActiveModel::Attribute and the port agree (#{RailsOracle.label})"
  exit
end

root = File.expand_path("..", __dir__)
case ARGV.first
when "native"
  RailsOracle.activate("activesupport", "activemodel", "activerecord")
  require "active_model"
  require "active_record"
  # Touch the autoloads the tree loads eagerly.
  ActiveModel::Attribute
  ActiveModel::Type::Integer
  ActiveModel::AttributeMethods # declares MissingAttributeError
when "emitted"
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(root, "runtime/spinel/active_model_attribute_methods")
  require File.join(root, "runtime/spinel/active_model_type")
  require File.join(root, "runtime/spinel/active_model_attribute")
else
  abort "usage: #{$PROGRAM_NAME} compare | native | emitted"
end
require "yaml"

A = ActiveModel::Attribute
T = ActiveModel::Type

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def attempt(label)
  value = yield
  show(label, value)
rescue StandardError, NotImplementedError => e
  puts "#{label}: raised #{e.class}: #{e.message}"
end

# The state an attribute answers, the type by class name.
def state(attribute)
  [
    attribute.class.name,
    attribute.name,
    attribute.value_before_type_cast,
    attribute.type.class.name,
    attribute.initialized?,
    attribute.came_from_user?,
    attribute.has_been_read?,
  ]
end

# Constants by name, as `constantize` reaches them, and directly.
%w[FromDatabase FromUser WithCastValue Null Uninitialized].each do |name|
  attempt("const_get ActiveModel::Attribute::#{name}") { Object.const_get("ActiveModel::Attribute::#{name}").name }
  attempt("superclass #{name}") { Object.const_get("ActiveModel::Attribute::#{name}").superclass.name }
end
attempt("direct ActiveModel::Attribute::FromDatabase") { ActiveModel::Attribute::FromDatabase }
show "public constants", A.constants.sort
show "Uninitialized constants", Object.const_get("ActiveModel::Attribute::Uninitialized").constants.sort

# The method tables, so a method the source defines is not missing.
[A, *%w[FromDatabase FromUser WithCastValue Null Uninitialized].map { |n| A.const_get(n) }].each do |klass|
  show "#{klass.name} public methods", klass.public_instance_methods(false).sort
  show "#{klass.name} private methods", klass.private_instance_methods(false).sort
end
show "singleton methods", A.singleton_methods(false).sort

integer = T::Integer.new
string = T::String.new
boolean = T::Boolean.new
immutable = T::ImmutableString.new

# FromDatabase over Integer.
db = A.from_database("x", "1", integer)
show "from_database state", state(db)
show "from_database value", db.value
show "from_database read", state(db)
show "from_database value_for_database", db.value_for_database
show "from_database original_value", db.original_value
show "from_database original_value_for_database", db.original_value_for_database
show "from_database changed?", [db.changed?, db.changed_in_place?]
show "from_database serializable?", db.serializable?
show "from_database with a cast value", A.from_database("x", "1", integer, 7).value
show "from_database cast value read?", A.from_database("x", "1", integer, 7).has_been_read?
show "from_database nil cast value read?", A.from_database("x", "1", integer, nil).has_been_read?

# Assignment over it.
user = db.with_value_from_user("2")
show "with_value_from_user state", state(user)
show "with_value_from_user value", user.value
show "with_value_from_user original_value", user.original_value
show "with_value_from_user original_value_for_database", user.original_value_for_database
show "with_value_from_user value_for_database", user.value_for_database
show "with_value_from_user changed?", [user.changed?, user.changed_in_place?]
same = db.with_value_from_user("1")
show "assigning the same value changed?", [same.changed?, same.value]
again = user.with_value_from_user("3")
show "twice assigned original_value", [again.value, again.original_value, again.changed?]
forgot = user.forgetting_assignment
show "forgetting_assignment state", state(forgot)
show "forgetting_assignment value", [forgot.value, forgot.changed?]
fresh = A.from_database("x", "05", integer)
show "unread forgetting_assignment keeps the raw value", state(fresh.forgetting_assignment)
fresh.value_for_database
show "persisted forgetting_assignment", state(fresh.forgetting_assignment)

# FromUser directly.
from_user = A.from_user("b", "0", boolean)
show "from_user boolean", [from_user.value, from_user.has_been_read?, from_user.came_from_user?]
show "from_user boolean value_for_database", from_user.value_for_database
show "from_user without original changed?", from_user.changed?
show "from_user original_value", from_user.original_value
attempt("from_user out of range value_for_database") { A.from_user("i", 2**70, integer).value_for_database }
attempt("from_database out of range value_for_database") { A.from_database("i", 2**70, integer).value_for_database }

# In-place change over a mutable type.
text = A.from_database("s", "abc", string)
show "string value", text.value
text.value << "d"
show "string changed in place", [text.changed?, text.changed_in_place?, text.value_for_database]
show "string original_value_for_database", text.original_value_for_database
show "string forgetting_assignment", [state(text.forgetting_assignment), text.forgetting_assignment.value]
retyped = text.with_type(immutable)
show "with_type after in-place change", [state(retyped), retyped.value, retyped.changed?]
plain = A.from_database("s", "abc", string)
show "with_type unchanged", state(plain.with_type(immutable))
show "with_type over integer", [state(db.with_type(string)), db.with_type(string).value]
show "with_value_from_database", state(user.with_value_from_database("9"))
show "with_cast_value on an instance", [state(user.with_cast_value(4)), user.with_cast_value(4).value]

# Copies.
shared = A.from_database("x", "1", integer)
show "dup_or_share immutable type", shared.dup_or_share.equal?(shared)
copied = A.from_database("s", "abc", string)
copied.value
dup = copied.dup_or_share
show "dup_or_share mutable type", [dup.equal?(copied), dup == copied, dup.value == copied.value, dup.value.equal?(copied.value)]
unread = A.from_database("s", "abc", string).dup
show "dup unread", [unread.has_been_read?, unread.value]

# WithCastValue, Null, Uninitialized.
cast = A.with_cast_value("c", [1], nil)
show "with_cast_value", [state(cast), cast.value, cast.changed_in_place?, cast.changed?]
null = A.null("n")
show "null", [state(null), null.value]
show "null value_for_database", null.value_for_database
attempt("null with_value_from_user") { null.with_value_from_user(1) }
attempt("null with_value_from_database") { null.with_value_from_database(1) }
attempt("null with_cast_value") { null.with_cast_value(1) }
show "null with_type", [state(null.with_type(integer)), null.with_type(integer).value]
missing = A.uninitialized("u", integer)
show "uninitialized", [state(missing), missing.value, missing.value { |name| "block saw #{name}" }]
show "uninitialized original_value", [missing.original_value.class.name, missing.original_value.frozen?, missing.original_value.equal?(A.uninitialized("v", nil).original_value)]
show "uninitialized value_for_database", missing.value_for_database
show "uninitialized changed?", [missing.changed?, missing.changed_in_place?]
show "uninitialized forgetting_assignment", [state(missing.forgetting_assignment), missing.forgetting_assignment.equal?(missing)]
show "uninitialized with_type", state(missing.with_type(string))
show "uninitialized with_value_from_user", [state(missing.with_value_from_user("4")), missing.with_value_from_user("4").value, missing.with_value_from_user("4").changed?]
attempt("base type_cast") { A.new("b", 1, integer).value }

# Equality.
show "== same", A.from_database("x", "1", integer) == A.from_database("x", "1", T::Integer.new)
show "eql? same", A.from_database("x", "1", integer).eql?(A.from_database("x", "1", integer))
show "hash same", A.from_database("x", "1", integer).hash == A.from_database("x", "1", integer).hash
show "== other class", A.from_database("x", "1", integer) == A.from_user("x", "1", integer)
show "== other raw value", A.from_database("x", "1", integer) == A.from_database("x", "2", integer)
show "== other type", A.from_database("x", "1", integer) == A.from_database("x", "1", string)
show "== other name", A.from_database("x", "1", integer) == A.from_database("y", "1", integer)
show "== non-attribute", A.from_database("x", "1", integer) == "1"

# The coder: what encode_with writes, and what init_with reads back.
def encoded(attribute)
  coder = Psych::Coder.new("tag")
  attribute.encode_with(coder)
  coder.map.transform_values { |v| v.is_a?(A) ? state(v) : (v.is_a?(T::Value) ? v.class.name : v) }
end
show "encode_with unread", encoded(A.from_database("x", "1", integer))
show "encode_with read", encoded(A.from_database("x", "1", integer).tap(&:value))
show "encode_with assigned", encoded(db.with_value_from_user("2"))
show "encode_with nil raw value and type", encoded(A.with_cast_value("c", nil, nil))
coder = Psych::Coder.new("tag")
coder.map = { "name" => "r", "value_before_type_cast" => "5", "type" => integer }
revived = A.from_database("ignored", nil, nil)
revived.init_with(coder)
show "init_with", [state(revived), revived.value]
coder.map = { "name" => "r", "value_before_type_cast" => "5", "type" => integer, "value" => 6 }
revived.init_with(coder)
show "init_with value", revived.value
yaml = YAML.dump(A.with_cast_value("c", 3, nil).tap(&:value))
show "YAML.dump", yaml
loaded = YAML.load(yaml, permitted_classes: ["ActiveModel::Attribute::WithCastValue"])
show "YAML.load", [state(loaded), loaded.value]
attempt("YAML.load not permitted") { YAML.load(yaml) }

# ActiveRecord::Relation::QueryAttribute: the bind attribute a VALUES row
# or a predicate wraps a value in (the port leaves out the statement cache's
# Substitute, which nothing here builds).
Q = ActiveRecord::Relation::QueryAttribute
show "QueryAttribute superclass", Q.superclass.name
show "QueryAttribute public methods", Q.public_instance_methods(false).sort
show "QueryAttribute private methods", Q.private_instance_methods(false).sort
query = Q.new("x", "1", integer)
show "QueryAttribute state", state(query)
show "QueryAttribute value", query.value
show "QueryAttribute value_for_database", query.value_for_database
show "QueryAttribute nil?", [query.nil?, Q.new("x", nil, integer).nil?]
show "QueryAttribute infinite?", [query.infinite?, Q.new("x", Float::INFINITY, T::Float.new).infinite?]
show "QueryAttribute unboundable?", [query.unboundable?, Q.new("x", 2**70, integer).unboundable?, Q.new("x", -(2**70), integer).unboundable?]
show "QueryAttribute with_cast_value", state(query.with_cast_value(5))
show "QueryAttribute with_cast_value value_for_database", query.with_cast_value(5).value_for_database
show "QueryAttribute changed?", query.changed?
# A mutable type's raw value is copied (Object#deep_dup) unless frozen, so a
# later edit of the caller's string does not reach the bind.
raw = +"abc"
mutable_query = Q.new("s", raw, T::String.new)
raw << "d"
show "QueryAttribute mutable raw value", [mutable_query.value_before_type_cast, mutable_query.value_for_database, T::String.new.mutable?]
