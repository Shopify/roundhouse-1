# frozen_string_literal: true

# activesupport/lib/active_support/core_ext/string/filters.rb (Shopify/rails
# 5813559f4aed, 8.2.0.alpha), verbatim; Rails' doc comments dropped.
# `require "active_support/core_ext/string/filters"` from a native gem loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

class String
  def squish
    dup.squish!
  end

  def squish!
    gsub!(/([[:space:]]{2,}|[[[:space:]]&&[^ ]])/, " ")
    strip!
    self
  end

  def remove(*patterns)
    dup.remove!(*patterns)
  end

  def remove!(*patterns)
    patterns.each do |pattern|
      gsub! pattern, ""
    end

    self
  end

  def truncate(truncate_to, options = {})
    return dup unless length > truncate_to

    omission = options[:omission] || "..."
    length_with_room_for_omission = truncate_to - omission.length
    return omission.dup if length_with_room_for_omission <= 0

    stop = \
      if options[:separator]
        rindex(options[:separator], length_with_room_for_omission) || length_with_room_for_omission
      else
        length_with_room_for_omission
      end

    +"#{self[0, stop]}#{omission}"
  end

  def truncate_bytes(truncate_to, omission: "…")
    omission ||= ""

    case
    when bytesize <= truncate_to
      dup
    when omission.bytesize > truncate_to
      raise ArgumentError, "Omission #{omission.inspect} is #{omission.bytesize}, larger than the truncation length of #{truncate_to} bytes"
    when omission.bytesize == truncate_to
      omission.dup
    else
      self.class.new.force_encoding(encoding).tap do |cut|
        cut_at = truncate_to - omission.bytesize

        each_grapheme_cluster do |grapheme|
          if cut.bytesize + grapheme.bytesize <= cut_at
            cut << grapheme
          else
            break
          end
        end

        cut << omission
      end
    end
  end

  def truncate_words(words_count, options = {})
    sep = options[:separator] || /\s+/
    sep = Regexp.escape(sep.to_s) unless Regexp === sep
    if self =~ /\A((?>.+?#{sep}){#{words_count - 1}}.+?)#{sep}.*/m
      $1 + (options[:omission] || "...")
    else
      dup
    end
  end
end
