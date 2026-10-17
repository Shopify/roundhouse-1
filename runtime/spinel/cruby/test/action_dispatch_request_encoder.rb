# frozen_string_literal: true

# ActionDispatch::RequestEncoder and ActionDispatch::IntegrationTest.
# register_encoder (actionpack 8.2.0.alpha, lib/action_dispatch/testing/
# request_encoder.rb and integration.rb), for the PostgreSQL CRuby test
# helper. A test registers an encoder for a MIME type
# (`ActionDispatch::IntegrationTest.register_encoder(:graphql,
# param_encoder: ->(params) { params.to_s }, response_parser: ->(body)
# { JSON.parse(body) })`); a request made `as: :graphql` then carries the
# encoded params with the type's Content-Type and Accept headers, and the
# response is parsed by the registered parser.
#
# The harness's own `as: :json` handling is left as it is (it hands the
# params to the controller as the parse Rails does on the way in).

require_relative "mime"

module ActionDispatch
  class RequestEncoder
    class IdentityEncoder
      def content_type
      end

      def accept_header
      end

      def encode_params(params)
        params
      end

      def response_parser
        -> body { body }
      end
    end

    @encoders = { identity: IdentityEncoder.new }

    attr_reader :response_parser

    def initialize(mime_name, param_encoder, response_parser)
      @mime = Mime[mime_name]

      unless @mime
        raise ArgumentError, "Can't register a request encoder for unregistered MIME Type: #{mime_name}. See `Mime::Type.register`."
      end

      @response_parser = response_parser || -> body { body }
      @param_encoder   = param_encoder   || :"to_#{@mime.symbol}".to_proc
    end

    def content_type
      @mime.to_s
    end

    def accept_header
      @mime.to_s
    end

    def encode_params(params)
      @param_encoder.call(params) if params
    end

    def self.parser(content_type)
      mime = Mime::Type.lookup(content_type)
      encoder(mime ? (mime.symbol || mime.to_s) : nil).response_parser
    end

    def self.encoder(name)
      @encoders[name] || @encoders[:identity]
    end

    def self.registered(name)
      @encoders[name] unless name == :identity
    end

    def self.register_encoder(mime_name, param_encoder: nil, response_parser: nil)
      @encoders[mime_name] = new(mime_name, param_encoder, response_parser)
    end
  end

  class IntegrationTest
    def self.register_encoder(*args, **options)
      RequestEncoder.register_encoder(*args, **options)
    end
  end

  # A request `as:` a registered type: the params encoded, the type's
  # Content-Type and Accept headers set unless the test gave its own, as
  # Integration::Session#process does.
  module RequestEncoding
    def dispatch_request(method, path, params, headers = {}, as = nil)
      encoder = as.nil? || as == :json ? nil : RequestEncoder.registered(as)
      return super if encoder.nil?

      headers = headers.dup
      content_type = encoder.content_type
      accept = encoder.accept_header
      headers["CONTENT_TYPE"] ||= content_type if content_type
      headers["HTTP_ACCEPT"] ||= accept if accept
      super(method, path, encoder.encode_params(params), headers, nil)
    end
  end

  # The response body is parsed by the encoder registered for its media
  # type; any other keeps the harness's parse.
  module ResponseParsing
    def parsed_body
      media_type = content_type.to_s.split(";", 2).first.to_s.strip
      mime = Mime::Type.valid?(media_type) ? Mime::Type.lookup(media_type) : nil
      encoder = mime.nil? ? nil : RequestEncoder.registered(mime.symbol)
      encoder ? encoder.response_parser.call(body) : super
    end
  end
end

RequestDispatch.prepend(ActionDispatch::RequestEncoding)
ActionResponse.prepend(ActionDispatch::ResponseParsing)
