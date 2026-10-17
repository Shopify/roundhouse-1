# frozen_string_literal: true
# `respond_to` (actionpack `ActionController::MimeResponds`) and
# `respond_with` (responders 3.1, `ActionController::RespondWith` and
# `ActionController::Responder`): the controller calls Core's REST admin
# API actions end in — `respond_with(risks, root: "risks")`,
# `respond_with(risk, location: path, root: "risk", status: status)`,
# `respond_with(resource) { |format| format.json { ... } }`.
#
# `respond_to { |format| format.json { ... } }` runs the block registered
# for the request's format (`format.any` as the fallback), or raises
# `UnknownFormat`; the lowering flattens it inside an action, and this is
# what a helper that is not an action (Core's `render_error`) reaches.
# With types and no block (`respond_to :json`) a request in one of them
# falls through to the default render, as the instance form does.
#
# `respond_with`, written out, each rule following the gem:
# - `respond_with(*resources, **options)`: the options are the last
#   resource when more than one is given; a lone Hash is the resource.
# - The format is the request's. One neither declared by the block's
#   collector nor served by a registered renderer raises `UnknownFormat`
#   (Rails answers 406/404 through its `rescue_from`).
# - `Responder#to_format`: a non-GET request whose resource has errors
#   renders them with 422 (`{ errors: errors }` for json); a block
#   registered for the format answers the request; otherwise
#   `api_behavior`: GET renders the resource, POST renders it with 201
#   and the `location:`, PUT/PATCH/DELETE answer 204.
# - `display` is `render(options.merge(format => resource))`: the
#   caller's `root:`, `status:` and `location:` reach the renderer, which
#   is `resource.to_json(options)` for json (`ActionController::
#   Renderers`), so a resource's `as_json(root:)` wraps its payload.
#
# DIVERGENCES from the gem, stated:
# - The class-level `respond_to :json, only: [...]` declaration is not
#   replayed (the lowering drops class-body macros it does not model), so
#   every action may respond in any format that has a renderer.
# - A POST without an explicit `location:` raises `NotImplementedError`:
#   the gem derives it from `polymorphic_url(resources)`, whose route
#   lookup is not carried here. Core passes `location:`.
# - Only `respond_with`'s default responder: `responder:` is refused.
#
# Dynamic Ruby (`send`, a method_missing collector), so ruby lane only.
module ActionController
  module RespondWith
    # `ActionController::MimeResponds::Collector` as `respond_with`
    # builds it: the blocks `format.json { ... }` registers by format.
    class Collector
      attr_reader :responses

      def initialize
        @responses = {}
      end

      def any(*formats, &block)
        formats = [:any] if formats.empty?
        formats.each { |format| @responses[format.to_sym] = block }
        nil
      end
      alias_method :all, :any

      def custom(format, &block)
        @responses[format.to_sym] = block
        nil
      end

      def method_missing(name, *args, &block)
        return super unless block_given? || args.empty?
        custom(name, &block)
      end

      def respond_to_missing?(_name, _include_private = false)
        true
      end

      # The block for `format`, falling back to `format.any`.
      def response_for(format)
        return @responses[format] if @responses.key?(format)
        @responses[:any]
      end

      def declares?(format)
        @responses.key?(format) || @responses.key?(:any)
      end
    end

    def respond_to(*mimes, &block)
      if !mimes.empty? && block
        raise ArgumentError, "respond_to takes either types or a block, never both"
      end
      collector = Collector.new
      mimes.each { |mime| collector.custom(mime, &nil) }
      block.call(collector) if block
      format = (request_format || :html).to_sym
      unless collector.declares?(format)
        raise ActionController::UnknownFormat
      end
      media_type = defined?(::Mime) ? ::Mime[format] : nil
      self.content_type = media_type.to_s if media_type && _renderer_media_type_unset?
      handler = collector.response_for(format)
      handler.call if handler
      nil
    end

    def respond_with(*resources, &block)
      if resources.size > 1 && resources.last.is_a?(Hash)
        options = resources.pop.dup
      else
        options = {}
      end
      if options.key?(:responder)
        raise NotImplementedError, "roundhouse: respond_with(responder:) is not supported"
      end
      resource = resources.last

      collector = Collector.new
      block.call(collector) if block
      format = (request_format || :html).to_sym
      handler = collector.response_for(format)
      renderer = Renderers._render_with_renderer_method_name(format)
      unless collector.declares?(format) || respond_to?(renderer, true)
        raise ActionController::UnknownFormat
      end

      if request_method != "GET" && !handler && _responder_has_errors?(resource)
        return _responder_display_errors(format, resource)
      end
      return handler.call(options) if handler
      raise ActionController::UnknownFormat unless respond_to?(renderer, true)

      case request_method
      when "GET"
        _responder_display(format, resource, {}, options)
      when "POST"
        location = options[:location]
        if location.nil?
          raise NotImplementedError,
            "roundhouse: respond_with on a POST needs an explicit location: " \
            "(the gem derives it from polymorphic_url(resources))"
        end
        _responder_display(format, resource, { status: :created, location: location }, options)
      else
        head(:no_content)
      end
    end

    private

    def _responder_has_errors?(resource)
      resource.respond_to?(:errors) && !resource.errors.empty?
    end

    def _responder_display_errors(format, resource)
      errors = resource.errors
      payload = format == :json ? { errors: errors } : errors
      _responder_render(format, payload, { status: :unprocessable_entity })
    end

    # `display(resource, given_options)`: the caller's options win over
    # the verb's defaults.
    def _responder_display(format, resource, given, options)
      render_options = given.merge(options)
      render_options.delete(:default_response)
      _responder_render(format, resource, render_options)
    end

    # `render format => payload, **options` through the registered
    # renderer, then the runtime's `render` for the response state.
    def _responder_render(format, payload, render_options)
      status = render_options[:status] || :ok
      location = render_options[:location]
      _process_options(render_options)
      body = send(Renderers._render_with_renderer_method_name(format), payload, render_options)
      render(body, status: status, location: location)
    end
  end

  class Base
    include RespondWith
  end
end
