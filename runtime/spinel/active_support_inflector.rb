# frozen_string_literal: true

# ActiveSupport::Inflector (Shopify/rails 5813559f4aed, 8.2.0.alpha),
# verbatim, in the order active_support/inflector.rb requires it:
# inflector/inflections.rb (Inflections, Uncountables, `inflections`),
# inflector/transliterate.rb, inflector/methods.rb and
# active_support/inflections.rb (the default English rules). Rails loads
# it with active_support before any app code; Core's boot prepends a
# memoizing `underscore`/`camelize` onto its singleton class
# (essentials' patches/active_support/inflector_lru_memoization.rb) and
# gems call it by name.
#
# Not carried: I18n. The `:en` paths never read it, but `ordinal`,
# `ordinalize`, a non-ASCII `transliterate` (so `parameterize` of a
# non-ASCII string) and `inflections(locale)` for another locale do, and
# raise NameError naming I18n here. Nor core_ext/string/inflections.rb:
# the CRuby overlay's String methods stay as they are.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

# ---- activesupport/lib/active_support/inflector/inflections.rb
require "concurrent/map"

module ActiveSupport
  module Inflector
    extend self

    # Active Support \Inflections
    # ===========================
    #
    # A singleton instance of this class is yielded by Inflector.inflections,
    # which can then be used to specify additional inflection rules. If passed
    # an optional locale, rules for other languages can be specified. The
    # default locale is `:en`. Only rules for English are provided.
    #
    # ```
    # ActiveSupport::Inflector.inflections(:en) do |inflect|
    #   inflect.plural /^(ox)$/i, '\1\2en'
    #   inflect.singular /^(ox)en/i, '\1'
    #
    #   inflect.irregular 'cactus', 'cacti'
    #
    #   inflect.uncountable 'equipment'
    # end
    # ```
    #
    # New rules are added at the top. So in the example above, the irregular
    # rule for cactus will now be the first of the pluralization and
    # singularization rules that is runs. This guarantees that your rules run
    # before any of the rules that may already have been loaded.
    class Inflections
      @__instance__ = Concurrent::Map.new
      @__en_instance__ = nil

      class Uncountables # :nodoc:
        include Enumerable

        delegate :each, :pop, :empty?, :to_s, :==, :to_a, :to_ary, to: :@members

        def initialize
          @members = []
          @pattern = nil
        end

        def freeze
          build_pattern
          @members.each(&:freeze)
          @members.freeze
          @pattern.freeze

          super
        end

        def delete(entry)
          @members.delete(entry)
          @pattern = nil
        end

        def <<(word)
          word = word.downcase
          @members << word
          @pattern = nil
          self
        end

        def flatten
          @members.dup
        end

        def add(words)
          words = words.flatten.map(&:downcase)
          @members.concat(words)
          @pattern = nil
          self
        end

        def uncountable?(str)
          if @pattern.nil?
            build_pattern
          end
          @pattern.match?(str)
        end

        private
          def build_pattern
            members_pattern = Regexp.union(@members.map { |w| /#{Regexp.escape(w)}/i })
            @pattern = /\b#{members_pattern}\Z/i
          end
      end

      def self.instance(locale = :en)
        return @__en_instance__ ||= new if locale == :en

        @__instance__[locale] ||= new
      end

      def self.all_instances # :nodoc:
        [@__en_instance__] + @__instance__.values
      end

      def self.instance_or_fallback(locale)
        return @__en_instance__ ||= new if locale == :en

        I18n.fallbacks[locale].each do |k|
          return @__en_instance__ if k == :en && @__en_instance__
          return @__instance__[k] if @__instance__.key?(k)
        end
        instance(locale)
      end

      attr_reader :plurals, :singulars, :uncountables, :humans, :acronyms

      attr_reader :acronyms_camelize_regex, :acronyms_underscore_regex # :nodoc:

      def initialize
        @plurals, @singulars, @uncountables, @humans, @acronyms = [], [], Uncountables.new, [], {}
        define_acronym_regex_patterns
      end

      def freeze
        freeze_rules(@plurals)
        freeze_rules(@singulars)
        freeze_rules(@humans)

        @uncountables.freeze
        @acronyms.freeze

        super
      end

      # Private, for the test suite.
      def initialize_dup(orig) # :nodoc:
        %w(plurals singulars uncountables humans acronyms).each do |scope|
          instance_variable_set("@#{scope}", orig.public_send(scope).dup)
        end
        define_acronym_regex_patterns
      end

      # Specifies a new acronym. An acronym must be specified as it will appear
      # in a camelized string. An underscore string that contains the acronym
      # will retain the acronym when passed to `camelize`, `humanize`, or
      # `titleize`. A camelized string that contains the acronym will maintain
      # the acronym when titleized or humanized, and will convert the acronym
      # into a non-delimited single lowercase word when passed to `underscore`.
      #
      # ```
      # acronym 'HTML'
      # titleize 'html'     # => 'HTML'
      # camelize 'html'     # => 'HTML'
      # underscore 'MyHTML' # => 'my_html'
      # ```
      #
      # The acronym, however, must occur as a delimited unit and not be part of
      # another word for conversions to recognize it:
      #
      # ```
      # acronym 'HTTP'
      # camelize 'my_http_delimited' # => 'MyHTTPDelimited'
      # camelize 'https'             # => 'Https', not 'HTTPs'
      # underscore 'HTTPS'           # => 'http_s', not 'https'
      #
      # acronym 'HTTPS'
      # camelize 'https'   # => 'HTTPS'
      # underscore 'HTTPS' # => 'https'
      # ```
      #
      # Note: Acronyms that are passed to `pluralize` will no longer be
      # recognized, since the acronym will not occur as a delimited unit in the
      # pluralized result. To work around this, you must specify the pluralized
      # form as an acronym as well:
      #
      # ```
      # acronym 'API'
      # camelize(pluralize('api')) # => 'Apis'
      #
      # acronym 'APIs'
      # camelize(pluralize('api')) # => 'APIs'
      # ```
      #
      # `acronym` may be used to specify any word that contains an acronym or
      # otherwise needs to maintain a non-standard capitalization. The only
      # restriction is that the word must begin with a capital letter.
      #
      # ```
      # acronym 'RESTful'
      # underscore 'RESTful'           # => 'restful'
      # underscore 'RESTfulController' # => 'restful_controller'
      # titleize 'RESTfulController'   # => 'RESTful Controller'
      # camelize 'restful'             # => 'RESTful'
      # camelize 'restful_controller'  # => 'RESTfulController'
      #
      # acronym 'McDonald'
      # underscore 'McDonald' # => 'mcdonald'
      # camelize 'mcdonald'   # => 'McDonald'
      # ```
      def acronym(word)
        @acronyms[word.downcase] = word
        define_acronym_regex_patterns
      end

      # Specifies a new pluralization rule and its replacement. The rule can
      # either be a string or a regular expression. The replacement should
      # always be a string that may include references to the matched data from
      # the rule.
      def plural(rule, replacement)
        @uncountables.delete(rule) if rule.is_a?(String)
        @uncountables.delete(replacement)
        @plurals.prepend([rule, replacement])
      end

      # Specifies a new singularization rule and its replacement. The rule can
      # either be a string or a regular expression. The replacement should
      # always be a string that may include references to the matched data from
      # the rule.
      def singular(rule, replacement)
        @uncountables.delete(rule) if rule.is_a?(String)
        @uncountables.delete(replacement)
        @singulars.prepend([rule, replacement])
      end

      # Specifies a new irregular that applies to both pluralization and
      # singularization at the same time. This can only be used for strings, not
      # regular expressions. You simply pass the irregular in singular and
      # plural form.
      #
      # ```
      # irregular 'cactus', 'cacti'
      # irregular 'person', 'people'
      # ```
      def irregular(singular, plural)
        @uncountables.delete(singular)
        @uncountables.delete(plural)

        s0 = singular[0]
        srest = singular[1..-1]

        p0 = plural[0]
        prest = plural[1..-1]

        if s0.upcase == p0.upcase
          plural(/(#{s0})#{srest}$/i, '\1' + prest)
          plural(/(#{p0})#{prest}$/i, '\1' + prest)

          singular(/(#{s0})#{srest}$/i, '\1' + srest)
          singular(/(#{p0})#{prest}$/i, '\1' + srest)
        else
          plural(/#{s0.upcase}(?i)#{srest}$/,   p0.upcase   + prest)
          plural(/#{s0.downcase}(?i)#{srest}$/, p0.downcase + prest)
          plural(/#{p0.upcase}(?i)#{prest}$/,   p0.upcase   + prest)
          plural(/#{p0.downcase}(?i)#{prest}$/, p0.downcase + prest)

          singular(/#{s0.upcase}(?i)#{srest}$/,   s0.upcase   + srest)
          singular(/#{s0.downcase}(?i)#{srest}$/, s0.downcase + srest)
          singular(/#{p0.upcase}(?i)#{prest}$/,   s0.upcase   + srest)
          singular(/#{p0.downcase}(?i)#{prest}$/, s0.downcase + srest)
        end
      end

      # Specifies words that are uncountable and should not be inflected.
      #
      # ```
      # uncountable 'money'
      # uncountable 'money', 'information'
      # uncountable %w( money information rice )
      # ```
      def uncountable(*words)
        @uncountables.add(words)
      end

      # Specifies a humanized form of a string by a regular expression rule or
      # by a string mapping. When using a regular expression based replacement,
      # the normal humanize formatting is called after the replacement. When a
      # string is used, the human form should be specified as desired (example:
      # 'The name', not 'the_name').
      #
      # ```
      # human /_cnt$/i, '\1_count'
      # human 'legacy_col_person_name', 'Name'
      # ```
      def human(rule, replacement)
        @humans.prepend([rule, replacement])
      end

      # Clears the loaded inflections within a given scope (default is
      # `:all`). Give the scope as a symbol of the inflection type, the
      # options are: `:plurals`, `:singulars`, `:uncountables`,
      # `:humans`, `:acronyms`.
      #
      # ```
      # clear :all
      # clear :plurals
      # ```
      def clear(scope = :all)
        case scope
        when :all
          clear(:acronyms)
          clear(:plurals)
          clear(:singulars)
          clear(:uncountables)
          clear(:humans)
        when :acronyms
          @acronyms = {}
          define_acronym_regex_patterns
        when :uncountables
          @uncountables = Uncountables.new
        when :plurals, :singulars, :humans
          instance_variable_set "@#{scope}", []
        end
      end

      private
        def define_acronym_regex_patterns
          sorted_acronyms = @acronyms.empty? ? [] : @acronyms.values.sort_by { |a| -a.length }
          @acronym_regex             = sorted_acronyms.empty? ? /(?=a)b/ : /#{sorted_acronyms.join("|")}/
          @acronyms_camelize_regex   = /^(?:#{@acronym_regex}(?=\b|[A-Z_])|\w)/
          @acronyms_underscore_regex = /(?:(?<=([A-Za-z\d]))|\b)(#{@acronym_regex})(?=\b|[^a-z])/
        end

        def freeze_rules(rules)
          rules.each do |pair|
            pair.each(&:freeze)
            pair.freeze
          end
          rules.freeze
        end
    end

    # Yields a singleton instance of Inflector::Inflections so you can specify
    # additional inflector rules. If passed an optional locale, rules for other
    # languages can be specified. If not specified, defaults to `:en`.
    # Only rules for English are provided.
    #
    # ```
    # ActiveSupport::Inflector.inflections(:en) do |inflect|
    #   inflect.uncountable 'rails'
    # end
    # ```
    def inflections(locale = :en)
      if block_given?
        yield Inflections.instance(locale)
      else
        Inflections.instance_or_fallback(locale)
      end
    end
  end
end

# ---- activesupport/lib/active_support/inflector/transliterate.rb
module ActiveSupport
  module Inflector
    ALLOWED_ENCODINGS_FOR_TRANSLITERATE = [Encoding::UTF_8, Encoding::US_ASCII, Encoding::GB18030].freeze

    # Replaces non-ASCII characters with an ASCII approximation, or if none
    # exists, a replacement character which defaults to "?".
    #
    # ```
    # transliterate('Ærøskøbing')
    # # => "AEroskobing"
    # ```
    #
    # Default approximations are provided for Western/Latin characters,
    # e.g., "ø", "ñ", "é", "ß", etc.
    #
    # This method is I18n aware, so you can set up custom approximations for a
    # locale. This can be useful, for example, to transliterate German's "ü"
    # and "ö" to "ue" and "oe", or to add support for transliterating Russian
    # to ASCII.
    #
    # In order to make your custom transliterations available, you must set
    # them as the `i18n.transliterate.rule` i18n key:
    #
    # ```
    # # Store the transliterations in locales/de.yml
    # i18n:
    #   transliterate:
    #     rule:
    #       ü: "ue"
    #       ö: "oe"
    #
    # # Or set them using Ruby
    # I18n.backend.store_translations(:de, i18n: {
    #   transliterate: {
    #     rule: {
    #       'ü' => 'ue',
    #       'ö' => 'oe'
    #     }
    #   }
    # })
    # ```
    #
    # The value for `i18n.transliterate.rule` can be a simple Hash that
    # maps characters to ASCII approximations as shown above, or, for more
    # complex requirements, a Proc:
    #
    # ```
    # I18n.backend.store_translations(:de, i18n: {
    #   transliterate: {
    #     rule: ->(string) { MyTransliterator.transliterate(string) }
    #   }
    # })
    # ```
    #
    # Now you can have different transliterations for each locale:
    #
    # ```
    # transliterate('Jürgen', locale: :en)
    # # => "Jurgen"
    #
    # transliterate('Jürgen', locale: :de)
    # # => "Juergen"
    # ```
    #
    # Transliteration is restricted to UTF-8, US-ASCII, and GB18030 strings.
    # Other encodings will raise an ArgumentError.
    def transliterate(string, replacement = "?", locale: nil)
      raise ArgumentError, "Can only transliterate strings. Received #{string.class.name}" unless string.is_a?(String)
      raise ArgumentError, "Cannot transliterate strings with #{string.encoding} encoding" unless ALLOWED_ENCODINGS_FOR_TRANSLITERATE.include?(string.encoding)

      return string.dup if string.ascii_only?

      # Operate on a copy so the in-place force_encoding/encode! below never
      # mutate the caller's (possibly non-frozen) string.
      string = string.dup

      input_encoding = string.encoding

      # US-ASCII is a subset of UTF-8 so we'll force encoding as UTF-8 if
      # US-ASCII is given. This way we can let tidy_bytes handle the string
      # in the same way as we do for UTF-8
      string.force_encoding(Encoding::UTF_8) if string.encoding == Encoding::US_ASCII

      # GB18030 is Unicode compatible but is not a direct mapping so needs to be
      # transcoded. Using invalid/undef :replace will result in loss of data in
      # the event of invalid characters, but since tidy_bytes will replace
      # invalid/undef with a "?" we're safe to do the same beforehand
      string.encode!(Encoding::UTF_8, invalid: :replace, undef: :replace) if string.encoding == Encoding::GB18030

      transliterated = I18n.transliterate(
        ActiveSupport::Multibyte::Unicode.tidy_bytes(string).unicode_normalize(:nfc),
        replacement: replacement,
        locale: locale
      )

      # Restore the string encoding of the input if it was not UTF-8.
      # Apply invalid/undef :replace as tidy_bytes does
      transliterated.encode!(input_encoding, invalid: :replace, undef: :replace) if input_encoding != transliterated.encoding

      transliterated
    end

    # Replaces special characters in a string so that it may be used as part of
    # a 'pretty' URL.
    #
    # ```
    # parameterize("Donald E. Knuth") # => "donald-e-knuth"
    # parameterize("^très|Jolie-- ")  # => "tres-jolie"
    # ```
    #
    # To use a custom separator, override the `separator` argument.
    #
    # ```
    # parameterize("Donald E. Knuth", separator: '_') # => "donald_e_knuth"
    # parameterize("^très|Jolie__ ", separator: '_')  # => "tres_jolie"
    # ```
    #
    # To remove the unwanted characters instead of replacing them, pass an
    # empty separator. `nil` is treated the same way.
    #
    # ```
    # parameterize("Donald E. Knuth", separator: '') # => "donaldeknuth"
    # ```
    #
    # To preserve the case of the characters in a string, use the `preserve_case` argument.
    #
    # ```
    # parameterize("Donald E. Knuth", preserve_case: true) # => "Donald-E-Knuth"
    # parameterize("^très|Jolie-- ", preserve_case: true) # => "tres-Jolie"
    # ```
    #
    # It preserves dashes and underscores unless they are used as separators:
    #
    # ```
    # parameterize("^très|Jolie__ ")                 # => "tres-jolie__"
    # parameterize("^très|Jolie-- ", separator: "_") # => "tres_jolie--"
    # parameterize("^très_Jolie-- ", separator: ".") # => "tres_jolie--"
    # ```
    #
    # If the optional parameter `locale` is specified,
    # the word will be parameterized as a word of that language.
    # By default, this parameter is set to `nil` and it will use
    # the configured `I18n.locale`.
    def parameterize(string, separator: "-", preserve_case: false, locale: nil)
      separator ||= ""

      # Replace accented chars with their ASCII equivalents.
      parameterized_string = transliterate(string, locale: locale)

      # Turn unwanted chars into the separator.
      parameterized_string.gsub!(/[^a-z0-9\-_]+/i, separator)

      unless separator.empty?
        # No more than one of the separator in a row.
        if separator.length == 1
          parameterized_string.squeeze!(separator)
        else
          re_sep = Regexp.escape(separator)
          parameterized_string.gsub!(/#{re_sep}{2,}/, separator)
        end
        # Remove leading/trailing separator.
        parameterized_string.delete_prefix!(separator)
        parameterized_string.delete_suffix!(separator)
      end

      parameterized_string.downcase! unless preserve_case
      parameterized_string
    end
  end
end

# ---- activesupport/lib/active_support/inflector/methods.rb
module ActiveSupport
  # Active Support \Inflector
  # =========================
  #
  # The Inflector transforms words from singular to plural, class names to table
  # names, modularized class names to ones without, and class names to foreign
  # keys. The default inflections for pluralization, singularization, and
  # uncountable words are kept in inflections.rb.
  #
  # The \Rails core team has stated patches for the inflections library will not
  # be accepted in order to avoid breaking legacy applications which may be
  # relying on errant inflections. If you discover an incorrect inflection and
  # require it for your application or wish to define rules for languages other
  # than English, please correct or add them yourself (explained below).
  module Inflector
    extend self

    # Returns the plural form of the word in the string.
    #
    # If passed an optional `locale` parameter, the word will be
    # pluralized using rules defined for that language. By default,
    # this parameter is set to `:en`.
    #
    # ```
    # pluralize('post')             # => "posts"
    # pluralize('octopus')          # => "octopi"
    # pluralize('sheep')            # => "sheep"
    # pluralize('words')            # => "words"
    # pluralize('CamelOctopus')     # => "CamelOctopi"
    # pluralize('ley', :es)         # => "leyes"
    # ```
    def pluralize(word, locale = :en)
      apply_inflections(word, inflections(locale).plurals, locale)
    end

    # The reverse of #pluralize, returns the singular form of a word in a
    # string.
    #
    # If passed an optional `locale` parameter, the word will be
    # singularized using rules defined for that language. By default,
    # this parameter is set to `:en`.
    #
    # ```
    # singularize('posts')            # => "post"
    # singularize('octopi')           # => "octopus"
    # singularize('sheep')            # => "sheep"
    # singularize('word')             # => "word"
    # singularize('CamelOctopi')      # => "CamelOctopus"
    # singularize('leyes', :es)       # => "ley"
    # ```
    def singularize(word, locale = :en)
      apply_inflections(word, inflections(locale).singulars, locale)
    end

    # Converts strings to UpperCamelCase.
    # If the `uppercase_first_letter` parameter is set to false, then produces
    # lowerCamelCase.
    #
    # Also converts '/' to '::' which is useful for converting
    # paths to namespaces.
    #
    # ```
    # camelize('active_model')                # => "ActiveModel"
    # camelize('active_model', false)         # => "activeModel"
    # camelize('active_model/errors')         # => "ActiveModel::Errors"
    # camelize('active_model/errors', false)  # => "activeModel::Errors"
    # ```
    #
    # As a rule of thumb you can think of `camelize` as the inverse of
    # #underscore, though there are cases where that does not hold:
    #
    # ```
    # camelize(underscore('SSLError'))        # => "SslError"
    # ```
    def camelize(term, uppercase_first_letter = true)
      string = term.to_s
      # String#camelize takes a symbol (:upper or :lower), so here we also support :lower to keep the methods consistent.
      if !uppercase_first_letter || uppercase_first_letter == :lower
        string = string.sub(inflections.acronyms_camelize_regex) { |match| match.downcase! || match }
      elsif string.match?(/\A[a-z\d]*\z/)
        return inflections.acronyms[string]&.dup || string.capitalize
      else
        string = string.sub(/^[a-z\d]*/) { |match| inflections.acronyms[match] || match.capitalize! || match }
      end
      string.gsub!(/(?:_|(\/))([a-z\d]*)/i) do
        word = $2
        substituted = inflections.acronyms[word] || word.capitalize! || word
        $1 ? "::#{substituted}" : substituted
      end
      string
    end

    # Makes an underscored, lowercase form from the expression in the string.
    #
    # Changes '::' to '/' to convert namespaces to paths.
    #
    # ```
    # underscore('ActiveModel')         # => "active_model"
    # underscore('ActiveModel::Errors') # => "active_model/errors"
    # ```
    #
    # As a rule of thumb you can think of `underscore` as the inverse of
    # #camelize, though there are cases where that does not hold:
    #
    # ```
    # camelize(underscore('SSLError'))  # => "SslError"
    # ```
    def underscore(camel_cased_word)
      return camel_cased_word.to_s.dup unless /[A-Z-]|::/.match?(camel_cased_word)
      word = camel_cased_word.to_s.gsub("::", "/")
      word.gsub!(inflections.acronyms_underscore_regex) { "#{$1 && '_' }#{$2.downcase}" }
      word.gsub!(/(?<=[A-Z])(?=[A-Z][a-z])|(?<=[a-z\d])(?=[A-Z])/, "_")
      word.tr!("-", "_")
      word.downcase!
      word
    end

    # Tweaks an attribute name for display to end users.
    #
    # Specifically, performs these transformations:
    #
    # * Applies human inflection rules to the argument.
    # * Deletes leading underscores, if any.
    # * Removes an "_id" suffix if present.
    # * Replaces underscores with spaces, if any.
    # * Downcases all words except acronyms.
    # * Capitalizes the first word.
    # The capitalization of the first word can be turned off by setting the
    # `:capitalize` option to false (default is true).
    #
    # The trailing '_id' can be kept by setting the
    # optional parameter `keep_id_suffix` to true (default is false).
    #
    # ```
    # humanize('employee_salary')                  # => "Employee salary"
    # humanize('author_id')                        # => "Author"
    # humanize('author_id', capitalize: false)     # => "author"
    # humanize('_id')                              # => "Id"
    # humanize('author_id', keep_id_suffix: true)  # => "Author id"
    # ```
    #
    # If "SSL" was defined to be an acronym:
    #
    # ```
    # humanize('ssl_error') # => "SSL error"
    # ```
    #
    def humanize(lower_case_and_underscored_word, capitalize: true, keep_id_suffix: false)
      result = lower_case_and_underscored_word.to_s.dup

      inflections.humans.each { |(rule, replacement)| break if result.sub!(rule, replacement) }

      result.tr!("_", " ")
      result.lstrip!
      if !keep_id_suffix && lower_case_and_underscored_word&.end_with?("_id")
        result.delete_suffix!(" id")
      end

      result.gsub!(/([[[:alpha:]]\d]+)/i) do |match|
        match.downcase!
        inflections.acronyms[match] || match
      end

      if capitalize
        result.sub!(/\A[[:alpha:]]/) do |match|
          match.upcase!
          match
        end
      end

      result
    end

    # Converts the first character in the string to uppercase.
    #
    # ```
    # upcase_first('what a Lovely Day') # => "What a Lovely Day"
    # upcase_first('w')                 # => "W"
    # upcase_first('')                  # => ""
    # ```
    def upcase_first(string)
      string.length > 0 ? string[0].upcase.concat(string[1..-1]) : +""
    end

    # Converts the first character in the string to lowercase.
    #
    # ```
    # downcase_first('If they enjoyed The Matrix') # => "if they enjoyed The Matrix"
    # downcase_first('I')                          # => "i"
    # downcase_first('')                           # => ""
    # ```
    def downcase_first(string)
      string.length > 0 ? string[0].downcase.concat(string[1..-1]) : +""
    end

    # Capitalizes all the words and replaces some characters in the string to
    # create a nicer looking title. `titleize` is meant for creating pretty
    # output. It is not used in the \Rails internals.
    #
    # The trailing '_id','Id'.. can be kept and capitalized by setting the
    # optional parameter `keep_id_suffix` to true.
    # By default, this parameter is false.
    #
    # ```
    # titleize('man from the boondocks')                       # => "Man From The Boondocks"
    # titleize('x-men: the last stand')                        # => "X Men: The Last Stand"
    # titleize('TheManWithoutAPast')                           # => "The Man Without A Past"
    # titleize('raiders_of_the_lost_ark')                      # => "Raiders Of The Lost Ark"
    # titleize('string_ending_with_id', keep_id_suffix: true)  # => "String Ending With Id"
    # ```
    def titleize(word, keep_id_suffix: false)
      humanize(underscore(word), keep_id_suffix: keep_id_suffix).gsub(/\b(?<!\w['’`()])\p{Lower}/) do |match|
        match.capitalize
      end
    end

    # Creates the name of a table like \Rails does for models to table names.
    # This method uses the #pluralize method on the last word in the string.
    #
    # ```
    # tableize('RawScaledScorer') # => "raw_scaled_scorers"
    # tableize('ham_and_egg')     # => "ham_and_eggs"
    # tableize('fancyCategory')   # => "fancy_categories"
    # ```
    def tableize(class_name)
      pluralize(underscore(class_name))
    end

    # Creates a class name from a plural table name like \Rails does for table
    # names to models. Note that this returns a string and not a Class. (To
    # convert to an actual class follow `classify` with #constantize.)
    #
    # ```
    # classify('ham_and_eggs') # => "HamAndEgg"
    # classify('posts')        # => "Post"
    # ```
    #
    # Singular names are not handled correctly:
    #
    # ```
    # classify('calculus')     # => "Calculu"
    # ```
    def classify(table_name)
      # strip out any leading schema name
      camelize(singularize(table_name.to_s.sub(/.*\./, "")))
    end

    # Replaces underscores with dashes in the string.
    #
    # ```
    # dasherize('puni_puni') # => "puni-puni"
    # ```
    def dasherize(underscored_word)
      underscored_word.tr("_", "-")
    end

    # Removes the module part from the expression in the string.
    #
    # ```
    # demodulize('ActiveSupport::Inflector::Inflections') # => "Inflections"
    # demodulize('Inflections')                           # => "Inflections"
    # demodulize('::Inflections')                         # => "Inflections"
    # demodulize('')                                      # => ""
    # ```
    #
    # See also #deconstantize.
    def demodulize(path)
      path = path.to_s
      if i = path.rindex("::")
        path[(i + 2), path.length]
      else
        path
      end
    end

    # Removes the rightmost segment from the constant expression in the string.
    #
    # ```
    # deconstantize('Net::HTTP')   # => "Net"
    # deconstantize('::Net::HTTP') # => "::Net"
    # deconstantize('String')      # => ""
    # deconstantize('::String')    # => ""
    # deconstantize('')            # => ""
    # ```
    #
    # See also #demodulize.
    def deconstantize(path)
      path.to_s[0, path.rindex("::") || 0] # implementation based on the one in facets' Module#spacename
    end

    # Creates a foreign key name from a class name.
    # `separate_class_name_and_id_with_underscore` sets whether
    # the method should put '_' between the name and 'id'.
    #
    # ```
    # foreign_key('Message')        # => "message_id"
    # foreign_key('Message', false) # => "messageid"
    # foreign_key('Admin::Post')    # => "post_id"
    # ```
    def foreign_key(class_name, separate_class_name_and_id_with_underscore = true)
      underscore(demodulize(class_name)) + (separate_class_name_and_id_with_underscore ? "_id" : "id")
    end

    # Tries to find a constant with the name specified in the argument string.
    #
    # ```
    # constantize('Module')   # => Module
    # constantize('Foo::Bar') # => Foo::Bar
    # ```
    #
    # The name is assumed to be the one of a top-level constant, no matter
    # whether it starts with "::" or not. No lexical context is taken into
    # account:
    #
    # ```
    # C = 'outside'
    # module M
    #   C = 'inside'
    #   C                # => 'inside'
    #   constantize('C') # => 'outside', same as ::C
    # end
    # ```
    #
    # NameError is raised when the name is not in CamelCase or the constant is
    # unknown.
    def constantize(camel_cased_word)
      Object.const_get(camel_cased_word)
    end

    # Tries to find a constant with the name specified in the argument string.
    #
    # ```
    # safe_constantize('Module')   # => Module
    # safe_constantize('Foo::Bar') # => Foo::Bar
    # ```
    #
    # The name is assumed to be the one of a top-level constant, no matter
    # whether it starts with "::" or not. No lexical context is taken into
    # account:
    #
    # ```
    # C = 'outside'
    # module M
    #   C = 'inside'
    #   C                     # => 'inside'
    #   safe_constantize('C') # => 'outside', same as ::C
    # end
    # ```
    #
    # `nil` is returned when the name is not in CamelCase or the constant (or
    # part of it) is unknown.
    #
    # ```
    # safe_constantize('blargle')                  # => nil
    # safe_constantize('UnknownModule')            # => nil
    # safe_constantize('UnknownModule::Foo::Bar')  # => nil
    # ```
    def safe_constantize(camel_cased_word)
      constantize(camel_cased_word)
    rescue NameError => e
      raise if e.name && !(camel_cased_word.to_s.split("::").include?(e.name.to_s) ||
        e.name.to_s == camel_cased_word.to_s)
    rescue LoadError => e
      message = e.respond_to?(:original_message) ? e.original_message : e.message
      raise unless /Unable to autoload constant #{const_regexp(camel_cased_word)}/.match?(message)
    end

    # Returns the suffix that should be added to a number to denote the position
    # in an ordered sequence such as 1st, 2nd, 3rd, 4th.
    #
    # ```
    # ordinal(1)     # => "st"
    # ordinal(2)     # => "nd"
    # ordinal(1002)  # => "nd"
    # ordinal(1003)  # => "rd"
    # ordinal(-11)   # => "th"
    # ordinal(-1021) # => "st"
    # ```
    def ordinal(number)
      I18n.translate("number.nth.ordinals", number: number)
    end

    # Turns a number into an ordinal string used to denote the position in an
    # ordered sequence such as 1st, 2nd, 3rd, 4th.
    #
    # ```
    # ordinalize(1)     # => "1st"
    # ordinalize(2)     # => "2nd"
    # ordinalize(1002)  # => "1002nd"
    # ordinalize(1003)  # => "1003rd"
    # ordinalize(-11)   # => "-11th"
    # ordinalize(-1021) # => "-1021st"
    # ```
    def ordinalize(number)
      I18n.translate("number.nth.ordinalized", number: number)
    end

    private
      # Mounts a regular expression, returned as a string to ease interpolation,
      # that will match part by part the given constant.
      #
      # ```
      # const_regexp("Foo::Bar::Baz") # => "Foo(::Bar(::Baz)?)?"
      # const_regexp("::")            # => "::"
      # ```
      def const_regexp(camel_cased_word)
        parts = camel_cased_word.split("::")

        return Regexp.escape(camel_cased_word) if parts.empty?

        last = parts.pop

        parts.reverse!.inject(last) do |acc, part|
          part.empty? ? acc : "#{part}(::#{acc})?"
        end
      end

      # Applies inflection rules for `singularize` and `pluralize`.
      #
      # If passed an optional `locale` parameter, the uncountables will be
      # found for that locale.
      #
      # ```
      # apply_inflections('post', inflections.plurals, :en)    # => "posts"
      # apply_inflections('posts', inflections.singulars, :en) # => "post"
      # ```
      def apply_inflections(word, rules, locale = :en)
        result = word.to_s.dup

        if word.empty? || inflections(locale).uncountables.uncountable?(result)
          result
        else
          rules.each { |(rule, replacement)| break if result.sub!(rule, replacement) }
          result
        end
      end
  end
end

# ---- activesupport/lib/active_support/inflections.rb
#--
# Defines the standard inflection rules. These are the starting point for
# new projects and are not considered complete. The current set of inflection
# rules is frozen. This means, we do not change them to become more complete.
# This is a safety measure to keep existing applications from breaking.
#++
module ActiveSupport
  Inflector.inflections(:en) do |inflect|
    inflect.plural(/$/, "s")
    inflect.plural(/s$/i, "s")
    inflect.plural(/^(ax|test)is$/i, '\1es')
    inflect.plural(/(octop|vir)us$/i, '\1i')
    inflect.plural(/(octop|vir)i$/i, '\1i')
    inflect.plural(/(alias|status)$/i, '\1es')
    inflect.plural(/(bu)s$/i, '\1ses')
    inflect.plural(/(buffal|tomat)o$/i, '\1oes')
    inflect.plural(/([ti])um$/i, '\1a')
    inflect.plural(/([ti])a$/i, '\1a')
    inflect.plural(/sis$/i, "ses")
    inflect.plural(/(?:([^f])fe|([lr])f)$/i, '\1\2ves')
    inflect.plural(/(hive)$/i, '\1s')
    inflect.plural(/([^aeiouy]|qu)y$/i, '\1ies')
    inflect.plural(/(x|ch|ss|sh)$/i, '\1es')
    inflect.plural(/(matr|vert|ind)(?:ix|ex)$/i, '\1ices')
    inflect.plural(/^(m|l)ouse$/i, '\1ice')
    inflect.plural(/^(m|l)ice$/i, '\1ice')
    inflect.plural(/^(ox)$/i, '\1en')
    inflect.plural(/^(oxen)$/i, '\1')
    inflect.plural(/(quiz)$/i, '\1zes')

    inflect.singular(/s$/i, "")
    inflect.singular(/(ss)$/i, '\1')
    inflect.singular(/(n)ews$/i, '\1ews')
    inflect.singular(/([ti])a$/i, '\1um')
    inflect.singular(/((a)naly|(b)a|(d)iagno|(p)arenthe|(p)rogno|(s)ynop|(t)he)(sis|ses)$/i, '\1sis')
    inflect.singular(/(^analy)(sis|ses)$/i, '\1sis')
    inflect.singular(/([^f])ves$/i, '\1fe')
    inflect.singular(/(hive)s$/i, '\1')
    inflect.singular(/(tive)s$/i, '\1')
    inflect.singular(/([lr])ves$/i, '\1f')
    inflect.singular(/([^aeiouy]|qu)ies$/i, '\1y')
    inflect.singular(/(s)eries$/i, '\1eries')
    inflect.singular(/(m)ovies$/i, '\1ovie')
    inflect.singular(/(x|ch|ss|sh)es$/i, '\1')
    inflect.singular(/^(m|l)ice$/i, '\1ouse')
    inflect.singular(/(bus)(es)?$/i, '\1')
    inflect.singular(/(o)es$/i, '\1')
    inflect.singular(/(shoe)s$/i, '\1')
    inflect.singular(/(cris|test)(is|es)$/i, '\1is')
    inflect.singular(/^(a)x[ie]s$/i, '\1xis')
    inflect.singular(/(octop|vir)(us|i)$/i, '\1us')
    inflect.singular(/(alias|status)(es)?$/i, '\1')
    inflect.singular(/^(ox)en/i, '\1')
    inflect.singular(/(vert|ind)ices$/i, '\1ex')
    inflect.singular(/(matr)ices$/i, '\1ix')
    inflect.singular(/(quiz)zes$/i, '\1')
    inflect.singular(/(database)s$/i, '\1')

    inflect.irregular("person", "people")
    inflect.irregular("man", "men")
    inflect.irregular("child", "children")
    inflect.irregular("sex", "sexes")
    inflect.irregular("move", "moves")
    inflect.irregular("zombie", "zombies")

    inflect.uncountable(%w(equipment information rice money species series fish sheep jeans police))
  end
end
