# frozen_string_literal: true

# String's inflection methods, ported from the activesupport native Core
# runs (Shopify/rails 5813559f4aed, 8.2.0.alpha:
# lib/active_support/core_ext/string/inflections.rb) over the Inflector
# port (runtime/active_support_inflector.rb), for the PostgreSQL CRuby
# tree, which loads it with the port before any gem or app file
# (runtime/gem_facades.rb): `require "active_support"` loads them before
# Bundler.require. frozen_record 0.27.4's Backends::Yaml names a model's
# file `"#{model_name.underscore.pluralize}.yml"`. Each method follows the
# native one, and replaces the CRuby overlay's rule-subset `pluralize` and
# `singularize` (runtime/active_support_core_ext.rb) with the Inflector's.
#
# `parameterize` is carried where the tree carries I18n (the lockfile has
# i18n; runtime/active_support_i18n.rb loads before this file): the
# Inflector's transliterates a non-ASCII string through I18n and
# ActiveSupport::Multibyte::Unicode (runtime/
# active_support_multibyte_unicode.rb), "Ærøskøbing" => "aeroskobing".
# Without I18n the overlay's ASCII form stays.

class String
  def pluralize(count = nil, locale = :en)
    locale = count if count.is_a?(Symbol)
    if count == 1
      dup
    else
      ActiveSupport::Inflector.pluralize(self, locale)
    end
  end

  def singularize(locale = :en)
    ActiveSupport::Inflector.singularize(self, locale)
  end

  def constantize
    ActiveSupport::Inflector.constantize(self)
  end

  def safe_constantize
    ActiveSupport::Inflector.safe_constantize(self)
  end

  def camelize(first_letter = :upper)
    case first_letter
    when :upper
      ActiveSupport::Inflector.camelize(self, true)
    when :lower
      ActiveSupport::Inflector.camelize(self, false)
    else
      raise ArgumentError, "Invalid option, use either :upper or :lower."
    end
  end
  alias_method :camelcase, :camelize

  def titleize(keep_id_suffix: false)
    ActiveSupport::Inflector.titleize(self, keep_id_suffix: keep_id_suffix)
  end
  alias_method :titlecase, :titleize

  def underscore
    ActiveSupport::Inflector.underscore(self)
  end

  def dasherize
    ActiveSupport::Inflector.dasherize(self)
  end

  def demodulize
    ActiveSupport::Inflector.demodulize(self)
  end

  def deconstantize
    ActiveSupport::Inflector.deconstantize(self)
  end

  def tableize
    ActiveSupport::Inflector.tableize(self)
  end

  def classify
    ActiveSupport::Inflector.classify(self)
  end

  def humanize(capitalize: true, keep_id_suffix: false)
    ActiveSupport::Inflector.humanize(self, capitalize: capitalize, keep_id_suffix: keep_id_suffix)
  end

  def upcase_first
    ActiveSupport::Inflector.upcase_first(self)
  end

  def downcase_first
    ActiveSupport::Inflector.downcase_first(self)
  end

  def foreign_key(separate_class_name_and_id_with_underscore = true)
    ActiveSupport::Inflector.foreign_key(self, separate_class_name_and_id_with_underscore)
  end
end

if defined?(::I18n)
  class String
    def parameterize(separator: "-", preserve_case: false, locale: nil)
      ActiveSupport::Inflector.parameterize(self, separator: separator, preserve_case: preserve_case, locale: locale)
    end
  end
end
