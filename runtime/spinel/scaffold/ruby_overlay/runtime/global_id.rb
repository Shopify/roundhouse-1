# globalid 1.4.0, ported for the CRuby tree: `GlobalID` as a value
# (lib/global_id/global_id.rb), `GlobalID::Identification`
# (identification.rb) and `URI::GID` (uri/gid.rb).
#
# The gem itself cannot load here: it requires `active_support` (its
# gemspec depends on activesupport >= 6.1), and Rails' own code does not
# run over this tree's ports. Shopify Core reaches it at boot: its
# essentials component reopens `T::Enum` with
# `include GlobalID::Identification` (t_enum_global_id.rb), and
# observe.rb lists `GlobalID` and `GlobalID::Identification` among the
# types it serializes.
#
# The mint the other lanes share (`GlobalID.param`/`.uri`,
# runtime/ruby/rails.rb) and campfire's `GlobalID::Locator.locate(param,
# only:)` (runtime/global_id_locator.rb) stay as they are; this file
# reopens the same class.
#
# NOT PORTED, and named where reached: the gem's Locator
# (`GlobalID#find`, `GlobalID.find`, the locator-dispatched
# `model_class`), `SignedGlobalID` and `GlobalID::Verifier`. The
# Railtie's `ActiveSupport.on_load(:active_record) { include
# GlobalID::Identification }` is runtime/active_record_bang.rb's, which
# loads after ActiveRecord::Base. `GlobalID.app` defaults to
# `Rails.application.global_id_app`, where the Railtie would use the
# application's railtie name.
#
# Rails-isms the gem leans on are spelled out: `delegate` as methods,
# `try(:find)` as a nil check, `options.except` as a reject; `blank?`
# and `with_indifferent_access` are runtime/active_support_core_ext.rb's.
require "uri"
require "uri/generic"
require "cgi"
require "base64"

class GlobalID
  class << self
    def app
      @app ||= (Rails.application.global_id_app if defined?(Rails) && Rails.respond_to?(:application) && Rails.application)
    end

    def create(model, options = {})
      if app = options.fetch(:app) { GlobalID.app }
        params = options.reject { |key, _| %i[app verifier for].include?(key) }
        new URI::GID.create(app, model, params), options
      else
        raise ArgumentError, "An app is required to create a GlobalID. " \
          "Pass the :app option or set the default GlobalID.app."
      end
    end

    def find(gid, options = {})
      found = parse(gid, options)
      found && found.find(options)
    end

    def parse(gid, options = {})
      gid.is_a?(self) ? gid : new(gid, options)
    rescue URI::Error
      parse_encoded_gid(gid, options)
    end

    def app=(app)
      @app = URI::GID.validate_app(app)
    end

    private

    def parse_encoded_gid(gid, options)
      new(Base64.urlsafe_decode64(gid), options) rescue nil
    end
  end

  attr_reader :uri

  def initialize(gid, options = {})
    @uri = gid.is_a?(URI::GID) ? gid : URI::GID.parse(gid)
  end

  def app = uri.app
  def model_name = uri.model_name
  def model_id = uri.model_id
  def params = uri.params
  def to_s = uri.to_s
  def deconstruct_keys(keys) = uri.deconstruct_keys(keys)

  def find(options = {})
    raise NotImplementedError, "roundhouse: GlobalID#find (globalid 1.4.0 GlobalID::Locator.locate) is not ported; #{to_s}"
  end

  def model_class
    raise NotImplementedError, "roundhouse: GlobalID#model_class (globalid 1.4.0 GlobalID::Locator.locator_for) is not ported; #{to_s}"
  end

  def ==(other)
    other.is_a?(GlobalID) && @uri == other.uri
  end
  alias_method :eql?, :==

  def hash
    self.class.hash | @uri.hash
  end

  def to_param
    Base64.urlsafe_encode64(to_s, padding: false)
  end

  def as_json(*)
    to_s
  end

  module Identification
    def to_global_id(options = {})
      GlobalID.create(self, options)
    end
    alias to_gid to_global_id

    def to_gid_param(options = {})
      to_global_id(options).to_param
    end

    def to_signed_global_id(options = {})
      SignedGlobalID.create(self, options)
    end
    alias to_sgid to_signed_global_id

    def to_sgid_param(options = {})
      to_signed_global_id(options).to_param
    end
  end
