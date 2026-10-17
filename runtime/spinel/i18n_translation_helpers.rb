# frozen_string_literal: true
# The helpers a folded `I18n.t` calls (`src/lower/i18n_translate.rs`),
# over the i18n gem on the PostgreSQL CRuby tree: `runtime/ruby/i18n_locale.rb`
# is the same set over a fixed `:en`, for trees without the gem. Here
# `I18n.locale` and `I18n::MissingTranslationData` are the gem's, so this
# file declares neither; it is inlined after the gem's require in the
# tree's runtime/i18n_locale.rb (`project::apply_postgres_bootstrap`).
module I18n
  # Whether this environment raises on a missing translation: `raises`
  # is `config/application.rb`'s setting, and the comma-separated
  # `flipped` environments set the other value in their own file.
  def self.missing_raises(raises, flipped)
    env = Rails.env.to_s
    flipped.split(",").include?(env) ? !raises : raises
  end

  # A run-time key as `I18n.normalize_keys` reads it: dot-separated,
  # empty segments dropped (`"a..b."` is `a.b`).
  def self.normalize_translation_key(key)
    out = []
    key.split(".").each { |part| out << part unless part.empty? }
    out.join(".")
  end

  # A run-time key that names a subtree, which `I18n.t` answers as a
  # Hash; no folded translation carries one.
  def self.subtree_translation(key)
    raise NotImplementedError, "I18n.t(\"#{key}\") answers a Hash of translations, which is not supported"
  end

  # `I18n.t`'s answer for `key` (`en.shop.title`, the locale first): the
  # gem's default handler returns the message,
  # `raise_on_missing_translations` raises it.
  def self.missing_translation(key, raises)
    if raises
      locale, _, rest = key.partition(".")
      raise MissingTranslationData.new(locale.to_sym, rest)
    end
    "Translation missing: " + key
  end

  # A template's `t` for `key`: Action View's debug span, titled with the
  # key and the call's options (`details`, each value escaped once), and
  # reading as the key's last segment titleized.
  def self.missing_translation_tag(key, details, raises)
    missing_translation(key, raises)
    last = key.split(".").last.to_s
    title = ActionView::ViewHelpers.html_escape("translation missing: " + key + details)
    text = ActionView::ViewHelpers.html_escape(ActiveSupport.titleize(last))
    "<span class=\"translation_missing\" title=\"" + title + "\">" + text + "</span>"
  end
end
