# frozen_string_literal: true

# active_support/i18n.rb (Shopify/rails 5813559f4aed, 8.2.0.alpha):
# loads the i18n gem with its Fallbacks backend module and puts Active
# Support's English locale on I18n's load path, after running the `:i18n`
# load hooks. `require "rails"` loads it before Bundler.require, so every
# Rails app has these two files on `I18n.load_path`: `en.yml` (number,
# currency, date and array formats: `to_sentence`'s connectors, the
# number helpers' units) and `en.rb` (the `number.nth` ordinal procs
# ActiveSupport::Inflector's `ordinal`/`ordinalize` translate).
#
# Both files are carried verbatim (runtime/active_support/locale/). The
# i18n gem itself is loaded from the gem home, as Bundler loads it: it
# depends on concurrent-ruby alone, nothing of Rails.
#
# Not here: the I18n railtie (active_support/i18n_railtie.rb), which
# applies `config.i18n` at boot (runtime/active_support_i18n_railtie.rb).
#
# The PostgreSQL CRuby tree whose lockfile has i18n only
# (`project::apply_postgres_bootstrap`).

require "i18n"
require "i18n/backend/fallbacks"
require_relative "active_support_lazy_load_hooks"
require_relative "active_support_ractors"

ActiveSupport.run_load_hooks(:i18n)
I18n.load_path << File.expand_path("active_support/locale/en.yml", __dir__)
I18n.load_path << File.expand_path("active_support/locale/en.rb", __dir__)
