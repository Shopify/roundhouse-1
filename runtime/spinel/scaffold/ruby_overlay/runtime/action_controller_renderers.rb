# `ActionController::Renderers` (actionpack 8.x
# action_controller/metal/renderers.rb): the registry behind
# `render json: obj` / `render xml:` / `render js:`, and the hooks gems
# override (active_model_serializers 0.9's ActionController::Serialization
# `include`s it and redefines `_render_with_renderer_json`, calling
# `super`).
#
# `RENDERERS` is the live set of registered names; `add`/`remove` define
# and drop the `_render_with_renderer_<name>` instance method on this
# module, so a class that includes it (or a module that includes it and
# is included in turn) reaches them through the ancestor chain, and an
# override above them can `super` into the stock one.
# `_render_to_body_with_renderer(options)` runs the first renderer in
# the class's `_renderers` whose key `options` carries.
#
# Rails builds this on ActiveSupport::Concern and gives the controller
# its `_renderers` through a class_attribute. This is a plain module with
# an `included` hook, so it loads on trees that carry neither, and a
# gem's plain module may `include` it before any class does (the module
# gets no ClassMethods then; the class that finally includes it
# inherits `_renderers` from Base, which carries them all).
#
# The runtime's `render(body, ...)` is the lowered form of an inline
# `render json:`; a controller reaching a renderer through the options
# hash calls `render_to_body(options)`.
require "set"

module ActionController
  module Renderers
    # Every registered renderer's name; Base's default `_renderers`.
    RENDERERS = Set.new

    module ClassMethods
      # The renderers this class answers; a subclass inherits its
      # parent's until it calls `use_renderers`.
      def _renderers
        return @_renderers if defined?(@_renderers)
        me = self #: untyped
        parent = me.superclass #: untyped
        if parent.respond_to?(:_renderers)
          parent._renderers
        else
          ActionController::Renderers::RENDERERS
        end
      end

      def _renderers=(value)
        @_renderers = value
      end

      def use_renderers(*args)
        renderers = _renderers + args #: Set[Symbol]
        self._renderers = renderers.freeze
      end
      alias use_renderer use_renderers
    end

    def self.included(base)
      base.extend(ClassMethods) if base.is_a?(Class)
      super
    end

    # Metal-level default, `super`'s end of the chain.
    def render_to_body(options = {})
      _render_to_body_with_renderer(options) || super
    end

    def _render_to_body_with_renderer(options)
      ctl = self.class #: untyped
      renderers = (ctl.respond_to?(:_renderers) ? ctl._renderers : RENDERERS) #: Set[Symbol]
      renderers.each do |name|
        if options.key?(name)
          _process_options(options)
          method_name = Renderers._render_with_renderer_method_name(name)
          return send(method_name, options.delete(name), options)
        end
      end
      nil
    end

    # ActionController::Rendering#_process_options: the response
    # attributes the options hash sets before a renderer runs.
    def _process_options(options)
      status = options[:status]
      content_type = options[:content_type]
      location = options[:location]
      @status = resolve_status(status) if status
      self.content_type = content_type if content_type
      @location = ActionController.sanitize_location(location) if location
      nil
    end

    def self._render_with_renderer_method_name(key)
      "_render_with_renderer_#{key}"
    end

    # Adds a renderer: `key` becomes an option `render` accepts, the
    # block runs on the controller with the option's value and the
    # remaining options.
    def self.add(key, &block)
      define_method(_render_with_renderer_method_name(key), &block)
      RENDERERS << key.to_sym
    end

    def self.remove(key)
      RENDERERS.delete(key.to_sym)
      method_name = _render_with_renderer_method_name(key)
      remove_method(method_name)
    end

    # Rails mixes this into Base and API: `_renderers` is every
    # registered renderer.
    module All
      def self.included(base)
        base.send(:include, Renderers)
        base._renderers = RENDERERS if base.is_a?(Class)
        super
      end
    end

    # `self.content_type = X if media_type.nil?`: nothing set the
    # response's type yet. This runtime's `media_type` is a String and its
    # default `text/html` is a placeholder until a writer marks it.
    def _renderer_media_type_unset?
      !@content_type_explicit
    end

    add :json do |json, options|
      json = json.to_json(options) unless json.is_a?(String)

      callback = options[:callback]
      if callback && !callback.to_s.strip.empty?
        if _renderer_media_type_unset? || media_type == Mime[:json].to_s
          self.content_type = Mime[:js].to_s
        end
        "/**/#{callback}(#{json})"
      else
        self.content_type = Mime[:json].to_s if _renderer_media_type_unset?
        json
      end
    end

    add :js do |js, options|
      self.content_type = Mime[:js].to_s if _renderer_media_type_unset?
      js.respond_to?(:to_js) ? js.to_js(options) : js
    end

    add :xml do |xml, options|
      self.content_type = Mime[:xml].to_s if _renderer_media_type_unset?
      xml.respond_to?(:to_xml) ? xml.to_xml(options) : xml
    end
  end

  class Metal
    # AbstractController::Rendering#render_to_body: no body.
    def render_to_body(_options = {})
      nil
    end
  end

  class Base
    include Renderers::All
  end
end
