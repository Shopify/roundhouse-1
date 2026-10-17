# frozen_string_literal: true
# `params[:key]` and `params["key"]` read the same value in Rails. The
# integration harness (and the dispatchers) assign the request parameters
# as a plain String-keyed Hash, and a controller method the lowering does
# not rewrite to string keys (`self.params[:uniqueId]` in a helper, as in
# Core's PartnerReferralController#token_payload) read nil from it.
# A plain Hash assigned as the params is held with indifferent access,
# as `ActionController::TestCase#process` already builds it.
#
# Dynamic Ruby (`with_indifferent_access`), so ruby lane only.
module ActionController
  module IndifferentParamsWriter
    def params=(value)
      value = value.with_indifferent_access if value.instance_of?(Hash)
      super(value)
    end
  end

  class Base
    prepend IndifferentParamsWriter
  end
end
