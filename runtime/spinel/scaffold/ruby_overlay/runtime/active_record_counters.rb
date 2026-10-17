# CRuby-only ActiveRecord counter writes: `update_counters`,
# `increment_counter` / `decrement_counter` and `increment!` /
# `decrement!`, as Rails 8's counter_cache.rb, relation.rb
# (`update_counters`, `_increment_attribute`) and persistence.rb
# (`increment`, `increment!`) write them.
#
# Lives on the CRuby overlay, not the shared runtime/ruby/active_record/:
# the counter names are variables (`public_send`, `self[name] +=`) and a
# `touch:` of a Symbol, an Array or `true` is a union, the dynamic shapes
# the typed shared runtime refuses. The shared `Model.update_counters`
# (connection.rb: `col = col + n` on `WHERE id =`, no `touch:`) stays for
# the strict targets; the later definition here is the one Ruby runs.
module ActiveRecord
  class Relation
    # `where(...).update_counters(retry_count: 1, touch: true)`: one
    # `UPDATE ... SET col = COALESCE(col, 0) + n` over the rows the
    # relation scopes, no callbacks or validations. A negative amount
    # subtracts its absolute value. `touch: true` also sets `updated_at`
    # (when the table has one), `touch: :col` / `touch: [:a, :b]` those
    # columns too. Answers the affected-row count.
    def update_counters(counters)
      counters = counters.dup
      touch = counters.delete(:touch)
      sets = []
      counters.each do |name, value|
        column = name.to_s
        raise ArgumentError, "invalid counter column: #{column.inspect}" unless column.match?(/\A\w+\z/)
        sign = value < 0 ? "-" : "+"
        sets.push("#{column} = COALESCE(#{column}, 0) #{sign} #{_counter_amount_sql(value.abs)}")
      end
      sets.concat(_counter_touch_sets(touch)) if touch
      return 0 if sets.empty?
      update_all(sets.join(", "))
    end

    private

    # An Integer, Float or BigDecimal literal for the amount.
    def _counter_amount_sql(amount)
      return amount.to_s("F") if defined?(BigDecimal) && amount.is_a?(BigDecimal)
      raise TypeError, "counter amount must be numeric, got #{amount.class}" unless amount.is_a?(Numeric)
      amount.to_s
    end

    # Rails' `touch_attributes_with_time(*names)`: `updated_at` where the
    # table has it, plus the named columns, all at one instant.
    def _counter_touch_sets(touch)
      names = touch == true ? [] : Array(touch).map(&:to_s)
      names.unshift("updated_at") if @model.schema_columns.include?(:updated_at)
      names.uniq!
      now = ActiveRecord.adapter.escape_value(ActiveSupport.db_now)
      names.map { |column| "#{column} = #{now}" }
    end
  end

  class Base
    # `Model.update_counters(id, comments_count: 1, touch: true)`: the
    # row with that primary key (`id` is an Array of the key's values
    # for a composite key), outside the default scope but for the
    # island Core's `unscoped` keeps.
    def self.update_counters(id, counters)
      key = primary_key
      condition = key.is_a?(Array) ? key.zip(Array(id)).to_h : { key => id }
      unscoped.where(condition).update_counters(counters)
    end

    # `Model.increment_counter(:visits, id, by: 2, touch: true)`.
    def self.increment_counter(counter_name, id, by: 1, touch: nil)
      update_counters(id, counter_name => by, touch: touch)
    end

    # `Model.decrement_counter(:visits, id)`.
    def self.decrement_counter(counter_name, id, by: 1, touch: nil)
      update_counters(id, counter_name => -by, touch: touch)
    end

    # `record.increment(:visits)`: in memory only, a NULL counting as 0.
    def increment(attribute, by = 1)
      self[attribute] ||= 0
      self[attribute] += by
      self
    end

    def decrement(attribute, by = 1)
      increment(attribute, -by)
    end

    # `record.increment!(:visits, 2, touch: true)`: the in-memory change
    # since the database value, written atomically as a counter update
    # (a concurrent bump is added to, not overwritten), then the column
    # is no longer dirty.
    def increment!(attribute, by = 1, touch: nil)
      increment(attribute, by)
      change = self[attribute] - (attribute_in_database(attribute.to_s) || 0)
      self.class.update_counters(id, attribute => change, touch: touch)
      clear_attribute_change(attribute)
      self
    end

    def decrement!(attribute, by = 1, touch: nil)
      increment!(attribute, -by, touch: touch)
    end
  end
end
