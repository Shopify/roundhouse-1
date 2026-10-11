# Instance-method form of ActiveSupport::NumberHelper. Kept separate from
# the module-function stem because the runtime source checker matches methods
# by their short class name and cannot distinguish the two forms in one file.
# active_support_number_helper.rb requires this file from its tail, after it has opened
# ActiveSupport::NumberHelper; requiring it back while it is still loading is a circular
# require.
require_relative "active_support_number_helper" unless defined?(ActiveSupport::NumberHelper)

module ActiveSupport
  module NumberHelper
    def number_to_currency(number, options = {})
      ActiveSupport::NumberHelper.number_to_currency(number, options)
    end

    def number_to_delimited(number, options = {})
      ActiveSupport::NumberHelper.number_to_delimited(number, options)
    end

    def number_to_human(number, options = {})
      ActiveSupport::NumberHelper.number_to_human(number, options)
    end

    def number_to_human_size(number, options = {})
      ActiveSupport::NumberHelper.number_to_human_size(number, options)
    end

    def number_to_percentage(number, options = {})
      ActiveSupport::NumberHelper.number_to_percentage(number, options)
    end

    def number_to_phone(number, options = {})
      ActiveSupport::NumberHelper.number_to_phone(number, options)
    end

    def number_to_rounded(number, options = {})
      ActiveSupport::NumberHelper.number_to_rounded(number, options)
    end
  end
end
