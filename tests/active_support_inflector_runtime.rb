# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActiveSupport::Inflector against
# runtime/spinel/active_support_inflector.rb.
#
#   ruby tests/active_support_inflector_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# active_support/inflector; `emitted` loads Module#delegate and the port
# as the emitted PostgreSQL tree's boot.rb does, on Core's gem home alone
# (bin/run-target's GEM_PATH). Each prints one line per observation and
# `compare` requires them equal. The observations are Rails' own
# inflector test tables (activesupport/test/inflector_test_cases.rb of
# the same checkout), Core's config/initializers/004_inflections.rb
# declarations, the Inflections API those declarations and the boot
# replay call, and essentials' patches/active_support/
# inflector_lru_memoization.rb prepended onto the singleton class.
#
# Not observed: what reads I18n (`ordinal`, `ordinalize`, a non-ASCII
# `transliterate` or `parameterize`, another locale's fallbacks), which
# the tree does not carry (the port's header).
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = {}
    if mode == "emitted"
      env = RailsOracle.core_gems_env.merge("GEM_HOME" => RailsOracle::CORE_GEM_HOME, "GEM_PATH" => RailsOracle::CORE_GEM_HOME)
    end
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "inflectors differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActiveSupport::Inflector agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/inflector"
else
  require_relative "../runtime/spinel/module_delegate"
  require_relative "../runtime/spinel/active_support_inflector"
end
# Both modes drop the tables' ActiveSupport::SafeBuffer keys (an Action
# View string type this port does not concern).
cases = File.join(RailsOracle.source, "activesupport/test/inflector_test_cases.rb")
eval(File.read(cases).lines.reject { |l| l.include?("SafeBuffer") }.join, TOPLEVEL_BINDING, cases)

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.chomp.inspect}"
end

I = ActiveSupport::Inflector

show("instance methods", I.instance_methods(false).sort)
show("private instance methods", I.private_instance_methods(false).sort)
show("Inflections public", I::Inflections.public_instance_methods(false).sort)
show("Uncountables public", I::Inflections::Uncountables.public_instance_methods(false).sort)
show("Uncountables ancestors", I::Inflections::Uncountables.ancestors.take(2).map(&:name))
en = I.inflections
show("inflections is the :en instance", [en.equal?(I::Inflections.instance), en.equal?(I.inflections(:en)), en.class.name])
show("default counts", [en.plurals.length, en.singulars.length, en.uncountables.to_a.length, en.humans.length, en.acronyms.length])
show("default uncountables", en.uncountables.to_a)
show("first plurals", en.plurals.first(3))
show("first singulars", en.singulars.first(3))

# Rails' own tables, on the default rules.
def tables(prefix)
  t = InflectorTestCases
  t::SingularToPlural.each do |singular, plural|
    show("#{prefix} pluralize #{singular}", [I.pluralize(singular), I.pluralize(plural), I.pluralize(singular.capitalize)])
    show("#{prefix} singularize #{plural}", [I.singularize(plural), I.singularize(singular), I.singularize(plural.capitalize)])
  end
  t::Irregularities.each { |s, p| show("#{prefix} irregular #{s}", [I.pluralize(s), I.singularize(p), I.pluralize(p), I.singularize(s)]) }
  t::CamelToUnderscore.each { |camel, under| show("#{prefix} camel #{camel}", [I.underscore(camel), I.camelize(under)]) }
  t::CamelToUnderscoreWithoutReverse.each { |camel, _| show("#{prefix} underscore only #{camel}", I.underscore(camel)) }
  t::CamelWithModuleToUnderscoreWithSlash.each { |camel, under| show("#{prefix} module #{camel}", [I.underscore(camel), I.camelize(under)]) }
  t::UnderscoreToLowerCamel.each { |under, _| show("#{prefix} lower camel #{under}", [I.camelize(under, false), I.camelize(under, :lower)]) }
  t::SymbolToLowerCamel.each { |sym, _| show("#{prefix} symbol camel #{sym}", I.camelize(sym, false)) }
  t::ClassNameToForeignKeyWithUnderscore.each { |k, _| show("#{prefix} foreign_key #{k}", [I.foreign_key(k), I.foreign_key(k, false)]) }
  t::ClassNameToTableName.each { |k, v| show("#{prefix} tableize #{k}", [I.tableize(k), I.classify(v), I.classify(v.to_sym)]) }
  t::UnderscoreToHuman.each { |k, _| show("#{prefix} humanize #{k}", [I.humanize(k), I.humanize(k, capitalize: false), I.humanize(k, keep_id_suffix: true)]) }
  t::MixtureToTitleCase.each { |k, _| show("#{prefix} titleize #{k}", [I.titleize(k), I.titleize(k, keep_id_suffix: true)]) }
  t::UnderscoresToDashes.each { |k, _| show("#{prefix} dasherize #{k}", I.dasherize(k)) }
  [
    t::StringToParameterized, t::StringToParameterizedPreserveCase, t::StringToParameterizeWithNoSeparator,
    t::StringToParameterizeWithUnderscore, t::StringToParameterizedAndNormalized,
  ].each_with_index do |table, n|
    table.each_key do |k|
      next unless k.ascii_only? # a non-ASCII string reaches I18n.transliterate
      show("#{prefix} parameterize #{n} #{k}", [I.parameterize(k), I.parameterize(k, preserve_case: true), I.parameterize(k, separator: "_"), I.parameterize(k, separator: ""), I.parameterize(k, separator: "__sep__")])
    end
  end
