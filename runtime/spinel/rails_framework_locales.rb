# frozen_string_literal: true

# The `:i18n` load hooks Active Model, Active Record and Action View
# register as they load (Shopify/rails 5813559f4aed, 8.2.0.alpha:
# active_model.rb:84, active_record.rb:699, action_view.rb:120), each
# putting its English locale on I18n's load path. An app requiring
# active_record/railtie and action_controller/railtie loads the three in
# that order, after `require "rails"` ran the `:i18n` hooks
# (active_support/i18n.rb), so each hook runs as it is registered and
# the three files follow Active Support's two on `I18n.load_path`.
#
# The locale files are carried verbatim (runtime/active_model/locale/,
# active_record/locale/, action_view/locale/). The PostgreSQL CRuby tree
# carries Active Model, Active Record and Action View's runtime, and
# with i18n in the lockfile it carries these, loaded with I18n::Railtie
# (runtime/gem_facades.rb) before any gem, as `require "rails"` and the
# framework railties load before Bundler.require.

require_relative "active_support_lazy_load_hooks"

ActiveSupport.on_load(:i18n) do
  I18n.load_path << File.expand_path("active_model/locale/en.yml", __dir__)
end

ActiveSupport.on_load(:i18n) do
  I18n.load_path << File.expand_path("active_record/locale/en.yml", __dir__)
end

ActiveSupport.on_load(:i18n) do
  I18n.load_path << File.expand_path("action_view/locale/en.yml", __dir__)
end
