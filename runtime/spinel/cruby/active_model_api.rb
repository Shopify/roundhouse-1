# frozen_string_literal: true

# ActiveModel::API, ActiveModel::Access and ActiveModel::Model, ported
# from the activemodel native Core runs (Shopify/rails 5813559f4aed,
# 8.2.0.alpha: lib/active_model/api.rb, access.rb and model.rb) for the
# PostgreSQL CRuby tree, carried where app code (a gem compiled as source
# among it) names ActiveModel::API or ActiveModel::Model. Core's tableless
# classes (Payments' chargeback-response SerializableContext, the
# trust_battery_client gem's SignalProvenance) include Model for its
# attribute-hash constructor, validations, naming and conversion.
#
# Each method follows the native one, over the ports this tree carries:
# ActiveModel::AttributeAssignment (runtime/active_record_core.rb),
# Validations and Translation (runtime/active_model_validations.rb), and
# Naming and Conversion (runtime/active_model_naming.rb).
#
# Differences, at the edges:
# - ActiveSupport::Concern is not used: API's dependency on Validations
#   and Conversion (each a Concern natively) and its `included do extend
#   ActiveModel::Naming; extend ActiveModel::Translation end` are written
#   out as Concern#append_features runs them: the dependencies are
#   included into the class first, then API itself, then the `included`
#   block, once, and not again for a class that already has the module.
#   An includer that is a module (an app concern) defers to its own
#   includers, as a Concern does.
# - Access#slice's `index_with(...).with_indifferent_access` is the
#   hash built directly, made indifferent where the tree has
#   HashWithIndifferentAccess.

require_relative "active_model_naming"
require_relative "active_model_validations"

module ActiveModel
  module API
    include ActiveModel::AttributeAssignment

    def self.append_features(base)
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      base.include(ActiveModel::Validations)
      base.include(ActiveModel::Conversion)
      super
      base.class_eval do
        extend ActiveModel::Naming
        extend ActiveModel::Translation
      end
    end

    # Initializes a new model with the given +params+.
    def initialize(attributes = {})
      assign_attributes(attributes) if attributes

      super()
    end

    # Indicates if the model is persisted. Default is +false+.
    def persisted?
      false
    end
  end

  module Access
    def slice(*methods)
      hash = methods.flatten.to_h { |method| [method, public_send(method)] }
      hash.respond_to?(:with_indifferent_access) ? hash.with_indifferent_access : hash
    end

    def values_at(*methods)
      methods.flatten.map! { |method| public_send(method) }
    end
  end

  module Model
    def self.append_features(base)
      unless base.is_a?(Class)
        base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
        return super unless base.instance_variable_defined?(:@_dependencies)
        base.instance_variable_get(:@_dependencies) << self
        return false
      end
      return false if base < self
      base.include(ActiveModel::API)
      base.include(ActiveModel::Access)
      super
    end
  end
end
