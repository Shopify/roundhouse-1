# frozen_string_literal: true

# ActiveRecord::NestedAttributes (activerecord nested_attributes.rb at
# Rails 5eb8c4c45bee) and the AutosaveAssociation pieces it relies on
# (autosave_association.rb: `mark_for_destruction`, `changed_for_autosave?`,
# the nested error import of `association_valid?`, `save_collection_association`,
# `save_has_one_association`, `save_belongs_to_association`), for the
# PostgreSQL CRuby tree, over active_record_core.rb's reflections and the
# association objects of active_record_associations.rb.
#
# `accepts_nested_attributes_for` is replayed from the model body after the
# compiled methods exist (`lower::model_to_library::macro_replay`). It
# declares `<association>_attributes=` in a module included in the model, so
# a model's own writer of that name still reaches Rails' through `super`,
# and makes the association autosave: the generated saves the compiler wrote
# for the association (`emit::ruby::collection_autosave`,
# `lower::model_to_library::associations`) are replaced by the autosave
# forms below, which also destroy the records marked for destruction and
# save the changed ones; where the compiler wrote none (an association whose
# reader keeps no target of its own) the same forms run as validate and
# save callbacks.
#
# Differences, at the edges:
# - The nested errors are `errors.import`ed under `<association>.<attribute>`
#   when ActiveModel::ErrorAttributes is carried; without it the message is
#   the association's humanized name before the child's lowercased message.
# - `index_errors` is not read: errors are never indexed (`lines[0].sku`).
# - A collection's records are destroyed one by one when saved, as
#   `association.destroy(record)` does for a persisted record, without
#   the association's `dependent:` handling or before/after_remove callbacks.
module ActiveRecord
  module NestedAttributes
    # Raised when more records are given than the `:limit` allows
    # (`TooManyRecords`).
    class TooManyRecords < ActiveRecordError
    end

    UNASSIGNABLE_KEYS = ["id", "_destroy"].freeze

    REJECT_ALL_BLANK_PROC = proc { |attributes| attributes.all? { |key, value| key == "_destroy" || value.blank? } }

    VALID_OPTIONS = [:allow_destroy, :reject_if, :limit, :update_only].freeze

    FALSE_VALUES = [false, 0, "0", :"0", "f", :f, "F", :F, "false", :false, "FALSE", :FALSE, "off", :off, "OFF", :OFF].freeze

    module ClassMethods
      # The options `accepts_nested_attributes_for` was given per
      # association name (`class_attribute :nested_attributes_options`).
      def nested_attributes_options
        return @nested_attributes_options if instance_variable_defined?(:@nested_attributes_options)
        parent = superclass
        parent.respond_to?(:nested_attributes_options) ? parent.nested_attributes_options : {}
      end

      def nested_attributes_options=(value)
        @nested_attributes_options = value
      end

      # nested_attributes.rb `accepts_nested_attributes_for`.
      def accepts_nested_attributes_for(*attr_names)
        options = { allow_destroy: false, update_only: false }
        given = attr_names.last.is_a?(Hash) ? attr_names.pop : {}
        given.each_key do |key|
          unless VALID_OPTIONS.include?(key.to_sym)
            raise ArgumentError, "Unknown key: #{key.inspect}. Valid keys are: #{VALID_OPTIONS.map(&:inspect).join(", ")}"
          end
        end
        options.update(given.transform_keys(&:to_sym))
        options[:reject_if] = REJECT_ALL_BLANK_PROC if options[:reject_if] == :all_blank

        attr_names.each do |association_name|
          reflection = reflect_on_association(association_name)
          if reflection.nil?
            raise ArgumentError, "No association found for name `#{association_name}'. Has it been defined yet?"
          end
          begin
            reflection.autosave = true
          rescue NotImplementedError
            # The compiler carried only some of the association's options.
            nil
          end
          name = association_name.to_sym
          all = nested_attributes_options.dup
          all[name] = options
          self.nested_attributes_options = all
          type = reflection.collection? ? :collection : :one_to_one
          _nested_attributes_module.send(:define_method, :"#{name}_attributes=") do |attributes|
            send(:"assign_nested_attributes_for_#{type}_association", name, attributes)
          end
          _nested_install_autosave(name, reflection)
        end
      end

      private
        # The module holding the generated writers (`generated_association_methods`).
        def _nested_attributes_module
          return @_nested_attributes_module if instance_variable_defined?(:@_nested_attributes_module)
          mod = Module.new
          include mod
          @_nested_attributes_module = mod
        end

        # Replaces the compiled instance method `name` by `block`, as
        # Rails' autosave callbacks are the methods named for the association.
        def _nested_replace(name, &block)
          remove_method(name) if method_defined?(name, false) || private_method_defined?(name, false)
          define_method(name, &block)
        end

        def _nested_generated?(name)
          method_defined?(name) || private_method_defined?(name)
        end

        # autosave_association.rb `add_autosave_association_callbacks` for
        # an association made autosave by the macro.
        def _nested_install_autosave(name, reflection)
          save_collection = :"_autosave_associated_records_for_#{name}"
          validate_collection = :"_validate_associated_records_for_#{name}"
          save_has_one = :"_save_has_one_association_#{name}"
          autosave_has_one = :"_autosave_#{name}"
          hook = _nested_attributes_module
          if reflection.collection?
            if _nested_generated?(save_collection)
              _nested_replace(save_collection) { |was_new| _nested_save_collection(name, was_new) }
            else
              hook.send(:define_method, :"_nested_after_save_#{name}") { _nested_save_collection(name, false) }
              after_save(:"_nested_after_save_#{name}")
            end
            if _nested_generated?(validate_collection)
              _nested_replace(validate_collection) { _nested_validate_collection(name) }
            elsif !_nested_generated?(save_collection) && (reflection.options[:validate] rescue nil) != false
              hook.send(:define_method, :"_nested_validate_#{name}") { _nested_validate_collection(name) }
              validate(:"_nested_validate_#{name}")
            end
          else
            hook.send(:define_method, :"_nested_validate_#{name}") { _nested_validate_single(name) }
            validate(:"_nested_validate_#{name}")
            if reflection.belongs_to?
              hook.send(:define_method, :"_nested_before_save_#{name}") { _nested_save_belongs_to(name) }
              before_save(:"_nested_before_save_#{name}")
            elsif _nested_generated?(autosave_has_one)
              _nested_replace(autosave_has_one) { _nested_save_has_one(name) }
            elsif _nested_generated?(save_has_one)
              _nested_replace(save_has_one) { _nested_save_has_one(name) }
            else
              hook.send(:define_method, :"_nested_after_save_#{name}") { _nested_save_has_one(name) }
              after_save(:"_nested_after_save_#{name}")
            end
          end
        end
    end

    # autosave_association.rb `mark_for_destruction`.
    def mark_for_destruction
      @marked_for_destruction = true
    end

    # autosave_association.rb `changed_for_autosave?`.
    def changed_for_autosave?
      new_record? || has_changes_to_save? || marked_for_destruction? || nested_records_changed_for_autosave?
    end

    # autosave_association.rb `nested_records_changed_for_autosave?`, over
    # the associations the macro made autosave.
    def nested_records_changed_for_autosave?
      return false if @_nested_records_changed_for_autosave_already_called
      begin
        @_nested_records_changed_for_autosave_already_called = true
        self.class.nested_attributes_options.each_key.any? do |name|
          next false unless instance_variable_get(:"@#{name}_loaded") == true
          target = instance_variable_get(:"@#{name}_cache")
          target.nil? ? false : Array(target).any? { |record| record.changed_for_autosave? }
        end
      ensure
        @_nested_records_changed_for_autosave_already_called = false
      end
    end

    private
      # `custom_validation_context?`: a context other than create/update.
      def _nested_custom_context?
        context = @validation_context
        !(context.nil? || context == :create || context == :update)
      end

      # nested_attributes.rb `assign_nested_attributes_for_one_to_one_association`.
      def assign_nested_attributes_for_one_to_one_association(association_name, attributes)
        options = self.class.nested_attributes_options[association_name]
        attributes = attributes.to_h if attributes.respond_to?(:permitted?)
        attributes = attributes.with_indifferent_access
        existing_record = send(association_name)

        if (options[:update_only] || !attributes["id"].blank?) && existing_record &&
            (options[:update_only] || existing_record.id.to_s == attributes["id"].to_s)
          assign_to_or_mark_for_destruction(existing_record, attributes, options[:allow_destroy]) unless call_reject_if(association_name, attributes)
        elsif attributes["id"].present?
          raise_nested_attributes_record_not_found!(association_name, attributes["id"])
        elsif !reject_new_record?(association_name, attributes)
          assignable_attributes = attributes.reject { |key, _| UNASSIGNABLE_KEYS.include?(key) }
          if existing_record && existing_record.new_record?
            existing_record.assign_attributes(assignable_attributes)
            association = association(association_name)
            association.send(:initialize_attributes, existing_record) if association.respond_to?(:initialize_attributes, true)
          else
            method = :"build_#{association_name}"
            singular = association(association_name)
            if respond_to?(method)
              send(method, assignable_attributes)
            elsif !singular.is_a?(ActiveRecord::Associations::BelongsToPolymorphicAssociation)
              singular.build(assignable_attributes)
            else
              raise ArgumentError, "Cannot build association `#{association_name}'. Are you trying to build a polymorphic one-to-one association?"
            end
          end
        end
      end

      # nested_attributes.rb `assign_nested_attributes_for_collection_association`.
      def assign_nested_attributes_for_collection_association(association_name, attributes_collection)
        options = self.class.nested_attributes_options[association_name]
        attributes_collection = attributes_collection.to_h if attributes_collection.respond_to?(:permitted?)

        unless attributes_collection.is_a?(Hash) || attributes_collection.is_a?(Array)
          raise ArgumentError, "Hash or Array expected for `#{association_name}` attributes, got #{attributes_collection.class.name}"
        end

        check_record_limit!(options[:limit], attributes_collection)

        if attributes_collection.is_a?(Hash)
          keys = attributes_collection.keys
          attributes_collection = if keys.include?("id") || keys.include?(:id)
            [attributes_collection]
          else
            attributes_collection.values
          end
        end

        association = association(association_name)
        existing_records = if association.loaded?
          association.target
        else
          attribute_ids = attributes_collection.filter_map { |a| a["id"] || a[:id] }
          attribute_ids.empty? ? [] : association.scope.where(association.klass.primary_key => attribute_ids).to_a
        end
        reader = public_send(association_name)

        attributes_collection.each do |attributes|
          attributes = attributes.to_h if attributes.respond_to?(:permitted?)
          attributes = attributes.with_indifferent_access

          if attributes["id"].blank?
            unless reject_new_record?(association_name, attributes)
              reader.build(attributes.reject { |key, _| UNASSIGNABLE_KEYS.include?(key) })
            end
          elsif (existing_record = existing_records.detect { |record| record.id.to_s == attributes["id"].to_s })
            unless call_reject_if(association_name, attributes)
              # The record the association holds (found, or added when it
              # was only read from the database for the ids).
              target = public_send(association_name)
              target_record = target.detect { |record| record.id.to_s == attributes["id"].to_s }
              if target_record
                existing_record = target_record
              else
                ActiveRecord::Associations::Association.push_target(target, existing_record)
              end
              assign_to_or_mark_for_destruction(existing_record, attributes, options[:allow_destroy])
            end
          else
            raise_nested_attributes_record_not_found!(association_name, attributes["id"])
          end
        end
      end

      def check_record_limit!(limit, attributes_collection)
        if limit
          limit = case limit
                  when Symbol then send(limit)
                  when Proc then limit.call
                  else limit
          end
          if limit && attributes_collection.size > limit
            raise TooManyRecords, "Maximum #{limit} records are allowed. Got #{attributes_collection.size} records instead."
          end
        end
      end

      def assign_to_or_mark_for_destruction(record, attributes, allow_destroy)
        record.assign_attributes(attributes.reject { |key, _| UNASSIGNABLE_KEYS.include?(key) })
        record.mark_for_destruction if has_destroy_flag?(attributes) && allow_destroy
      end

      # `ActiveModel::Type::Boolean.new.cast(hash["_destroy"])`.
      def has_destroy_flag?(hash)
        value = hash["_destroy"]
        return false if value.nil? || value == ""
        !FALSE_VALUES.include?(value)
      end

      def will_be_destroyed?(association_name, attributes)
        allow_destroy?(association_name) && has_destroy_flag?(attributes)
      end

      def allow_destroy?(association_name)
        self.class.nested_attributes_options[association_name][:allow_destroy]
      end

      def call_reject_if(association_name, attributes)
        return false if will_be_destroyed?(association_name, attributes)

        case callback = self.class.nested_attributes_options[association_name][:reject_if]
        when Symbol
          method(callback).arity == 0 ? send(callback) : send(callback, attributes)
        when Proc
          callback.call(attributes)
        end
      end

      def reject_new_record?(association_name, attributes)
        will_be_destroyed?(association_name, attributes) || call_reject_if(association_name, attributes)
      end

      def raise_nested_attributes_record_not_found!(association_name, record_id)
        model = self.class.reflect_on_association(association_name).klass.name
        raise RecordNotFound.new("Couldn't find #{model} with ID=#{record_id} for #{self.class.name} with ID=#{id}",
          model, "id", record_id)
      end

      # autosave_association.rb `association_valid?` for an autosave
      # association: nested errors for an invalid record.
      def _nested_association_valid?(name, record)
        return true if record.destroyed? || record.marked_for_destruction?
        context = _nested_custom_context? ? @validation_context : nil
        valid = record.valid?(context)
        _nested_import_errors(name, record) unless valid
        valid
      end

      def _nested_import_errors(name, record)
        own = errors
        child = record.errors
        if defined?(::ActiveModel::ErrorAttributes)
          error_attributes = ::ActiveModel::ErrorAttributes #: untyped
          begin
            error_attributes.objects(child).each do |error|
              error_attributes.import(own, error, attribute: "#{name}.#{error.attribute}")
            end
            return
          rescue RuntimeError
            # A message pushed without its attribute: the full message below.
            nil
          end
        end
        child.each do |message|
          text = message.to_s
          own << "#{ActiveSupport.humanize(name.to_s)} #{text[0].to_s.downcase}#{text[1..]}"
        end
      end

      # autosave_association.rb `validate_collection_association`.
      def _nested_validate_collection(name)
        return nil unless instance_variable_get(:"@#{name}_loaded") == true
        target = instance_variable_get(:"@#{name}_cache")
        records = if new_record? || _nested_custom_context?
          target.to_a
        else
          target.select { |record| record.changed_for_autosave? }
        end
        records.each { |record| _nested_association_valid?(name, record) }
        nil
      end

      # autosave_association.rb `validate_single_association`.
      def _nested_validate_single(name)
        return nil unless instance_variable_get(:"@#{name}_loaded") == true
        record = instance_variable_get(:"@#{name}_cache")
        if record && (record.changed_for_autosave? || _nested_custom_context?)
          _nested_association_valid?(name, record)
        end
        nil
      end

      # The owner's key (and `as:` type, and the inverse) a has_many or
      # has_one record is saved with.
      def _nested_set_owner_keys(name, record)
        reflection = self.class.reflect_on_association(name)
        _nested_owner_key_pairs(reflection).each do |foreign_key, owner_key|
          record.public_send(:"#{foreign_key}=", send(owner_key))
        end
        record.public_send(:"#{reflection.type}=", self.class.base_class.name) if reflection.type
        _set_inverse_instance(name, record) if respond_to?(:_set_inverse_instance, true)
        ActiveRecord::Associations::Association.enroll(record) if defined?(ActiveRecord::Associations::Association)
      end

      # `[foreign key column, owner column]` pairs of the association. A
      # composite owner key (`query_constraints [:shop_id, :id]`) with a
      # single foreign key column joins as Rails' `derive_fk_query_constraints`
      # derives it: `[:shop_id, <foreign key>]` against the owner's key, so
      # `shop_id` is copied along with the id.
      def _nested_owner_key_pairs(reflection)
        owner_keys = Array(reflection.active_record_primary_key).map(&:to_s)
        foreign_keys = Array(reflection.foreign_key).map(&:to_s)
        if owner_keys.size > 1 && foreign_keys.size == 1
          foreign_keys = [owner_keys.first] + foreign_keys
        end
        foreign_keys.zip(owner_keys.last(foreign_keys.size))
      end

      def _nested_autosave_disabled?(name)
        (self.class.reflect_on_association(name).options[:autosave] rescue nil) == false
      end

      # autosave_association.rb `save_collection_association` for an
      # autosave association.
      def _nested_save_collection(name, was_new)
        return nil unless instance_variable_get(:"@#{name}_loaded") == true
        return nil if _nested_autosave_disabled?(name)
        target = instance_variable_get(:"@#{name}_cache")
        records = if was_new || _nested_custom_context?
          target.to_a
        else
          target.select { |record| record.changed_for_autosave? }
        end
        doomed = records.select { |record| record.marked_for_destruction? }
        doomed.each do |record|
          record.destroy unless record.new_record?
          target.delete_if { |candidate| candidate.equal?(record) }
        end
        (records - doomed).each do |record|
          next if record.destroyed?
          saved = true
          if was_new || record.new_record?
            _nested_set_owner_keys(name, record)
            saved = record.save(validate: false)
          else
            saved = record.save(validate: false)
          end
          raise ActiveRecord::RecordInvalid.new(self) unless saved
        end
        records
      end

      # autosave_association.rb `save_has_one_association` for an
      # autosave association.
      def _nested_save_has_one(name)
        return nil unless instance_variable_get(:"@#{name}_loaded") == true
        return nil if _nested_autosave_disabled?(name)
        record = instance_variable_get(:"@#{name}_cache")
        return nil if record.nil? || record.destroyed?
        if record.marked_for_destruction?
          record.destroy
          return nil
        end
        reflection = self.class.reflect_on_association(name)
        pairs = _nested_owner_key_pairs(reflection)
        if record.changed_for_autosave? || record.new_record? ||
            pairs.any? { |foreign_key, owner_key| record.public_send(foreign_key) != send(owner_key) } ||
            pairs.any? { |foreign_key, _| record.attribute_changed?(foreign_key) }
          _nested_set_owner_keys(name, record)
          saved = record.save(validate: false)
          raise ActiveRecord::Rollback unless saved
          saved
        end
      end

      # autosave_association.rb `save_belongs_to_association` for an
      # autosave association.
      def _nested_save_belongs_to(name)
        return nil unless instance_variable_get(:"@#{name}_loaded") == true
        return nil if _nested_autosave_disabled?(name)
        record = instance_variable_get(:"@#{name}_cache")
        return nil if record.nil? || record.destroyed?
        reflection = self.class.reflect_on_association(name)
        if record.marked_for_destruction?
          public_send(:"#{reflection.foreign_key}=", nil)
          record.destroy
        elsif record.new_record? || record.changed_for_autosave?
          saved = record.save(validate: false)
          if saved && record.id && public_send(reflection.foreign_key) != record.id
            public_send(:"#{reflection.foreign_key}=", record.id)
          end
        end
        nil
      end
  end

  class Base
    extend NestedAttributes::ClassMethods
    include NestedAttributes
  end
end
