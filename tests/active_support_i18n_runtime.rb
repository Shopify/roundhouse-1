# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for active_support/i18n.rb and ActiveSupport::Ractors
# against runtime/spinel/active_support_i18n.rb and
# runtime/spinel/active_support_ractors.rb.
#
#   ruby tests/active_support_i18n_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activesupport's
# active_support/i18n (what `require "rails"` loads); `emitted` loads the
# ports as the emitted PostgreSQL tree's gem_facades.rb does, after the
# Inflector port. Each prints one line per observation: the load path
# (each file by its last three path segments), the locales and fallbacks
# I18n answers with nothing configured, Active Support's English
# translations (array connectors, number units, date formats), the
# ordinal procs of en.rb through ActiveSupport::Inflector, and the
# Ractors shims those procs are built with. `compare` requires them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = mode == "emitted" ? RailsOracle.core_gems_env : {}
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "I18n differs (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted active_support/i18n agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport")
  require "active_support"
  require "active_support/i18n"
  require "active_support/inflector"
else
  require_relative "../runtime/spinel/module_delegate"
  require_relative "../runtime/spinel/active_support_inflector"
  require_relative "../runtime/spinel/active_support_i18n"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.chomp.inspect}"
end

show "load_path", I18n.load_path.map { |path| path.to_s.split("/").last(3).join("/") }
show "fallbacks backend defined", defined?(I18n::Backend::Fallbacks).to_s
show "I18n.fallbacks", outcome { I18n.fallbacks[:"fr-CA"] }
show "available_locales", I18n.available_locales
show "default_locale", I18n.default_locale
show "enforce_available_locales", I18n.enforce_available_locales
show "backend", I18n.backend.class.name
%w[
  support.array.words_connector support.array.two_words_connector support.array.last_word_connector
  number.format.delimiter number.format.precision number.currency.format.unit number.currency.format.format
  number.human.storage_units.units.kb number.human.decimal_units.units.million
  date.formats.default date.formats.short date.abbr_day_names date.order
  time.formats.default time.am datetime.distance_in_words.x_minutes
  errors.format errors.messages.blank
].each do |key|
  show "t #{key}", outcome { I18n.t(key) }
end
show "t missing", outcome { I18n.t("roundhouse.missing") }
show "t fr", outcome { I18n.t("support.array.words_connector", locale: :fr) }
[1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111, 1002, -1, -11, -1021].each do |n|
  show "ordinalize #{n}", ActiveSupport::Inflector.ordinalize(n)
  show "ordinal #{n}", ActiveSupport::Inflector.ordinal(n)
end
show "nth procs lambda", I18n.t("number.nth").transform_values { |f| [f.class.name, f.lambda?] }
show "Ractors main?", ActiveSupport::Ractors.main?
show "Ractors unshareable_proc_action", ActiveSupport::Ractors.unshareable_proc_action
show "Ractors try_shareable_proc", outcome { ActiveSupport::Ractors.try_shareable_proc { 1 }.call }
show "Ractors shareable_lambda", outcome { f = ActiveSupport::Ractors.shareable_lambda { |x| x * 2 }; [f.lambda?, f.call(3), Ractor.shareable?(f)] }
show "Ractors make_shareable", outcome { ActiveSupport::Ractors.make_shareable([+"a"]).frozen? }
show "Ractors store_if_absent", outcome { ActiveSupport::Ractors.store_if_absent(:rh_probe) { 41 + 1 } }
show "Ractors []", ActiveSupport::Ractors[:rh_probe]
