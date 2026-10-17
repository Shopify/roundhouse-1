# CRuby-only `ActionMailer::Rescuable` (actionmailer/lib/action_mailer/
# rescuable.rb) and the `AbstractController::Base` class surface a mailer
# class answers: a mailer class takes `rescue_from` handlers, and
# `handle_exception` answers an exception with the nearest matching one,
# re-raising it when none matches. `SendMailJob#handle_exception_with_
# mailer_class` asks the mailer class for it when a delivery fails, and
# `SendMailJob#validate_mailer_action!` asks it for its `action_methods`.
require "set"
require_relative "action_mailer"
# Only an app whose tree carries Rescuable (the emit leaves it out of one
# that rescues nothing) gets the handler registry.
rescuable = File.join(__dir__, "active_support_rescuable.rb")
require rescuable if File.exist?(rescuable)

module ActionMailer
  class Base
    include ActiveSupport::Rescuable if defined?(ActiveSupport::Rescuable)

    def self.handle_exception(exception)
      (respond_to?(:rescue_with_handler) && rescue_with_handler(exception)) || raise(exception)
    end

    def handle_exceptions
      yield
    rescue => exception
      (respond_to?(:rescue_with_handler, true) && rescue_with_handler(exception)) || raise
    end

    # AbstractController::Base: `abstract!` marks a class whose own public
    # methods are not actions. `action_methods` is the public instance
    # methods of the class and its ancestors, less those of the nearest
    # abstract ancestor and above (the framework's own), but for any a
    # concrete class between defines again.
    def self.abstract!
      @abstract = true
    end

    def self.abstract?
      !!(defined?(@abstract) && @abstract)
    end

    def self.action_methods
      @action_methods ||= begin
        methods = (public_instance_methods(true) - internal_methods + public_instance_methods(false))
        methods.map!(&:to_s)
        methods.to_set
      end
    end

    def self.internal_methods
      controller = self
      methods = []
      until controller.abstract?
        methods += controller.public_instance_methods(false)
        controller = controller.superclass
      end
      controller.public_instance_methods(true) - methods
    end
    private_class_method :internal_methods

    # `ActionMailer::Base.method_missing`: an action called on the class
    # (`Mailer.one_time_passcode(...)`, dispatched by name through
    # `public_send`) runs on a new instance, as the class-side wrappers
    # the lowering writes for the statically named calls do.
    def self.method_missing(name, *args, **kwargs, &block)
      return super unless action_methods.include?(name.to_s)
      new.public_send(name, *args, **kwargs, &block)
    end

    def self.respond_to_missing?(name, include_private = false)
      action_methods.include?(name.to_s) || super
    end

    abstract!
  end
end
