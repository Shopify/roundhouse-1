# typed: false
module T
  def self.untyped = Types::Untyped.new

  module Types
    class Untyped
      def error_message_for_obj(_obj) = nil
    end

    class Simple
      def initialize(raw_type) = @raw_type = raw_type
      def error_message_for_obj(obj) = obj.is_a?(@raw_type) ? nil : "expected #{@raw_type}, got #{obj.class}"
    end
  end

  module Private
    module Types
      module Void
        VOID = Object.new
      end
    end

    module Methods
      module Modes
        def self.abstract = :abstract
        def self.standard = :standard
      end

      Signature = Struct.new(:method, :method_name, :mode, :bind, :arg_types, :kwarg_types, :block_type, :rest_type, :keyrest_type, :effective_return_type)

      # sorbet-runtime's DeclBuilder, for the declarations `abstract`, `params`, `returns` and `void`.
      class Declaration
        attr_reader :mode, :arg_types, :return_type

        def initialize
          @mode = Modes.standard
          @arg_types = {}
          @return_type = nil
        end

        def abstract = tap { @mode = Modes.abstract }
        def params(**types) = tap { @arg_types = types }
        def returns(type) = tap { @return_type = type.is_a?(Class) ? T::Types::Simple.new(type) : type }
        def void = tap { @return_type = Void::VOID_TYPE }
      end
      Void = T::Private::Types::Void
      Void::VOID_TYPE = Class.new { def error_message_for_obj(_obj) = nil }.new

      @signatures = {}
      @pending = {}

      class << self
        def signature_for_method(method)
          @signatures[[method.owner, method.name]]
        end

        def declare(mod, block) = @pending[mod] = block

        def _on_method_added(mod, name)
          block = @pending.delete(mod) or return
          declaration = Declaration.new
          declaration.instance_exec(&block)
          original = mod.instance_method(name)
          signature = Signature.new(original, name, declaration.mode, nil, declaration.arg_types.to_a, {}, nil, nil, nil, declaration.return_type)
          @signatures[[mod, name]] = signature
          wrap(mod, name, original, signature)
        end

        def wrap(mod, name, original, signature)
          Thread.current[:sorbet_reproduction_wrapping] = true
          if signature.mode == Modes.abstract
            CallValidation.create_abstract_wrapper(mod, name)
          else
            CallValidation.create_validator_method(mod, original, signature)
          end
        ensure
          Thread.current[:sorbet_reproduction_wrapping] = false
        end
      end

      module CallValidation
        def self.create_abstract_wrapper(mod, method_name)
          mod.module_eval(<<~METHOD, __FILE__, __LINE__ + 1)
            def #{method_name}(...)
              if defined?(super)
                super
              else
                raise NotImplementedError.new("The method `#{method_name}` on #{mod} is declared as `abstract`. It does not have an implementation.")
              end
            end
          METHOD
        end

        def self.create_validator_method(mod, original, signature)
          mod.send(:define_method, signature.method_name) do |*args, &blk|
            value = original.bind_call(self, *args, &blk)
            message = signature.effective_return_type.error_message_for_obj(value)
            raise TypeError, message if message

            signature.effective_return_type.equal?(Void::VOID_TYPE) ? Void::VOID : value
          end
        end
      end
    end
  end

  module Sig
    def self.extended(base)
      base.singleton_class.prepend(Hook)
    end

    module Hook
      def method_added(name)
        super
        T::Private::Methods._on_method_added(self, name) unless Thread.current[:sorbet_reproduction_wrapping]
      end

      def singleton_method_added(name)
        super
      end
    end

    def sig(&block) = T::Private::Methods.declare(self, block)
  end

  module Helpers
    def abstract! = nil
  end
end
