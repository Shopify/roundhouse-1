# Runtime port of the database_documentation gem's model metadata
# (database_documentation 3.2.0, the version Core's Gemfile pins with
# "~> 3.0"): what a model body runs at class load.
#
# `include DatabaseDocumentation::ModelMetadata::Metadata` gives the
# class an inheritable `annotation` class attribute and the
# `database_documentation` / `annotate` / `metadata` class macros. Each
# only RECORDS: the block is called with an annotator whose `field`,
# `description` and `enable_cdc_extraction` build plain value objects,
# and the merged result lands in `annotation` (read at runtime by Core's
# privacy purge, `record.class.annotation.fields`). `database_documentation`
# also keeps a ModelAnnotation in `model_annotations`.
#
# The gem itself loads through zeitwerk and ActiveSupport (Concern,
# class_attribute, `present?`), so no emitted tree requires it; this port
# writes the same behavior in plain Ruby:
# - Concern's `included` + `class_methods` is `self.included` extending
#   `ClassMethods`;
# - `class_attribute :annotation, instance_accessor: false,
#   instance_predicate: false` is a class-side reader the writer redefines
#   on the class it is called on, so subclasses inherit and may override
#   it, as class_attribute's do.
#
# A Datatype here carries what the annotation path reads (`name`,
# `pii?`); the substring tables the gem's column-name auto-detection
# (Helpers::DatatypeResolver, its Dataplex sync) reads are not ported,
# nor is that pipeline. `Configuration#namespaces` without explicit
# namespaces would read ActiveRecord::Base.configurations, which the port
# refuses by raising; the namespaces' `connection_pool` is not ported.
require "set"

