# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActionController::Parameters against the port in
# runtime/ruby/action_controller/parameters.rb.
#
#   ruby tests/action_controller_parameters_runtime.rb compare
#
# Modes:
# - `native` loads actionpack's strong_parameters from the pinned Rails;
#   `emitted` loads the runtime as the emitted PostgreSQL tree does
#   (runtime/ruby/rails.rb, the CRuby overlay's core extensions, then
#   the port). Each prints one line per observation: reads, permitted
#   status, the conversions, every `permit` / `expect` filter shape,
#   the hash-like API, equality, copies, YAML, and the error each
#   failure raises (class and message).
# - `compare` runs both and requires every observation equal.
# - `bare` loads the port without the overlay (no
#   HashWithIndifferentAccess, as on Spinel) and checks the core
#   contract by assertion.
#
# What the railtie configures is set explicitly here, since the native
# side runs without a Rails app: `action_on_unpermitted_parameters`
# defaults to nil in actionpack and to the railtie's `:log` in a local
# environment in the port.
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
    abort "Parameters differ (#{native.length} native lines, #{emitted.length} emitted):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{native.length}"
  puts "Native ActionController::Parameters and the port agree (#{RailsOracle.label})"
  exit
end

root = File.expand_path("..", __dir__)
case ARGV.first
when "native"
  RailsOracle.activate("activesupport", "actionpack", "activemodel")
  require "action_controller"
  require "action_controller/metal/strong_parameters"