end

module URI
  class GID < Generic
    alias :app :host
    attr_reader :model_name, :model_id, :params

    class MissingModelIdError < URI::InvalidComponentError; end
    class InvalidModelIdError < URI::InvalidComponentError; end

    COMPOSITE_MODEL_ID_MAX_SIZE = 20
    COMPOSITE_MODEL_ID_DELIMITER = "/"

    URI_PARSER = URI::RFC3986_Parser.new # :nodoc:

    class << self
      def validate_app(app)
        parse("gid://#{app}/Model/1").app
      rescue URI::Error
        raise ArgumentError, "Invalid app name. " \
          "App names must be valid URI hostnames: alphanumeric, hyphen and underscore characters only."
      end

      def parse(uri)
        generic_components = URI.split(uri) << URI_PARSER << true # RFC3986 parser, true arg_check
        new(*generic_components)
      end

      def create(app, model, params = nil)
        build app: app, model_name: model.class.name, model_id: model.id, params: params
      end

      def build(args)
        parts = Util.make_components_hash(self, args)
        parts[:host] = parts[:app]
        model_id_segment = Array(parts[:model_id]).map { |p| CGI.escape(p.to_s) }.join(COMPOSITE_MODEL_ID_DELIMITER)
        parts[:path] = "/#{parts[:model_name]}/#{model_id_segment}"

        if parts[:params] && !parts[:params].empty?
          parts[:query] = URI.encode_www_form(parts[:params])
        end

        super parts
      end
    end

    def to_s
      "gid://#{app}#{path}#{'?' + query if query}"
    end

    def deconstruct_keys(_keys)
      { app: app, model_name: model_name, model_id: model_id, params: params }
    end

    protected

    def set_path(path)
      set_model_components(path) unless defined?(@model_name) && @model_id
      super
    end

    def query=(query)
      set_params parse_query_params(query)
      super
    end

    def set_query(query)
      set_params parse_query_params(query)
      super
    end

    def set_params(params)
      @params = params
    end

    private

    COMPONENT = [:scheme, :app, :model_name, :model_id, :params].freeze

    def check_host(host)
      validate_component(host)
      super
    end

    def check_path(path)
      validate_component(path)
      set_model_components(path, true)
    end

    def check_scheme(scheme)
      if scheme == "gid"
        true
      else
        raise URI::BadURIError, "Not a gid:// URI scheme: #{inspect}"
      end
    end

    def set_model_components(path, validate = false)
      _, model_name, model_id = path.split("/", 3)

      validate_component(model_name) && validate_model_id_section(model_id, model_name) if validate
      @model_name = model_name

      if model_id
        model_id_parts = model_id
          .split(COMPOSITE_MODEL_ID_DELIMITER, COMPOSITE_MODEL_ID_MAX_SIZE)
          .reject(&:blank?)

        model_id_parts.map! do |id|
          validate_model_id(id)
          CGI.unescape(id)
        end

        @model_id = model_id_parts.length == 1 ? model_id_parts.first : model_id_parts
      end
    end

    def validate_component(component)
      return component unless component.blank?

      raise URI::InvalidComponentError,
        "Expected a URI like gid://app/Person/1234: #{inspect}"
    end

    def validate_model_id_section(model_id, model_name)
      return model_id unless model_id.blank?

      raise MissingModelIdError, "Unable to create a Global ID for " \
        "#{model_name} without a model id."
    end

    def validate_model_id(model_id_part)
      return unless model_id_part.include?("/")

      raise InvalidModelIdError, "Unable to create a Global ID for " \
        "#{model_name} with a malformed model id."
    end

    def parse_query_params(query)
      Hash[URI.decode_www_form(query)].with_indifferent_access if query
    end
  end

  register_scheme("GID", GID)
end
