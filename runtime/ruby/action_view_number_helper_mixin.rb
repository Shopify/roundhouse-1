# Instance-method facade of ActionView::Helpers::NumberHelper, separated
# from its module-function implementation for runtime source typing.
# action_view_number_helper.rb requires this file from its tail, after it has opened
# ActionView::Helpers::NumberHelper; requiring it back while it is still loading is a circular
# require.
require_relative "action_view_number_helper" unless defined?(ActionView::Helpers::NumberHelper)

module ActionView
  module Helpers
    module NumberHelper
      def number_to_currency(number, options = {})
        NumberHelper.number_to_currency(number, options)
      end

      def number_to_human(number, options = {})
        NumberHelper.number_to_human(number, options)
      end

      def number_to_human_size(number, options = {})
        NumberHelper.number_to_human_size(number, options)
      end

      def number_to_percentage(number, options = {})
        NumberHelper.number_to_percentage(number, options)
      end

      def number_to_phone(number, options = {})
        NumberHelper.number_to_phone(number, options)
      end

      def number_with_delimiter(number, options = {})
        NumberHelper.number_with_delimiter(number, options)
      end

      def number_with_precision(number, options = {})
        NumberHelper.number_with_precision(number, options)
      end
    end
  end
end