module DatabaseDocumentation
  class UsageError < StandardError; end
  class InvalidTable < StandardError; end
  class InvalidModel < StandardError; end
  class InvalidColumn < StandardError; end

  module Constants
    METADATA_PROJECT = "shopify-metadata"
    METADATA_LOCATION = "us"
  end

  class Configuration
    class MultipleNamespacesWithSameSchemaNameError < StandardError; end
    class ChangingApplicationNameError < StandardError; end
    class MissingApplicationNameError < StandardError; end

    class NamespaceConfiguration
      class InvalidSchemaNameError < StandardError; end

      attr_reader :namespace, :cdc_topic_format, :base_connection

      def initialize(namespace:, database_config_name: nil, schema_name: nil, cdc_topic_format: nil, base_connection: nil)
        @namespace = namespace
        @database_config_name = database_config_name
        @schema_name = schema_name
        @cdc_topic_format = cdc_topic_format
        @base_connection = base_connection
      end

      def database_config_name
        blank = @database_config_name.nil? || @database_config_name.to_s.strip.empty?
        blank ? namespace : @database_config_name
      end

      def schema_name
        raise InvalidSchemaNameError if @schema_name.nil? || @schema_name.to_s.strip.empty?

        @schema_name.to_s
      end

      def to_s
        "Namespace: #{namespace}, Schema Name: #{schema_name}, CDC Topic Format: #{cdc_topic_format}"
      end
    end

    attr_accessor :drop_non_pii_annotations
    attr_reader :google_project_id, :google_location, :default_privacy_schema

    def initialize(application_name = nil)
      @application_name = application_name.to_s if application_name
      @drop_non_pii_annotations = false
      @namespaces = {}
      @google_project_id = Constants::METADATA_PROJECT
      @google_location = Constants::METADATA_LOCATION
    end

    def apply(config_hash)
      config_hash.each do |key, value|
        public_send("#{key}=", value)
      end
    end

    def namespaces
      if @namespaces.none?
        raise NotImplementedError,
          "roundhouse: DatabaseDocumentation's default namespaces read ActiveRecord::Base.configurations, which the port does not model"
      end

      @namespaces.tap { |namespaces| validate_namespaces!(namespaces) }
    end

    def namespaces=(value)
      value ||= {}
      @namespaces = value.map do |namespace, namespace_config|
        namespace_config ||= {}
        namespace_config[:schema_name] ||= application_name
        [namespace, NamespaceConfiguration.new(namespace: namespace, **namespace_config)]
      end.to_h
      @namespaces.tap { |namespaces| validate_namespaces!(namespaces) }
    end

    def google_project_id=(value)
      @google_project_id = value&.gsub("_", "-") || Constants::METADATA_PROJECT
    end

    def google_location=(value)
      @google_location = value&.gsub("_", "-") || Constants::METADATA_LOCATION
    end

    def application_name
      unless @application_name
        raise MissingApplicationNameError,
          ":application_name must be explicitly set in the DatabaseDocumentation configuration. " \
            "See https://github.com/Shopify/database-documentation#application-name-required for setup instructions."
      end

      @application_name
    end

    def application_name=(value)
      raise ChangingApplicationNameError if @application_name

      string_value = value.to_s
      raise MissingApplicationNameError, ":application_name cannot be nil or empty." if string_value.empty?

      @application_name = string_value
    end

    def default_privacy_schema=(value)
      warn(":default_privacy_schema is deprecated. Set schema_name in namespace configuration instead if required.")
      @default_privacy_schema = value
    end

    private

    def validate_namespaces!(namespaces)
      schema_names = namespaces.values.map(&:schema_name)
      return if schema_names.uniq.size == schema_names.size

      raise MultipleNamespacesWithSameSchemaNameError,
        "Namespaces must have unique schema names\n#{namespaces.values.map(&:to_s).join("\n")}"
    end
  end

  class << self
    attr_writer :logger, :config

    def logger
      @logger ||= Logger.new($stdout)
    end

    def setup
      yield config unless @_ran_once
      @_ran_once = true
    end

    # The gem's Railtie initializer makes the default follow the Rails
    # environment (`drop_non_pii_annotations = Rails.env.production?`)
    # before any model loads; the port applies it when the config is built.
    def config
      @config ||= Configuration.new.tap do |config|
        config.drop_non_pii_annotations = ::Rails.env.production? if defined?(::Rails) && ::Rails.respond_to?(:env)
      end
    end
  end

  module Datatypes
    class Datatype
      attr_reader :name

      def initialize(name:, is_pii: false)
        @name = name
        @is_pii = is_pii
      end

      def pii?
        @is_pii
      end

      def matches(field_name)
        name if name == field_name.to_sym
      end
    end

    CookieIdentifier = Datatype.new(name: :COOKIE_IDENTIFIER, is_pii: true)
    Credentials = Datatype.new(name: :CREDENTIALS, is_pii: true)
    EmailAddress = Datatype.new(name: :EMAIL_ADDRESS, is_pii: true)
    FinancialAccount = Datatype.new(name: :FINANCIAL_ACCOUNT, is_pii: true)
    FinancialData = Datatype.new(name: :FINANCIAL_DATA)
    FulfillmentTracking = Datatype.new(name: :FULFILLMENT_TRACKING, is_pii: true)
    GeoLocationPrecise = Datatype.new(name: :GEO_LOCATION_PRECISE, is_pii: true)
    GovernmentIdentifier = Datatype.new(name: :GOVERNMENT_IDENTIFIER, is_pii: true)
    IdentifyingUrl = Datatype.new(name: :IDENTIFYING_URL, is_pii: true)
    IdentityUuid = Datatype.new(name: :IDENTITY_UUID)
    IpAddress = Datatype.new(name: :IP_ADDRESS, is_pii: true)
    MessageContents = Datatype.new(name: :MESSAGE_CONTENTS, is_pii: true)
    MonetaryValue = Datatype.new(name: :MONETARY_VALUE)
    NamePersonal = Datatype.new(name: :NAME_PERSONAL, is_pii: true)
    OtherData = Datatype.new(name: :OTHER_DATA)
    OtherIdentifier = Datatype.new(name: :OTHER_IDENTIFIER, is_pii: true)
    PartnerId = Datatype.new(name: :PARTNER_ID)
    PaymentCardDigits = Datatype.new(name: :PAYMENT_CARD_DIGITS, is_pii: true)
    PhoneNumber = Datatype.new(name: :PHONE_NUMBER, is_pii: true)
    PostalCode = Datatype.new(name: :POSTAL_CODE, is_pii: true)
    PublicAccountIdentifier = Datatype.new(name: :PUBLIC_ACCOUNT_IDENTIFIER, is_pii: true)
    ShopAccountUuid = Datatype.new(name: :SHOP_ACCOUNT_UUID)
    ShopId = Datatype.new(name: :SHOP_ID)
    ShopPermanentDomain = Datatype.new(name: :SHOP_PERMANENT_DOMAIN)
    StreetAddress = Datatype.new(name: :STREET_ADDRESS, is_pii: true)
    UserAgent = Datatype.new(name: :USER_AGENT, is_pii: true)
  end

  module ModelMetadata
    # `@descriptions.present?` on an Array is `!empty?`.
    class FieldAnnotation
      attr_reader :name, :datatypes, :descriptions

      def initialize(name, datatypes: [], description: [], is_pii: false)
        @name = name
        @datatypes = datatypes
        @descriptions = description.nil? ? [] : Array(description)
        @is_pii = is_pii
      end

      def description
        @descriptions.uniq.sort.join(" | ") unless @descriptions.empty?
      end

      def description=(new_description)
        @descriptions = Array(new_description)
      end

      def pii?
        @is_pii
      end

      def merge(other)
        return self if other.nil?
        return self if name != other.name

        FieldAnnotation.new(
          name,
          datatypes: (datatypes + other.datatypes).uniq,
          description: (descriptions + other.descriptions).uniq,
          is_pii: pii? || other.pii?,
        )
      end
    end

    class Annotation
      attr_reader :model_class, :descriptions
      attr_accessor :fields, :enable_cdc_extraction

      def initialize(model_class: nil, description: [], fields: [], enable_cdc_extraction: false)
        @model_class = model_class
        @descriptions = description.nil? ? [] : Array(description)
        @fields = fields.map { |field| [field.name.to_sym, field] }.to_h
        @enable_cdc_extraction = enable_cdc_extraction
      end

      def description
        @descriptions.uniq.sort.join(" | ") unless @descriptions.empty?
      end

      def description=(new_description)
        @descriptions = Array(new_description)
      end

      def model_name
        model_class.name
      end

      def cdc_extraction_enabled?
        enable_cdc_extraction
      end

      def add_field(field_annotation)
        fields[field_annotation.name.to_sym] = field_annotation
      end

      # The merged annotation carries no model_class, as the gem's does.
      def merge(other)
        return self if other.nil?

        merged_fields = fields.merge(other.fields) { |_key, field, other_field| field.merge(other_field) }
        Annotation.new(
          description: (@descriptions + other.descriptions).uniq,
          fields: merged_fields.values,
          enable_cdc_extraction: enable_cdc_extraction || other.enable_cdc_extraction,
        )
      end
    end

    class Annotator
      attr_reader :annotation

      def initialize(model_class: nil)
        @annotation = Annotation.new(model_class: model_class)
      end

      def description(description)
        @annotation.description = description
      end

      def enable_cdc_extraction(enabled = true)
        @annotation.enable_cdc_extraction = enabled
      end

      def field(field_name, description: nil, datatype: nil, is_pii: nil)
        datatypes = resolve_datatypes(Array(datatype), is_pii)
        is_pii = is_pii.nil? ? datatypes.any?(&:pii?) : is_pii

        return if DatabaseDocumentation.config.drop_non_pii_annotations && !is_pii

        @annotation.add_field(FieldAnnotation.new(field_name, datatypes: datatypes, description: description, is_pii: is_pii))
      end

      private

      def resolve_datatypes(datatypes, is_pii)
        unless datatypes.all? { |d| d.is_a?(Datatypes::Datatype) }
          raise ArgumentError, "datatype must be an instance or array of Datatypes::Datatype"
        end

        if is_pii.nil?
          datatypes
        elsif is_pii
          [Datatypes::OtherIdentifier]
        else
          [Datatypes::OtherData]
        end
      end
    end

    class ModelAnnotation
      attr_reader :table_name, :table_description, :fields, :model_class

      def initialize(table_name:, model_class: nil)
        @table_name = table_name
        @model_class = model_class
        @fields = {}
      end

      def description(description)
        @table_description = description
      end

      # A no-op in the gem too: the Annotator's interface.
      def enable_cdc_extraction(_enabled = true); end

      def field(field_name, description: nil, datatype: nil, is_pii: nil)
        resolved_datatype = determine_datatype(datatype, is_pii)
        resolved_datatype = resolved_datatype.uniq if resolved_datatype.is_a?(Array)

        return if DatabaseDocumentation.config.drop_non_pii_annotations

        # `is_pii.present?`: true is present, false and nil are not.
        @fields.merge!(
          field_name.to_sym => {
            description: description,
            datatype: resolved_datatype,
            is_pii: is_pii ? is_pii : Array(datatype).any?(&:pii?),
          },
        )
      end

      private

      def determine_datatype(datatype, is_pii)
        unless Array(datatype).all? { |d| d.is_a?(Datatypes::Datatype) }
          raise ArgumentError, "datatype must be an instance or array of Datatypes::Datatype"
        end

        return datatype if is_pii.nil?

        overridden = is_pii ? Datatypes::OtherIdentifier : Datatypes::OtherData
        return overridden if datatype.nil?

        if datatype.is_a?(Array)
          datatype.map { |type| type.pii? == is_pii ? type : overridden }
        else
          datatype.pii? == is_pii ? datatype : overridden
        end
      end
    end

    module Metadata
      class MissingSchema < StandardError; end
      class MissingModelAnnotation < StandardError; end

      # ActiveSupport::Concern's append_features: a class whose superclass
      # already includes the module runs neither the `included` block
      # (`class_attribute :annotation`) nor `class_methods`, so it keeps
      # the inherited annotation.
      def self.append_features(base)
        # A module includer is a concern natively (an app concern is lowered
        # without its extend): Concern#append_features defers to its includers.
        unless base.is_a?(Class)
          base.extend(ActiveSupport::Concern) if defined?(ActiveSupport::Concern) && !base.instance_variable_defined?(:@_dependencies)
          return super unless base.instance_variable_defined?(:@_dependencies)
          base.instance_variable_get(:@_dependencies) << self
          return false
        end
        return false if base < self

        super
        base.extend(ClassMethods)
      end

      module ClassMethods
        # class_attribute's reader, nil until the writer sets it.
        def annotation
          nil
        end

        # class_attribute's writer: redefines the reader on this class,
        # which its subclasses inherit until they write their own.
        def annotation=(value)
          define_singleton_method(:annotation) { value }
          value
        end

        def annotate(&block)
          annotator = Annotator.new(model_class: self)
          block.call(annotator)
          self.annotation = annotator.annotation.merge(annotation)
        end

        def model_annotations
          @database_documentation_annotations
        end

        def database_documentation(&block)
          @database_documentation_annotations = ModelAnnotation.new(table_name: table_name, model_class: name)
          block.call(@database_documentation_annotations)
          annotate(&block)
        end

        def metadata(&block)
          warn("metadata method is deprecated and will be removed from " \
            "DatabaseDocumentation::ModelMetadata::Metadata. Please replace the call to metadata with a call to the " \
            "database_documentation method.")
          database_documentation(&block)
        end
      end
    end
  end
end
