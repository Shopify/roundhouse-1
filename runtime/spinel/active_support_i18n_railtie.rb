# frozen_string_literal: true

# active_support/i18n_railtie.rb (Shopify/rails 5813559f4aed,
# 8.2.0.alpha): I18n::Railtie, the railtie `require "rails"` defines first
# (rails.rb -> active_support/railtie -> active_support/i18n_railtie). It
# owns `config.i18n`, an OrderedOptions every railtie's, engine's and the
# application's configuration share (Railtie::Configuration's options).
# Engines add their `config/locales` to `config.i18n.railties_load_path`
# (Rails::Engine's `add_locales` initializer, runtime/rails_engine.rb), a
# gem's railtie adds files to `config.i18n.load_path` (phonelib 0.10.27),
# and the application's `config.i18n.*` writes land here
# (project::i18n_config_block). `initialize_i18n` applies all of it to I18n
# once: on `before_eager_load` when the app eager loads, else on
# `after_initialize`.
#
# Ported verbatim, over this tree's Rails::Railtie (runtime/rails_railtie.rb)
# and the active_support/i18n port (runtime/active_support_i18n.rb), with
# one difference: the class is written `class I18n::Railtie` rather than
# nested in `module I18n`, so this file declares I18n::Railtie and not the
# i18n gem's I18n namespace (analyze::runtime_reads gates the names a
# port declares by tree). Every constant the body names is qualified, so
# lookup is unchanged.
#
# The reloader block runs only under `config.reloading_enabled?`, which
# this tree answers false: it never reloads code
# (runtime/rails_configuration.rb).
#
# The PostgreSQL CRuby tree whose lockfile has i18n only
# (`project::apply_postgres_bootstrap`); runtime/gem_facades.rb loads it
# before any gem, as `require "rails"` does before Bundler.require.

require_relative "active_support_i18n"
require_relative "active_support_ordered_options"
require_relative "rails_railtie"

class I18n::Railtie < Rails::Railtie
  config.i18n = ActiveSupport::OrderedOptions.new
  config.i18n.railties_load_path = []
  config.i18n.load_path = []
  config.i18n.fallbacks = ActiveSupport::OrderedOptions.new

  config.eager_load_namespaces << I18n

  # Make sure i18n is ready before eager loading, in case any eager loaded
  # code needs it.
  config.before_eager_load do |app|
    I18n::Railtie.initialize_i18n(app)
  end

  # i18n initialization needs to run after application initialization, since
  # initializers may configure i18n.
  #
  # If the application eager loaded, this was done on before_eager_load. The
  # hook is still OK, though, because initialize_i18n is idempotent.
  config.after_initialize do |app|
    I18n::Railtie.initialize_i18n(app)
  end

  @i18n_inited = false

  # Setup i18n configuration.
  def self.initialize_i18n(app)
    return if @i18n_inited

    fallbacks = app.config.i18n.delete(:fallbacks)

    # Avoid issues with setting the default_locale by disabling available locales
    # check while configuring.
    enforce_available_locales = app.config.i18n.delete(:enforce_available_locales)
    enforce_available_locales = I18n.enforce_available_locales if enforce_available_locales.nil?
    I18n.enforce_available_locales = false

    reloadable_paths = []
    app.config.i18n.each do |setting, value|
      case setting
      when :railties_load_path
        reloadable_paths = value
        app.config.i18n.load_path.unshift(*value.flat_map(&:expanded_files))
      when :load_path
        I18n.load_path += value
      when :raise_on_missing_translations
        strict = value == :strict
        setup_raise_on_missing_translations_config(app, strict)
      else
        I18n.public_send("#{setting}=", value)
      end
    end

    init_fallbacks(fallbacks) if fallbacks && validate_fallbacks(fallbacks)

    # Restore available locales check so it will take place from now on.
    I18n.enforce_available_locales = enforce_available_locales

    if app.config.reloading_enabled?
      directories = watched_dirs_with_extensions(reloadable_paths)
      reloader = app.config.file_watcher.new(I18n.load_path, directories) do
        I18n.load_path.delete_if { |path| path.to_s.start_with?(Rails.root.to_s) && !File.exist?(path) }
        I18n.load_path |= reloadable_paths.flat_map(&:existent)
      end

      app.reloaders << reloader
      app.reloader.to_run do
        reloader.execute_if_updated { require_unload_lock! }
      end
    end

    @i18n_inited = true
  end

  def self.setup_raise_on_missing_translations_config(app, strict)
    ActiveSupport.on_load(:action_view) do
      ActionView::Helpers::TranslationHelper.raise_on_missing_translations = app.config.i18n.raise_on_missing_translations
    end

    ActiveSupport.on_load(:active_model_translation) do
      ActiveModel::Translation.raise_on_missing_translations = app.config.i18n.raise_on_missing_translations if strict
    end

    if app.config.i18n.raise_on_missing_translations &&
        I18n.exception_handler.is_a?(I18n::ExceptionHandler) # Only override the i18n gem's default exception handler.

      I18n.exception_handler = ->(exception, *) {
        exception = exception.to_exception if exception.is_a?(I18n::MissingTranslation)
        raise exception
      }
    end
  end

  def self.include_fallbacks_module
    I18n.backend.class.include(I18n::Backend::Fallbacks)
  end

  def self.init_fallbacks(fallbacks)
    include_fallbacks_module

    args = \
      case fallbacks
      when ActiveSupport::OrderedOptions
        [*(fallbacks[:defaults] || []) << fallbacks[:map]].compact
      when Hash, Array
        Array.wrap(fallbacks)
      else # TrueClass
        [I18n.default_locale]
      end

    I18n.fallbacks = I18n::Locale::Fallbacks.new(*args)
  end

  def self.validate_fallbacks(fallbacks)
    case fallbacks
    when ActiveSupport::OrderedOptions
      !fallbacks.empty?
    when TrueClass, Array, Hash
      true
    else
      raise "Unexpected fallback type #{fallbacks.inspect}"
    end
  end

  def self.watched_dirs_with_extensions(paths)
    paths.each_with_object({}) do |path, result|
      result[path.absolute_current] = path.extensions
    end
  end
end
