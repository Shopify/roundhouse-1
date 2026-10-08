# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for I18n::Railtie (runtime/spinel/
# active_support_i18n_railtie.rb) on the emitted PostgreSQL tree: the
# app's `config.i18n.*`, the engines' and the app's config/locales
# (Rails::Engine's `add_locales`), a gem's railtie (phonelib 0.10.27's,
# loaded through a gem source as worldwide loads it in Core), the
# framework railties' locales, and Finisher's `eager_load!` over
# `config.eager_load_namespaces`.
#
#   ruby tests/i18n_railtie_runtime.rb compare APP_DIR TREE_DIR
#
# boots APP_DIR with real Rails (`native`) and the emitted TREE_DIR
# (`emitted`, after its boot.rb), each lazily and with the environment's
# `config.eager_load` on (PROBE_EAGER=1), and requires every observation
# equal: the load path in order, I18n's locales and fallbacks, the keys
# of `config.i18n`, translations from each source, the
# raise-on-missing exception handler, `config.i18n.locale = false`'s
# I18n::Disabled, and what eager loading loaded (I18n's backend,
# phonelib's data).
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  app, tree = ARGV[1], ARGV[2]
  abort "usage: #{$PROGRAM_NAME} compare APP_DIR TREE_DIR" unless app && tree
  report = []
  [nil, "1"].each do |eager|
    outputs = [["native", app, {}], ["emitted", tree, RailsOracle.core_gems_env]].map do |mode, dir, env|
      env = env.merge("PROBE_EAGER" => eager, "RAILS_ENV" => "test")
      stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode, File.expand_path(dir), chdir: dir)
      abort "#{mode} (eager=#{eager.inspect}): #{stderr}" unless status.success?
      [stdout, stderr]
    end
    native, emitted = outputs.map { |out, _| out.lines }
    equal = native.zip(emitted).count { |x, y| x == y }
    unless native == emitted
      diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
      abort "I18n::Railtie differs (eager=#{eager.inspect}; #{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
        diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n") +
        "\nemitted stderr:\n#{outputs.last.last.lines.grep(/roundhouse/).join}"
    end
    report << "== eager=#{eager.inspect}" << outputs.first.first
    report << "observations: #{equal}/#{native.length} equal"
  end
  puts report
  puts "Native Rails and emitted I18n::Railtie agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
ROOT = ARGV[1]
abort "usage: #{$PROGRAM_NAME} compare APP TREE | native APP | emitted TREE" unless %w[native emitted].include?(MODE) && ROOT

if MODE == "native"
  RailsOracle.activate("railties", "activerecord", "actionpack", "actionview", "activemodel")
  # config/application.rb's framework requires, then Bundler.require
  # (the lockfile's gems; phoneish, the gem source, requires phonelib),
  # then the application, as Core's boot runs them.
  require "rails"
  require "active_record/railtie"
  require "action_controller/railtie"
  require "action_view/railtie"
  require "i18n"
  require "phonelib"
  $LOAD_PATH.unshift(File.join(ROOT, "engines/widgets/lib"))
  require "widgets"
  require File.join(ROOT, "config/application")
  Rails.application.initialize!
else
  require File.join(ROOT, "boot")
end

# Rails' framework railties (Active Support, Action Dispatch, Active
# Model, Action View, Action Controller, Active Record) and the
# application add their namespaces too. The tree does not port those
# railties (each `eager_load!` is Zeitwerk's over the framework) and
# loads the app's own code itself (app/models.rb), so the comparison is
# of the namespaces the ported railties, the engines and the gems add.
UNPORTED_NAMESPACES = %w[ActiveSupport ActionDispatch Mime ActiveModel ActionView::Railtie ActionView AbstractController ActionController ActiveRecord TZInfo I18nProbe::Application].freeze

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.chomp.inspect}"
end

# A load path entry by where it comes from: phonelib's gem directory,
# a framework's locale directory (the checkout's, or the tree's verbatim
# copy), else relative to the app (or tree) root.
def tail(path)
  path = path.to_s
  if (gem = path[%r{/gems/(phonelib)-[^/]+/(.*)}, 0])
    gem.sub(%r{\A/gems/}, "")
  elsif (framework = path[%r{/(active_support|active_model|active_record|action_view)/locale/[^/]+\z}, 0])
    framework.delete_prefix("/")
  else
    path.delete_prefix(ROOT + "/")
  end
end

# Before any lookup: what eager loading loaded.
show "backend initialized", I18n.backend.initialized?
show "phonelib data loaded", !Phonelib::Core.class_variable_get(:@@phone_data).nil?
show "eager_load_namespaces", Rails.application.config.eager_load_namespaces.map(&:name) - UNPORTED_NAMESPACES
show "load_path", I18n.load_path.map { |p| tail(p) }
show "available_locales", I18n.available_locales
show "default_locale", I18n.default_locale
show "locale", I18n.locale
show "enforce_available_locales", I18n.enforce_available_locales
show "fallbacks fr-CA", outcome { I18n.fallbacks[:"fr-CA"] }
show "fallbacks backend", I18n.backend.class.ancestors.include?(I18n::Backend::Fallbacks)
show "config.i18n keys", Rails.application.config.i18n.keys
show "railties_load_path files", Rails.application.config.i18n.railties_load_path.flat_map(&:expanded_files).map { |f| tail(f) }
show "t probe.hello", outcome { I18n.t("probe.hello", locale: :en) }
show "t probe.hello fr", outcome { I18n.t("probe.hello", locale: :fr) }
show "t probe.hello fr-CA", outcome { I18n.t("probe.hello", locale: :"fr-CA") }
show "t probe.shared", outcome { I18n.t("probe.shared", locale: :en) }
show "t probe.model", outcome { I18n.t("probe.model", locale: :en) }
show "t widgets.title", outcome { I18n.t("widgets.title", locale: :en) }
show "t widgets.title fr", outcome { I18n.t("widgets.title", locale: :fr) }
show "t widgets.title fr-CA", outcome { I18n.t("widgets.title", locale: :"fr-CA") }
show "t phonelib", outcome { I18n.t("errors.messages.phone_invalid", locale: :en) }
show "t active_model", outcome { I18n.t("errors.messages.blank", locale: :en) }
show "t active_record", outcome { I18n.t("errors.messages.taken", locale: :en) }
show "t action_view", outcome { I18n.t("datetime.distance_in_words.half_a_minute", locale: :en) }
show "t active_support", outcome { I18n.t("support.array.last_word_connector", locale: :en) }
show "t missing", outcome { I18n.t("probe.missing", locale: :en) }
show "t missing default", outcome { I18n.t("probe.missing", default: "d", locale: :en) }
show "t disabled", outcome { I18n.t("probe.hello") }
show "t de", outcome { I18n.t("probe.hello", locale: :de) }
show "exception_handler", I18n.exception_handler.class.name
