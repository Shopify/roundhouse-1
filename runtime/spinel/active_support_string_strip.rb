# frozen_string_literal: true

# String#strip_heredoc (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/string/strip.rb), verbatim;
# Rails' doc comments dropped. `require "active_support/core_ext/string/strip"`
# from a native gem (typed_parameters' deprecated_parameters.rb) loads it
# (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "strscan"

class String
  def strip_heredoc
    scanner = StringScanner.new("")

    min_indent_len = nil
    lines = self.lines

    lines.each do |line|
      scanner.string = line
      indent_len = scanner.skip(/[ \t]*/)

      next unless scanner.match?(/[^\r\n]/)

      min_indent_len = indent_len if indent_len < (min_indent_len || Float::INFINITY)
    end

    if min_indent_len.nil? || min_indent_len.zero?
      return frozen? ? self : dup
    end

    lines.each do |line|
      line[0, min_indent_len] = "" unless line == "\n"
    end

    result = lines.join

    result.freeze if frozen?
    result
  end
end
