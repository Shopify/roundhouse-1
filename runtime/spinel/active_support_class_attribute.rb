# frozen_string_literal: true

# Class#class_attribute over ActiveSupport::ClassAttribute (Shopify/rails
# 5813559f4aed, 8.2.0.alpha activesupport/lib/active_support/class_attribute.rb
# and core_ext/class/attribute.rb, in that order), verbatim; Rails' doc
# comments dropped, and the two files' requires are the redefine_method
# port's. `require "active_support/core_ext/class/attribute"` (or
# core_ext/class) from a native gem (activerecord-rescue_from_duplicate)
# loads it (runtime/features, `project::FRAMEWORK_FEATURE_PORTS`), as
# Rails has before any gem natively. The app's own class_attribute calls
# are lowered at ingest.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "active_support_redefine_method"

module ActiveSupport
  module ClassAttribute # :nodoc:
    class << self
      def redefine(owner, name, owner_method, reader_method, value, instance_reader)
        ivar_name = :"@#{reader_method}"
        owner.instance_variable_set(ivar_name, value)

        owner_proc =
          if defined?(Ractor.shareable_proc)
            Ractor.shareable_proc { owner }
          else
            -> { owner }
          end

        if owner.singleton_class? && !owner.attached_object.is_a?(Module) && instance_reader
          owner.class_eval("def #{name}; self.singleton_class.#{reader_method}; end", __FILE__, __LINE__)
        end

        redefine_method(owner.singleton_class, owner_method, private: true, &owner_proc)
      end

      def redefine_method(owner, name, private: false, &block)
        owner.silence_redefinition_of_method(name)
        owner.define_method(name, &block)
        owner.send(:private, name) if private
      end
    end
  end
end

class Class
  def class_attribute(*attrs, instance_accessor: true,
    instance_reader: instance_accessor, instance_writer: instance_accessor, instance_predicate: true, default: nil
  )
    class_methods, methods = [], []
    attrs.each do |name|
      unless name.is_a?(Symbol) || name.is_a?(String)
        raise TypeError, "#{name.inspect} is not a symbol nor a string"
      end

      name = name.to_sym
      reader_method = :"__class_attr_#{name}"
      owner_method = :"__class_attr_#{name}_owner"
      ::ActiveSupport::ClassAttribute.redefine(self, name, owner_method, reader_method, default, instance_reader)

      singleton_class.attr_reader(reader_method)

      class_methods << "def #{name}; #{owner_method}.#{reader_method}; end"
      class_methods << <<~RUBY
        def #{name}=(value)
          if #{owner_method}.equal?(self)
            @#{reader_method} = value
          else
            ::ActiveSupport::ClassAttribute.redefine(self, :#{name}, :#{owner_method}, :#{reader_method}, value, #{!!instance_reader})
          end
        end
      RUBY

      if singleton_class?
        methods << <<~RUBY if instance_reader
          silence_redefinition_of_method(:#{name})
          def #{name}
            self.singleton_class.#{name}
          end
        RUBY
      else
        methods << <<~RUBY if instance_reader
          silence_redefinition_of_method def #{name}
            if defined?(@#{name})
              @#{name}
            else
              self.class.#{name}
            end
          end
        RUBY
      end

      methods << <<~RUBY if instance_writer
        silence_redefinition_of_method(:#{name}=)
        attr_writer :#{name}
      RUBY

      if instance_predicate
        class_methods << "silence_redefinition_of_method def #{name}?; !!self.#{name}; end"
        if instance_reader
          methods << "silence_redefinition_of_method def #{name}?; !!self.#{name}; end"
        end
      end
    end

    location = caller_locations(1, 1).first
    class_eval(["class << self", *class_methods, "end", *methods].join(";").tr("\n", ";"), location.path, location.lineno)
  end
end
