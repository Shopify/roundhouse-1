# CRuby-only ActiveRecord::Relation extensions: enumerable terminals
# and record-valued IN lists.
#
# Lives on the CRuby overlay, not shared runtime/ruby/active_record/
# relation.rb: the IN-list coercion dispatches on
# is_a?(ActiveRecord::Base) — exactly the shape the typed shared
# runtime refuses (see the monomorphic-API rule). When lobsters comes
# up on another target, that target grows its own variant.
# (`Relation#+` lived here too until it moved to the shared runtime.)
module ActiveRecord
  class Relation
    # Enumerable terminal Rails relations answer via to_a delegation;
    # lobsters' Comment.arrange_for_user groups its ordered relation by
    # parent id.
    def group_by(&block)
      to_a.group_by(&block)
    end

    # Rails' `Relation#update(id = :all, attributes)`: each record of the
    # relation updated through its callbacks and validations, or the one
    # (or each of the several) found by `id` within this scope. An
    # `extending` module's `update(*args)` override (Core's
    # `MutationGuard::Relation`) ends in `super` here. The records come
    # back as Rails returns them: the relation's records for `:all`'s
    # `each`, the found record(s) otherwise.
    def update(id = :all, attributes)
      return each { |record| record.update(attributes) } if id == :all
      scoping do
        if id.is_a?(Array)
          id.map { |one| update(one, attributes) }
        else
          record = find(id)
          record.update(attributes)
          record
        end
      end
    end

    # Rails' `ActiveRecord::Delegation::ClassSpecificRelation`: a method
    # the relation does not define goes to the model's class methods, run
    # under this relation's scope (`Order.where(…).by_state` calling a
    # `def self.by_state`; `name`, `table_name` and any other class
    # method answered from an `extending` module, whose methods run with
    # the relation as `self`; a module prepended on the Relation, such as
    # Core's `GenerateOid::ActiveRecordInsertPatch` asking `oid_enabled?`).
    # Only what the model answers publicly (`respond_to?`) is forwarded,
    # so a typo is still a NoMethodError.
    # CRuby-only: the shared relation has no `method_missing`.
    def method_missing(method, *args, **kwargs, &block)
      model = @model
      return super if model.nil? || !model.respond_to?(method)
      scoping { model.public_send(method, *args, **kwargs, &block) }
    end

    def respond_to_missing?(method, include_private = false)
      model = @model
      (!model.nil? && model.respond_to?(method)) || super
    end

    private

    # Rails casts an ActiveRecord object used as a condition value to its
    # id (`where(story_id: [<Story rows>, 7])` → `IN (3, 5, 7)`). The
    # shared escape_list feeds values straight to the adapter, which only
    # knows scalars — so the record→id cast happens here, on the overlay.
    def escape_list(vals)
      out = []
      vals.each do |v|
        v = v.id if v.is_a?(ActiveRecord::Base)
        out << ActiveRecord.adapter.escape_value(v)
      end
      out.join(", ")
    end
  end

  # Rails 8.2 QueryMethods#select: with a block, Enumerable's select over
  # the loaded records (an Array), and a block with fields is an
  # ArgumentError. The compiler renames the call sites it types as a
  # relation to `filter` (`relation_select_block`); one it cannot type
  # (a relation answered by an untyped helper) reached the shared
  # projection, which raised. CRuby-only, so the shared `select` stays
  # monomorphic for the strict targets.
  module RelationSelectBlock
    def select(*fields, &block)
      return super(*fields) unless block
      raise ArgumentError, "'select' with block doesn't take arguments." if fields.any?
      filter(&block)
    end
  end
  Relation.prepend(RelationSelectBlock)

  # Rails' `Relation#select_values`: the fields `select` was given, as
  # written (Symbols stay Symbols), in order, each once (`select_values |=
  # fields`). The shared relation keeps only the projection's SQL, so the
  # overlay records the specs beside it: Core's
  # `Podding::Database::Iteration.check_relation_for_batching` reads
  # `relation.select_values` to see which columns a batched relation
  # selects. Arrays are replaced, never appended to, so a spawned
  # relation shares its parent's list safely. `unscope(:select)` clears it
  # with the SQL.
  module RelationSelectValues
    def select!(*specs)
      super
      @select_values = select_values | specs.flatten
      self
    end

    def select_values
      @select_values || []
    end

    def unscope_clause(scope)
      @select_values = [] if scope == :select
      super
    end
    private :unscope_clause
  end
  Relation.prepend(RelationSelectValues)

  # Rails' `Relation#joins_values` / `left_outer_joins_values`: the specs
  # `joins` / `left_outer_joins` were given, as written (Symbols, Hashes,
  # Strings), in order, each once (`joins_values |= args`). The shared
  # relation keeps only the join SQL, so the overlay records the specs
  # beside it, as it does for `select_values`: Core's
  # `GeneralizedBatchedFinder::Cursor` reads `relation.joins_values` to
  # require fully-qualified cursor columns once the relation joins a table.
  # Arrays are replaced, never appended to, so a spawned relation shares
  # its parent's list safely. `unscope(:joins)` clears the lists with the
  # SQL, and a relation built under a current scope inherits the scope's.
  module RelationJoinsValues
    def joins!(spec)
      super
      @joins_values = joins_values | [spec]
      self
    end

    def left_outer_joins!(spec)
      super
      @left_outer_joins_values = left_outer_joins_values | [spec]
      self
    end

    def joins_values
      @joins_values || []
    end

    def left_outer_joins_values
      @left_outer_joins_values || []
    end

    def inherit_scope(scope)
      super
      @joins_values = joins_values | scope.joins_values
      @left_outer_joins_values = left_outer_joins_values | scope.left_outer_joins_values
      self
    end
    private :inherit_scope

    def unscope_clause(scope)
      if scope == :joins || scope == :left_outer_joins
        @joins_values = []
        @left_outer_joins_values = []
      end
      super
    end
    private :unscope_clause
  end
  Relation.prepend(RelationJoinsValues)

  # Join aliasing by a where hash's key. Reflections are dynamic in Rails
  # (an emitted per-model object answering `through_reflection?`,
  # `polymorphic?`, `klass`, ...) and the shared runtime does not type
  # them, so the three methods that read them live here, outside the typed
  # gate; relation.rbs declares them for the shared body's calls. They
  # reopen Relation, so `reference_tables` and `reflected_join_sql` reach
  # them as before.
  class Relation
    # `joins(:email_addresses)` with `where(email_addresses: {...})`, the
    # association named differently from its table (`class_name:
    # "CustomerEmailAddress"`): Rails joins the table aliased to the
    # association's name, as the where's qualifier says. A `joins` that was
    # rendered before the where named the association is re-rendered
    # aliased, in place.
    def alias_referenced_joins(keys)
      return if @joins.empty? || !@model.respond_to?(:reflect_on_association)
      keys.each do |key|
        reflection = @model.reflect_on_association(key.to_sym)
        next unless alias_join_reflection?(@model, reflection)
        ["INNER JOIN", "LEFT OUTER JOIN"].each do |kind|
          plain = reflection_join_sql(@model, reflection, kind)
          index = @joins.index(plain)
          next if index.nil?
          own_lists
          @joins[index] = reflection_join_sql(@model, reflection, kind, key.to_s)
        end
      end
    end

    # A hop `where(<name>: {...})` can alias: a direct, unscoped,
    # non-polymorphic association whose name is not its table's.
    def alias_join_reflection?(owner, reflection)
      return false if reflection.nil? || !owner.equal?(@model)
      return false if reflection.through_reflection? || reflection.polymorphic?
      return false if reflection.macro == :has_and_belongs_to_many || reflection._roundhouse_scoped?
      reflection.name.to_s != reflection.klass.table_name.to_s
    end

    # The alias a join of `reflection` off `owner` takes: its own name when
    # a where hash already keys off it, else none.
    def join_alias_for(owner, reflection)
      return nil unless alias_join_reflection?(owner, reflection)
      @referenced.include?(reflection.name.to_s) ? reflection.name.to_s : nil
    end

# A `joins` hop's SQL, aliased to the association's name when a where
# hash keys off it (`join_alias_for`); the shared `reflected_join_sql`
# calls this so the alias lookup stays out of the typed body.
def aliased_join_sql(owner, reflection, kind)
  reflection_join_sql(owner, reflection, kind, join_alias_for(owner, reflection))
end
  end
end