end
tables("default")

show("demodulize", ["MyApplication::Billing::Account", "Account", "::Account", ""].map { |w| I.demodulize(w) })
show("deconstantize", ["MyApplication::Billing::Account", "::Net::HTTP", "Account", "::Account", ""].map { |w| I.deconstantize(w) })
show("upcase/downcase_first", [I.upcase_first("what a Lovely Day"), I.upcase_first(""), I.downcase_first("If I had"), I.downcase_first("")])
show("constantize", [I.constantize("ActiveSupport::Inflector").name, outcome { I.constantize("Nope::Missing") }, outcome { I.constantize("") }])
show("safe_constantize", [I.safe_constantize("Comparable"), I.safe_constantize("Nope::Missing"), I.safe_constantize("blargle"), outcome { I.safe_constantize("ActiveSupport::Inflector::Nope") }])
show("transliterate ascii", [I.transliterate("Hello World"), outcome { I.transliterate(:sym) }, outcome { I.transliterate("abc".encode("UTF-16LE")) }])
show("camelize edge", [I.camelize(""), I.camelize("a"), I.camelize("ab/cd_ef"), I.camelize(nil), I.underscore(""), I.underscore(:"Foo::Bar")])
show("pluralize edge", [I.pluralize(""), I.singularize(""), I.pluralize("Status"), I.pluralize("hello world"), I.singularize("hello worlds")])

# Core's config/initializers/004_inflections.rb, as the tree's boot.rb
# replays it.
I.inflections(:en) do |inflect|
  %w[BAB IBAN GraphQL GeoIP MRuby TOS GCS GPV KYM B2B VM SQLite RPC GRPC VGS GS1].each { |a| inflect.acronym(a) }
  inflect.irregular("reserve", "reserves")
  inflect.irregular("named_delta", "named_deltas")
  inflect.irregular("subject_key_value_delta", "subject_key_value_deltas")
  %w[payment_details client_details dns hardware supported_hardware signup_metadata].each { |u| inflect.uncountable(u) }
end
show("core counts", [en.plurals.length, en.singulars.length, en.uncountables.to_a.length, en.acronyms.length])
show("core acronyms", en.acronyms)
show("core acronym regexes", [en.acronyms_camelize_regex.source, en.acronyms_underscore_regex.source])
%w[
  graphql_type GraphQLType api/graph_ql/schema GeoIPLookup geo_ip_lookup MRubyScript mruby_script TOSAcceptance
  b2b_customer B2BCustomer SQLiteAdapter sqlite_adapter GRPCClient grpc_client rpc_server GS1Barcode gs1_barcode
  VMWorker IBANValidator iban_validator payment_details client_details dns hardware supported_hardware
  signup_metadata reserve reserves named_delta named_deltas subject_key_value_delta Reserve
  shop_reserve platform/essentials/graph_ql_controller Shopify::GraphQL::Schema
].each do |word|
  show("core #{word}", [I.camelize(word), I.camelize(word, false), I.underscore(word), I.pluralize(word), I.singularize(word), I.humanize(word), I.titleize(word), I.tableize(word), I.classify(word)])
