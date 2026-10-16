# Runtime port of the sequenced_column gem (sequenced_column 1.2.1,
# lib/sequenced_column/extension.rb), what a model body's
# `sequenced_column :number, scope: :order` runs at class load.
#
# The gem is Rails-dependent (`ActiveSupport.on_load(:active_record) {
# include SequencedColumn }`), so no emitted tree carries its Ruby; this
# port writes the same behavior against the tree's ActiveRecord::Base.
# The macro records the column's options and registers `before_create
# :_record_sequences` and `after_rollback :_invalidate_record_sequences,
# on: :create`. At create the record draws the next number from
# `SequencedColumn.store` (Core's initializer sets the in-memory store in
# test) after seeding it from the largest value already in the table for the
# record's scope, and a rolled-back create returns its ticket.
#
# One difference: the gem's `value_format` branch asks the database for
# `MAX(CAST(SUBSTRING(column, start) AS DECIMAL))`; the relation port
# aggregates plain columns only, so the port reads the column's values and
# takes the maximum of their numeric suffix in Ruby (an empty suffix is 0,
# as the gem's CASE answers).
require "active_support/concern"
require "active_support/core_ext/hash/keys"
require "active_support/core_ext/string/inflections"
require "active_support/core_ext/numeric/time"

module SequencedColumn
  extend ActiveSupport::Concern

  class << self
    attr_accessor :store
  end

  # mattr_accessor :store: the instance reader the gem's `store` calls use.
  def store
    SequencedColumn.store
  end

  module ClassMethods
    def sequenced_column(column, options = {})
      options.assert_valid_keys(:scope, :value_format, :no_overwrite, :condition)

      unless respond_to?(:_sequenced_columns)
        class << self
          attr_reader :_sequenced_columns
        end
      end

      sequence_data = options.merge(:column => column)

      sequence_data[:sequence_name] = "#{table_name.singularize}_sequences"
      if options[:scope]
        if options[:scope].kind_of?(Array)
          sequence_data[:sequence_scope_columns] = options[:scope]
        else
          # Use the foreign key, like order_id as the name for the column if there is a scope
          sequence_data[:sequence_scope_columns] = [options[:scope].to_s.foreign_key.to_sym]
        end
      end

      @_sequenced_columns ||= []
      @_sequenced_columns << sequence_data

      before_create :_record_sequences
      after_rollback :_invalidate_record_sequences, on: :create
    end
  end

  # !Warning!: Only call when it's certain that a DB transaction rollback has failed.
  def unsafe_invalidate_record_sequences
    _sequenced_log("[sequenced_column] Manual invalidation of sequences")
    _invalidate_record_sequences
  end

  private

  def _sequenced_log(message)
    log = self.class.respond_to?(:logger) ? self.class.logger : nil
    log.info(message) if log
  end

  def _next_sequence(sequence_data)
    raise RuntimeError, "store is not set up for SequencedColumn. please set SequencedColumn.store to a store instance." if store.nil?

    sequence_key = _sequence_key(sequence_data)
    recycled_key = _recycled_ticket_key(sequence_data)

    _sequenced_log("[sequenced_column] Generating sequence for #{sequence_key}")

    db_count = _old_count_from_last_db_entry(sequence_data)
    _sequenced_log("mysql count: #{db_count.inspect}")

    result = store.recycled(recycled_key)
    if result != 0
      _sequenced_log("using ticket #{result}")
    else
      result = store.reserve(sequence_key, db_count, 1.day)

      if db_count >= result
        _sequenced_log("starting over. db count is higher or equal to store (#{db_count} >= #{result})")
        store.delete(sequence_key)
        return _next_sequence(sequence_data)
      end
    end

    _sequenced_log("store value: sequence_number=#{result}")

    result.to_i
  end

  # see if we can guess it from the last db entry. This also serves as a fallback if Redis were ever to lose data.
  def _old_count_from_last_db_entry(sequence_data)
    rel = self.class.unscoped

    if sequence_data[:scope]
      _scope_values_hash(sequence_data).map do |column, value|
        rel = rel.where(column => value)
      end
    end

    if sequence_data[:value_format]
      start_at = sequence_data[:value_format].index('%')
      rel.pluck(sequence_data[:column]).map do |value|
        suffix = value.to_s[start_at..].to_s
        suffix.empty? ? 0 : suffix.to_f.to_i
      end.max.to_i
    else
      rel.maximum(sequence_data[:column]).to_i
    end
  end

  def _scope_id(sequence_data)
    _scope_values_hash(sequence_data).map { |k, v| "#{k}:#{v}" }.join(';') if sequence_data[:scope]
  end

  def _scope_values_hash(sequence_data)
    non_methods = []
    hash = {}
    nil_columns = []
    sequence_data[:sequence_scope_columns].each do |column|
      if respond_to?(column)
        hash[column] = send(column)
        nil_columns << column if hash[column].nil?
      else
        non_methods << column
      end
    end

    raise RuntimeError, "The following scopes are not methods: #{non_methods.join(', ')}" unless non_methods.empty?
    raise RuntimeError, "The following scopes had nil values: #{nil_columns.join(', ')}" unless nil_columns.empty?

    hash
  end

  # Stores numbers for sequenced columns
  def _record_sequences
    _sequenced_columns.each do |sequence_data|
      next if sequence_data[:condition] && !send(sequence_data[:condition])
      next if sequence_data[:no_overwrite] && !send(sequence_data[:column]).nil?
      value = _next_sequence(sequence_data)
      if value_format = sequence_data[:value_format]
        value = value_format % value
      end
      write_attribute sequence_data[:column], value
    end
  end

  # @returns [boolean] Returns `true` if any sequenced columns got invalidated. `false` otherwise.
  def _invalidate_record_sequences
    did_invalidate = false

    _sequenced_columns.each do |sequence_data|
      next if sequence_data[:condition] && !send(sequence_data[:condition])
      value = read_attribute(sequence_data[:column])
      unless value.nil?
        # Erase value, so no repeated invalidation can commit the same sequence number twice.
        write_attribute(sequence_data[:column], nil)

        recycled_key = _recycled_ticket_key(sequence_data)
        sequence_key = _sequence_key(sequence_data)
        _sequenced_log("[sequenced_column] Returning ticket #{value} into #{recycled_key}")
        store.commit(recycled_key, sequence_key, value)

        did_invalidate = true
      end
    end

    did_invalidate
  end

  def _sequenced_columns
    self.class.base_class._sequenced_columns
  end

  def _sequence_key(sequence_data)
    scope_name = sequence_data[:sequence_name]
    scope_key = [sequence_data[:column], _scope_id(sequence_data)].compact.join('::')
    "#{scope_name}::#{scope_key}"
  end

  def _recycled_ticket_key(sequence_data)
    "#{_sequence_key(sequence_data)}:recycled"
  end
end

# The gem's `ActiveSupport.on_load(:active_record) { include SequencedColumn }`.
# The tree has defined the base by the time boot loads this (a tree with no
# initializer or load-hook registration never fires the hook), so include
# at once; a base defined later gets it from the hook.
if defined?(ActiveRecord::Base)
  ActiveRecord::Base.include(SequencedColumn)
else
  ActiveSupport.on_load(:active_record) do
    include SequencedColumn
  end
end
