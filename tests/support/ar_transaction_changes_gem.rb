module ArTransactionChanges
  VERSION = "1.1.11"

  if ActiveRecord.version >= Gem::Version.new("7.1")
    def run_callbacks(kind, type = nil)
      begin
        super
      ensure
        @transaction_changed_attributes = nil if kind == :commit || kind == :rollback
      end
    end

    if ActiveRecord.version >= Gem::Version.new("8.1.0.alpha")
      def _run_commit_callbacks
        begin
          super
        ensure
          @transaction_changed_attributes = nil
        end
      end

      def _run_rollback_callbacks
        begin
          super
        ensure
          @transaction_changed_attributes = nil
        end
      end
    end
  else
    def run_callbacks(kind)
      begin
        super
      ensure
        @transaction_changed_attributes = nil if kind == :commit || kind == :rollback
      end
    end
  end

  def transaction_changed_attributes
    @transaction_changed_attributes ||= {}
  end

  def _write_attribute(attr_name, value)
    _store_transaction_changed_attributes(attr_name) do super(attr_name, value) end
  end

  if ActiveRecord.version >= Gem::Version.new("6.1.0.alpha")
    def write_attribute(attr_name, value)
      _store_transaction_changed_attributes(attr_name) do super(attr_name, value) end
    end
  end

  def attribute_will_change!(attr_name)
    if transaction_changed_attributes.key?(attr_name)
      nil
    else
      value = _read_attribute_for_transaction(attr_name)
      value = _deserialize_transaction_change_value(attr_name, value)
      transaction_changed_attributes[attr_name] = ActiveSupport.indifferent_value(value, true)
    end
    super
  end

  def init_internals
    super
    @transaction_changed_attributes = nil
  end
  private :init_internals

  def _deserialize_transaction_change_value(attr_name, value)
    attribute = @attributes[attr_name]
    if attribute.type.is_a?(::ActiveRecord::Type::Serialized)
      nil
    else
      return value
    end
    attribute.type.deserialize(value)
  end
  private :_deserialize_transaction_change_value

  def _store_transaction_changed_attributes(attr_name)
    __rh_attr_name = attr_name.to_s
    old_value = _read_attribute_for_transaction(__rh_attr_name)
    ret = yield
    new_value = _read_attribute_for_transaction(__rh_attr_name)
    if !(transaction_changed_attributes.key?(__rh_attr_name)) && new_value != old_value
      transaction_changed_attributes[__rh_attr_name] = ActiveSupport.indifferent_value(_deserialize_transaction_change_value(__rh_attr_name, old_value), true)
    else
      if transaction_changed_attributes.key?(__rh_attr_name)
        new_value = _deserialize_transaction_change_value(__rh_attr_name, new_value)
        
        stored_value = transaction_changed_attributes[__rh_attr_name]
        
        transaction_changed_attributes.delete(__rh_attr_name) if new_value == stored_value
      end
    end
    ret
  end
  private :_store_transaction_changed_attributes

  def _read_attribute_for_transaction(attr_name)
    attribute = @attributes[attr_name]
    if attribute.type.is_a?(::ActiveRecord::Type::Serialized)
      if attribute.came_from_user?
        attribute.type.serialize(attribute.value_before_type_cast)
      else
        attribute.value_before_type_cast
      end
    else
      _read_attribute(attr_name)
    end
  end
  private :_read_attribute_for_transaction
end