end
tables("core")

# Inflections API beyond the initializer.
show("Uncountables api", outcome {
  u = I::Inflections::Uncountables.new
  u << "Fish"
  u.add([["Sheep", "Rice"]])
  [u.to_a, u.uncountable?("big_fish"), u.uncountable?("fishes"), u.empty?, u == ["fish", "sheep", "rice"], u.to_s, u.flatten, u.delete("sheep"), u.to_a, u.pop, u.to_ary]
})
show("frozen Uncountables", outcome { u = I::Inflections::Uncountables.new.add(["x"]); u.freeze; [u.frozen?, u.to_a.frozen?, u.uncountable?("x")] })
copy = en.dup
copy.clear
show("dup is separate", [copy.plurals.length, en.plurals.length > 0, copy.acronyms, en.acronyms.length])
copy.clear(:plurals)
show("clear scopes", [copy.plurals, copy.singulars, copy.uncountables.to_a, copy.humans])
I.inflections do |inflect|
  inflect.human(/_cnt$/i, '\1_count')
  inflect.human("legacy_col_person_name", "Name")
  inflect.plural("quota", "quotas")
  inflect.singular("quotas", "quota")
  inflect.uncountable("quota")
  inflect.plural(/^(ox)$/i, '\1en')
  inflect.singular(/^(ox)en/i, '\1')
  inflect.irregular("cactus", "cacti")
end
show("human rules", [I.humanize("jargon_cnt"), I.humanize("legacy_col_person_name")])
show("string rules", [I.pluralize("quota"), I.singularize("quotas"), en.uncountables.to_a.include?("quota")])
show("regex and irregular rules", [I.pluralize("ox"), I.singularize("oxen"), I.pluralize("cactus"), I.singularize("cacti"), I.pluralize("Cactus")])
# Another locale's block form (`instance`); its reads go through
# `instance_or_fallback`, which reads I18n.fallbacks, and are not observed.
es = I.inflections(:es) { |inflect| inflect.plural(/$/, "s"); inflect.plural(/([^aeéiou])$/i, '\1es'); inflect }
show("es rules", [es.equal?(I::Inflections.instance(:es)), es.equal?(en), es.plurals.length, I.pluralize("libro")])
show("all_instances", I::Inflections.all_instances.length)

# essentials' patches/active_support/inflector_lru_memoization.rb.
require "lru_redux"
module ActiveSupportInflectorMemoization
  UNDERSCORE_CACHE = LruRedux::Cache.new(1_000)
  private_constant :UNDERSCORE_CACHE
  def underscore(camel_cased_word)
    +UNDERSCORE_CACHE.getset(camel_cased_word) { -super.to_s }
  end

  CAMELIZE_CACHE = LruRedux::Cache.new(50_000)
  private_constant :CAMELIZE_CACHE
  def camelize(term, uppercase_first_letter = true)
    if uppercase_first_letter
      +CAMELIZE_CACHE.getset(term) { -super }
    else
      super
    end
  end
end
ActiveSupport::Inflector.singleton_class.prepend(ActiveSupportInflectorMemoization)
show("memoized ancestors", ActiveSupport::Inflector.singleton_class.ancestors.take(2).map(&:to_s))
first = ActiveSupport::Inflector.underscore("GraphQLType")
second = ActiveSupport::Inflector.underscore("GraphQLType")
show("memoized underscore", [first, second, first.frozen?, first.equal?(second)])
show("memoized camelize", [ActiveSupport::Inflector.camelize("graphql_type"), ActiveSupport::Inflector.camelize("graphql_type").frozen?, ActiveSupport::Inflector.camelize("graphql_type", false)])
show("memoized tableize", [ActiveSupport::Inflector.tableize("GraphQLType"), ActiveSupport::Inflector.foreign_key("Shopify::GraphQLType")])
show("module_function form", [I.method(:underscore).owner.to_s, I.respond_to?(:pluralize), I.respond_to?(:apply_inflections), I.respond_to?(:apply_inflections, true)])