when "emitted"
  require File.join(root, "runtime/ruby/rails")
  require File.join(root, "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(root, "runtime/ruby/action_controller/parameters")
when "bare"
  require File.join(root, "runtime/ruby/action_controller/parameters")
  P = ActionController::Parameters
  def check(label, ok)
    abort "bare: #{label}" unless ok
  end
  params = P.new(a: 1, b: { c: 2 })
  check "read by name", params["a"] == 1 && params[:a] == 1
  check "nested read is Parameters", params[:b].is_a?(P) && params[:b][:c] == 2
  check "not permitted", !params.permitted?
  begin
    params.to_h
    check "to_h raises", false
  rescue ActionController::UnfilteredParameters => e
    check "to_h message", e.message == "unable to convert unpermitted parameters to hash"
  end
  check "to_unsafe_h", params.to_unsafe_h == { "a" => 1, "b" => { "c" => 2 } }
  check "permit!", params.permit!.permitted? && params[:b].permitted?
  check "to_h", params.to_h == { "a" => 1, "b" => { "c" => 2 } } && params.to_h.instance_of?(Hash)
  check "permit", P.new(a: 1, z: 2).permit(:a).to_h == { "a" => 1 }
  check "constantize", Object.const_get("ActionController::Parameters") == P
  puts "bare: ok"
  exit
else
  abort "usage: #{$PROGRAM_NAME} compare | native | emitted | bare"
end
require "yaml"
require "date"
require "stringio"

P = ActionController::Parameters
P.action_on_unpermitted_parameters = false

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def attempt(label)
  value = yield
  show(label, value)
rescue StandardError, NotImplementedError => e
  puts "#{label}: raised #{e.class}: #{e.message}"
end

# Reads.
show "new(a: 1)[\"a\"]", P.new(a: 1)["a"]
show "new(a: 1)[:a]", P.new(a: 1)[:a]
show "new(\"a\" => 1)[:a]", P.new("a" => 1)[:a]
show "missing", P.new(a: 1)[:z]
x = P.new(a: 1, b: { c: 2, d: { e: 3 } }, f: [{ g: 4 }, 5], h: [1, 2])
show "inspect", x
show "nested", x[:b]
show "nested deeper", x[:b][:d]
show "inspect after nested reads", x
show "array of hashes", x[:f]
show "array element class", x[:f].map(&:class)
show "array of scalars", x[:h]
show "same array object on reread", x[:f].equal?(x[:f])
show "same nested object on reread", x[:b].equal?(x[:b])
show "nested permitted?", x[:b].permitted?
show "keys", x.keys
show "values", x.values
show "key? :a", [x.key?(:a), x.has_key?("a"), x.include?(:z), x.member?(:b), x.exclude?(:z)]
show "empty?", [x.empty?, P.new.empty?]
show "to_s", P.new(a: 1, b: { c: 2 }).to_s
show "class name", P.name
show "const_get", Object.const_get("ActionController::Parameters").equal?(P)

# Construction errors.
attempt("new(1 => 2)") { P.new(1 => 2) }
attempt("new(Parameters)") { P.new(P.new(a: 1)) }
attempt("new(nil)") { P.new(nil) }

# Permitted status and conversions.
y = P.new(a: 1, b: { c: 2 })
show "permitted?", y.permitted?
attempt("to_h unpermitted") { y.to_h }
attempt("to_hash unpermitted") { y.to_hash }
show "UnfilteredParameters ancestors", ActionController::UnfilteredParameters.ancestors.take(3)
show "to_unsafe_h", y.to_unsafe_h
show "to_unsafe_h class", y.to_unsafe_h.class.name
show "to_unsafe_h nested class", y.to_unsafe_h["b"].class.name
show "to_unsafe_h[:b]", y.to_unsafe_h[:b]
show "to_unsafe_hash", y.to_unsafe_hash
show "permit! returns self", y.permit!.equal?(y)
show "permitted? after permit!", y.permitted?
show "nested permitted? after permit!", y[:b].permitted?
show "to_h", y.to_h
show "to_h class", y.to_h.class.name
show "to_h with block", y.to_h { |k, v| [k.upcase, v] }
show "to_hash", y.to_hash
show "to_hash class", y.to_hash.class.name
show "to_hash nested class", y.to_hash["b"].class.name
z = P.new(list: [{ a: 1 }, [{ b: 2 }]])
z.permit!
show "permit! reaches arrays", [z[:list][0].permitted?, z[:list][1][0].permitted?]
mixed = P.new(a: 1)
mixed.permit!
mixed[:inner] = P.new(b: 2)
attempt("to_h with unpermitted child") { mixed.to_h }
show "to_unsafe_h with unpermitted child", mixed.to_unsafe_h

# permit filters.
person = P.new(
  name: "Ann", age: 30, admin: true, bio: nil, tags: ["a", "b"], ids: [1, 2],
  address: { city: "Ottawa", zip: "K1A", extra: "x" },
  pets: [{ name: "Rex", kind: "dog" }, { name: "Tom", kind: "cat" }],
  prefs: { theme: "dark", nested: { deep: [1, { no: 1 }] } },
  "birthday(1i)" => "2000", "birthday(2i)" => "1", "birthday(3i)" => "2",
  numbered: { "0" => { label: "a", x: 1 }, "1" => { label: "b" } },
  hash_for_scalar: { a: 1 }, mixed_array: [1, { a: 1 }],
  when_date: Date.new(2020, 1, 2), when_time: Time.at(0).utc, io: StringIO.new("x"),
  controller: "c", action: "a",
)
show "permit scalars", person.permit(:name, :age, :admin, :bio).to_h
show "permit string filter", person.permit("name").to_h
show "permit non-scalar for scalar filter", person.permit(:hash_for_scalar, :tags).to_h
show "permit scalar arrays", person.permit(tags: [], ids: []).to_h
show "permit mixed array as scalars", person.permit(mixed_array: []).to_h
show "permit nested", person.permit(address: [:city, :zip]).to_h
show "permit nested hash filter", person.permit(address: { city: [] }).to_h
show "permit array of hashes", person.permit(pets: [:name]).to_h
show "permit array of hashes explicit", person.permit(pets: [[:name]]).to_h
show "permit any hash", person.permit(prefs: {}).to_h
show "permit multiparameter", person.permit(:birthday).to_h
show "permit nested attributes", person.permit(numbered: [:label]).to_h
show "permit numeric keys", person.permit(numbered: { "0" => [:label] }).to_h
show "permit dates and io", person.permit(:when_date, :when_time).to_h
show "permit io class", person.permit(:io).to_h["io"].class.name
show "permit result permitted?", person.permit(:name).permitted?
show "permit leaves source", person.permitted?
show "permit missing key", person.permit(:nope).to_h
show "permit nested missing", person.permit(nope: [:a]).to_h
show "permit flattened arrays", person.permit([:name, [:age]]).to_h
P.action_on_unpermitted_parameters = :raise
attempt("raise on unpermitted") { person.permit(:name, :age, :admin, :bio) }
attempt("raise on one unpermitted") { P.new(a: 1, b: 2, controller: "c").permit(:a) }
attempt("raise nested") { P.new(a: { b: 1, c: 2 }).permit(a: [:b]) }
# typed_parameters' `permit_types`: `unpermitted_parameters!(params)`, the
# action read from the class.
attempt("unpermitted_parameters! with the class action") { P.new(a: 1, b: 2).send(:unpermitted_parameters!, P.new(a: 1)) }
show "always permitted", P.new(a: 1, controller: "c", action: "x").permit(:a).to_h
show "UnpermittedParameters#params", (begin; P.new(a: 1, b: 2).permit; rescue ActionController::UnpermittedParameters => e; e.params; end)
show "UnpermittedParameters ancestors", ActionController::UnpermittedParameters.ancestors.include?(StandardError)
P.action_on_unpermitted_parameters = false
show "always_permitted_parameters", P.always_permitted_parameters

# require / expect.
form = P.new(person: { name: "Ann", age: 1 }, blank: "", spaces: "  ", none: nil, off: false, zero: 0, list: [], items: [{ n: 1 }, { n: 2 }])
show "require", form.require(:person)
show "required", form.required("person")
show "require array", form.require([:person, :zero])
show "require false", form.require(:off)
show "require zero", form.require(:zero)
attempt("require blank") { form.require(:blank) }
attempt("require spaces") { form.require(:spaces) }
attempt("require nil") { form.require(:none) }
attempt("require empty list") { form.require(:list) }
attempt("require missing") { form.require(:missing) }
show "ParameterMissing param", (begin; form.require(:missing); rescue ActionController::ParameterMissing => e; e.param; end)
show "expect", form.expect(person: [:name])
show "expect permitted?", form.expect(person: [:name]).permitted?
show "expect two", form.expect(:zero, person: [:name])
show "expect array of hashes", form.expect(items: [[:n]])
attempt("expect array where hash") { form.expect(items: [:n]) }
attempt("expect missing") { form.expect(missing: [:a]) }
attempt("expect!") { form.expect!(missing: [:a]) }
show "ExpectedParameterMissing ancestors", ActionController::ExpectedParameterMissing.ancestors.take(2)

# Hash-like API.
h = P.new(a: 1, b: { c: 2 }, d: nil, e: "", f: [], g: " ", i: "x")
show "fetch", h.fetch(:a)
show "fetch nested", h.fetch(:b)
show "fetch default", h.fetch(:z, 9)
show "fetch default hash", h.fetch(:z, { y: 1 })
show "fetch block", h.fetch(:z) { |k| "got #{k.inspect}" }
attempt("fetch missing") { h.fetch(:z) }
show "fetch_values", h.fetch_values(:a, "i")
show "fetch_values block", h.fetch_values(:a, :z) { |k| k.to_s * 2 }
attempt("fetch_values missing") { h.fetch_values(:a, :z) }
show "values_at", h.values_at(:a, :b, :z)
show "dig", h.dig(:b, :c)
show "dig missing", h.dig(:z, :c)
attempt("dig scalar") { h.dig(:a, :c) }
show "dig one", h.dig(:a)
show "slice", h.slice(:a, :b, :z)
show "slice keeps permitted", P.new(a: 1).permit!.slice(:a).permitted?
show "except", h.except(:a, "b")
show "without", h.without(:a)
copy = h.dup
show "extract!", copy.extract!(:a, :z)
show "after extract!", copy
copy = h.dup
show "slice!", copy.slice!(:a, :i)
show "delete", h.dup.delete(:a)
show "delete missing", h.dup.delete(:z)
show "delete block", h.dup.delete(:z) { |k| "no #{k}" }
show "delete nested", h.dup.delete(:b)
show "transform_values", h.transform_values { |v| v.class.name }
copy = h.dup
copy.transform_values! { |v| v.is_a?(Integer) ? v + 1 : v }
show "transform_values!", copy
show "transform_keys", h.transform_keys { |k| k.upcase }
show "transform_keys to symbols", h.transform_keys(&:to_sym).keys
copy = h.dup
copy.transform_keys! { |k| "#{k}!" }
show "transform_keys!", copy
show "select", h.select { |k, v| v.is_a?(String) }
show "reject", h.reject { |k, v| v.nil? }
copy = h.dup
copy.select! { |k, _| k < "c" }
show "select!", copy
copy = h.dup
copy.keep_if { |k, _| k == "a" }
show "keep_if", copy
copy = h.dup
copy.reject! { |k, _| k == "a" }
show "reject!", copy
copy = h.dup
copy.delete_if { |k, _| k != "a" }
show "delete_if", copy
show "compact", h.compact
show "compact! none", P.new(a: 1).compact!
copy = h.dup
show "compact!", copy.compact!
show "compact_blank", h.compact_blank
copy = h.dup
show "compact_blank! returns self", copy.compact_blank!.equal?(copy)
show "compact_blank!", copy
show "has_value?", [h.has_value?(1), h.value?({ c: 2 }), h.has_value?(9)]
show "merge", h.merge(a: 2, z: 3)
show "merge hashes", h.merge({ a: 2 }, { "z" => 3 })
show "merge block", h.merge(a: 5) { |key, old, new| [key, old, new] }
attempt("merge unpermitted") { h.merge(P.new(q: 1)) }
show "merge permitted", h.merge(P.new(q: 1).permit!)
show "merge keeps permitted", P.new(a: 1).permit!.merge(b: 2).permitted?
copy = h.dup
copy.merge!(a: 7, y: { k: 1 })
show "merge!", copy
show "merge! nested", copy[:y]
show "reverse_merge", h.reverse_merge(a: 9, z: 8)
show "with_defaults", h.with_defaults("z" => 8)
copy = h.dup
copy.reverse_merge!(a: 9, z: 8)
show "reverse_merge!", copy
copy = h.dup
copy.with_defaults!(z: 1)
show "with_defaults!", copy
copy = h.dup
copy[:new] = { deep: { er: 1 } }
copy[:sym] = :value
show "[]=", copy
show "[]= nested", copy[:new][:deep]
show "stringify_keys", h.stringify_keys
show "stringify_keys copies", h.stringify_keys.equal?(h)
pairs = []
h.each_pair { |k, v| pairs << [k, v.class.name] }
show "each_pair", pairs
single = []
P.new(a: 1).each { |pair| single << pair }
show "each one arg", single
seen = []
h.each_value { |v| seen << v.class.name }
show "each_value", seen
keys = []
h.each_key { |k| keys << k }
show "each_key", keys
show "each_pair returns self", h.each_pair { |*| }.equal?(h)
show "extract_value", P.new(id: "1_2_3", tags: "a,b").extract_value(:id)
show "extract_value delimiter", P.new(tags: "a,b,").extract_value(:tags, delimiter: ",")
show "extract_value missing", P.new.extract_value(:id)
show "deconstruct_keys", P.new(a: 1, b: { c: 2 }).deconstruct_keys([:a, :b])
show "deconstruct_keys nil", P.new(a: 1).deconstruct_keys(nil)
matched = case P.new(name: "Ann", age: 3)
          in { name: String => n, age: Integer => a } then [n, a]
          else :no_match
          end
show "pattern match", matched

# Equality and identity.
show "== same", P.new(a: 1) == P.new("a" => 1)
show "== hash", P.new(a: 1) == { "a" => 1 }
show "== permitted differs", P.new(a: 1) == P.new(a: 1).permit!
show "== nested", P.new(a: { b: 1 }) == P.new(a: { b: 1 })
nested = P.new(a: { b: 1 })
nested[:a]
show "== after nested read", nested == P.new(a: { b: 1 })
show "eql?", [P.new(a: 1).eql?(P.new(a: 1)), P.new(a: 1).eql?({ "a" => 1 })]
show "hash equal", P.new(a: 1).hash == P.new(a: 1).hash
show "uniq", [P.new(a: 1), P.new(a: 1), P.new(a: 2)].uniq.size

# Copies.
orig = P.new(a: "s", b: { c: "t" }, l: [1])
d = orig.dup
d[:a] = "changed"
d[:b][:c] = "shared"
show "dup independent top", orig[:a]
show "dup shares nested", orig[:b][:c]
dd = orig.deep_dup
dd[:b][:c] = "deep"
dd[:l] << 2
show "deep_dup independent", [orig[:b][:c], orig[:l]]
show "deep_dup permitted", P.new(a: 1).permit!.deep_dup.permitted?
show "dup permitted", P.new(a: 1).permit!.dup.permitted?

# permit_all_parameters.
P.permit_all_parameters = true
show "permit_all_parameters new", P.new(a: 1).permitted?
P.permit_all_parameters = false
show "permit_all_parameters reset", P.new(a: 1).permitted?

# YAML: Rails' encode_with / init_with (the `!ruby/object:` form).
dumped = YAML.dump(P.new(a: 1, b: { c: "x" }))
show "yaml dump", dumped
loaded = YAML.unsafe_load(dumped)
show "yaml load class", loaded.class.name
show "yaml load", loaded
show "yaml load read", loaded[:b][:c]
permitted_dump = YAML.dump(P.new(a: 1).permit!)
show "yaml permitted load", YAML.unsafe_load(permitted_dump).permitted?
