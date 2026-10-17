# frozen_string_literal: true
# `ActionView::Helpers::TranslationHelper` for a class that includes it
# outside a view (Core's `Home::SafeTranslatable::Helper` includes it and
# `ShopifyI18n::ActionViewSoftDegrade` then wraps `translate` with a bare
# `super`). The members are the subset that needs no view context: a
# template's `t` is folded at compile time (`src/lower/i18n_translate.rs`),
# so what an includer reaches here is the run-time call over the real
# i18n gem, with Action View's answers for a missing key and for an
# `_html` key.
module ActionView
  module Helpers
    module TranslationHelper
      @raise_on_missing_translations = false

      class << self
        attr_accessor :raise_on_missing_translations
      end

      def translate(key, **options)
        return key.map { |k| translate(k, **options) } if key.is_a?(Array)

        key = key.to_s unless key.is_a?(Symbol)
        if key.to_s.start_with?(".")
          raise "Cannot use t(#{key.to_s.inspect}) shortcut because path is not available"
        end
        options[:raise] = true if options[:raise].nil? && TranslationHelper.raise_on_missing_translations

        missing = Object.new
        translation = I18n.translate(key, **options, default: options.key?(:default) ? options[:default] : missing)
        if missing.equal?(translation)
          return missing_translation(key, options)
        end
        if translation.is_a?(String) && html_safe_translation_key?(key)
          translation = translation.html_safe if translation.respond_to?(:html_safe)
        end
        translation
      end
      alias t translate

      def localize(object, **options)
        I18n.localize(object, **options)
      end
      alias l localize

      private

      def html_safe_translation_key?(key)
        /(?:_|\b)html\z/.match?(key.to_s)
      end

      def missing_translation(key, options)
        keys = I18n.normalize_keys(options[:locale] || I18n.locale, key, options[:scope]) #: Array[Symbol]
        title = +"translation missing: #{keys.join(".")}"
        options.each do |name, value|
          unless name == :scope || name == :locale || name == :raise || name == :default
            title << ", #{name}: #{value}"
          end
        end
        if TranslationHelper.raise_on_missing_translations || options[:raise]
          raise I18n::MissingTranslationData.new(options[:locale] || I18n.locale, key, options)
        end
        "<span class=\"translation_missing\" title=\"#{ERB::Util.html_escape(title)}\">#{ERB::Util.html_escape(key.to_s.split(".").last.to_s.tr("_", " ").capitalize)}</span>"
      end
    end
  end
end
