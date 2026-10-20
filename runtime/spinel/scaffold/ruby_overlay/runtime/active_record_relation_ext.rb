# frozen_string_literal: true
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

  # Rails' `build_joins` order. Association joins form ONE tree, rendered
  # ahead of the string joins written after the first of them; the
  # lowering renders every association join as a string at its call
  # position, so `joins(:responders).joins("LEFT JOIN (...) AS d ON d.k =
  # groups.k").joins(responders: :group)` reaches the relation as three
  # fragments with the one naming `groups` before the one introducing it
  # (PostgreSQL: missing FROM-clause entry), and the third repeating the
  # first's `INNER JOIN responders ...` (table name specified more than
  # once). Two repairs on each appended fragment, neither touching a query
  # PostgreSQL would accept:
  #
  # * a clause already in the list is dropped from it (the tree joins a
  #   hop once);
  # * a fragment that names a table or alias a LATER fragment introduces
  #   moves behind that fragment.
  module RelationJoinOrdering
    def joins!(spec)
      before = @joins.length
      super
      settle_join_fragments(before)
      self
    end

    def left_outer_joins!(spec)
      before = @joins.length
      super
      settle_join_fragments(before)
      self
    end

    private

    def settle_join_fragments(before)
      return if @joins.length <= before
      frag = @joins.pop
      clauses = join_clauses_of(frag)
      kept = clauses.reject { |clause| join_clause_present?(clause) }
      return if kept.empty?
      frag = kept.join(" ")
      return if @joins.include?(frag)
      @joins << frag
      return if @joins.length < 2
      moves = 0
      i = 0
      while i < @joins.length - 1 && moves < 16
        moved = false
        j = i + 1
        while j < @joins.length
          if join_names_later_table?(@joins[i], @joins[j], i)
            @joins.insert(j, @joins.delete_at(i))
            moves += 1
            moved = true
            break
          end
          j += 1
        end
        i += 1 unless moved
      end
    end

    # True when `clause` is one of the join clauses a listed fragment holds.
    def join_clause_present?(clause)
      @joins.any? { |j| j == clause || join_clauses_of(j).include?(clause) }
    end

    # The `JOIN` clauses of a fragment, split where a clause keyword starts
    # outside any parenthesis (a joined subquery's own joins stay inside its
    # clause). A fragment with no such start is one clause.
    def join_clauses_of(frag)
      clauses = []
      depth = 0
      start = 0
      i = 0
      n = frag.length
      while i < n
        ch = frag[i]
        if ch == "("
          depth += 1
        elsif ch == ")"
          depth -= 1
        elsif depth == 0 && i > start && frag[i - 1] == " " && frag[i, 20].match?(/\A(?:INNER|LEFT|RIGHT|FULL|CROSS)\s+(?:OUTER\s+)?JOIN\b/i)
          clauses << frag[start...(i - 1)]
          start = i
        end
        i += 1
      end
      clauses << frag[start...n]
      clauses
    end

    # True when `early` names a table or alias that `later` introduces (and
    # `early`, the base table and the joins before `early` do not).
    def join_names_later_table?(early, later, index)
      introduced = join_introduced_names(later)
      return false if introduced.empty?
      own = join_introduced_names(early)
      before = []
      k = 0
      while k < index
        join_introduced_names(@joins[k]).each { |n| before << n }
        k += 1
      end
      back = join_referenced_names(later)
      early_refs = join_referenced_names(early)
      return false if own.any? { |n| back.include?(n) }
      introduced.any? do |n|
        early_refs.include?(n) && !own.include?(n) && !before.include?(n) && n != @table.to_s
      end
    end

    # The table names and aliases a join fragment puts in scope.
    def join_introduced_names(frag)
      names = []
      frag.scan(/\bJOIN\s+"?([A-Za-z_][A-Za-z0-9_]*)"?(?:\s+(?:AS\s+)?"?([A-Za-z_][A-Za-z0-9_]*)"?)?\s+ON\b/i).each do |m|
        names << m[0].downcase
        names << m[1].downcase unless m[1].nil?
      end
      frag.scan(/\bAS\s+"?([A-Za-z_][A-Za-z0-9_]*)"?\s+ON\b/i).each { |m| names << m[0].downcase }
      names
    end

    # The `name.column` qualifiers a join fragment uses.
    def join_referenced_names(frag)
      names = []
      frag.scan(/(?<![A-Za-z0-9_.])"?([A-Za-z_][A-Za-z0-9_]*)"?\.[A-Za-z_*"]/).each { |m| names << m[0].downcase }
      names
    end
  end
  Relation.prepend(RelationJoinOrdering)

  # Rails' `pluck(catalogs: :id)` / `pluck(catalogs: [:id, :name])`: a
  # table => column(s) Hash names `table.column`, as the string form does.
  module RelationPluckHash
    def pluck(*cols)
      return super unless cols.any? { |col| col.is_a?(Hash) }
      fields = []
      cols.flatten.each do |col|
        if col.is_a?(Hash)
          col.each { |table, named| Array(named).each { |name| fields << "#{table}.#{name}" } }
        else
          fields << col
        end
      end
      super(*fields)
    end
  end
  Relation.prepend(RelationPluckHash)

  # Rails' `Relation#from_clause`: always a FromClause (never nil), its
  # `value` the source `from` was given (nil when the relation has none).
  # Core's `Pagination::SqlPaginationBackend#ignore_index_for_id_sort`
  # reads `scope.from_clause.value` to see whether an index hint is
  # already spelled into the FROM. The shared relation keeps the source
  # in `@from` (a subquery as its rendered SQL), so this answers it
  # without a second copy.
  module RelationFromClause
    FromClause = Struct.new(:value) do
      def empty?
        value.nil?
      end
    end

    def from_clause
      FromClause.new(@from)
    end
  end
  Relation.prepend(RelationFromClause)

  # `relation.map` / `collect` with no block is the records' Enumerator
  # (Rails delegates Enumerable to `records`), so `results.map.with_index
  # { |row, i| ... }` (Core's `SqlPaginationBackend#raw_results_map_with_
  # index`) numbers the rows. The shared relation's `map` is the blocked
  # form only: with no block its `yield` raises LocalJumpError, or on an
  # empty relation answers `[]`, which has no `with_index`.
  module RelationBlocklessMap
    def map(&block)
      return loaded_records.map unless block
      super
    end

    def collect(&block)
      return loaded_records.map unless block
      super
    end
  end
  Relation.prepend(RelationBlocklessMap)

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
      # Hops below the first first: aliasing a hop renames its table in the
      # hops after it, which are matched under the names they were rendered
      # with.
      keys = keys.reject { |k| !@model.reflect_on_association(k.to_sym).nil? } +
             keys.select { |k| !@model.reflect_on_association(k.to_sym).nil? }
      keys.each do |key|
        reflection = @model.reflect_on_association(key.to_sym)
        # A key that is no association of this model may name a hop below
        # the first (`joins(conditions: :owner_market_definition)` then
        # `where.not(owner_market_definition: {...})`), as Rails finds it
        # in its join dependencies.
        alias_nested_join_hop(key.to_sym) if reflection.nil?
        next unless alias_join_reflection?(@model, reflection)
        found = false
        ["INNER JOIN", "LEFT OUTER JOIN"].each do |kind|
          plain = reflection_join_sql(@model, reflection, kind)
          index = @joins.index(plain)
          next if index.nil?
          found = true
          own_lists
          @joins[index] = reflection_join_sql(@model, reflection, kind, key.to_s)
        end
        # The first hop of a `joins(a: :b)` the compiler rendered whole.
        alias_join_hop(@model, reflection, key.to_s) unless found
      end
    end

    # The pre-rendered join of a hop reached through other hops
    # (`joins(a: :b)` expands to one SQL string at compile time, the hop
    # `b` under its table name) is re-rendered aliased to `b`, in place,
    # when a where hash then keys off `b`. The hop is found by walking the
    # associations down from this model (as far as a `joins` hash nests in
    # practice) for one named `name` whose plain SQL is in the join text.
    def alias_nested_join_hop(name)
      seen = [@model]
      level = [@model]
      3.times do
        following = []
        level.each do |owner|
          next unless owner.respond_to?(:_association_names)
          owner._association_names.each do |assoc|
            reflection = owner.reflect_on_association(assoc)
            next if reflection.nil?
            target = joinable_target(reflection)
            next if target.nil?
            if assoc.to_sym == name && alias_join_reflection?(owner, reflection)
              alias_join_hop(owner, reflection, name.to_s)
            end
            next if seen.include?(target)
            seen << target
            following << target
          end
        end
        level = following
      end
    end

    # The class a join through `reflection` lands on, or nil where the
    # walk cannot go: a polymorphic or habtm association, a class the app
    # does not define or has not loaded yet (the walk follows the joins
    # already built, whose models are loaded, rather than loading the
    # app's whole association graph), an option the compiler did not carry
    # (each of which `joins` itself refuses by name when asked for).
    def joinable_target(reflection)
      return nil unless reflection.respond_to?(:polymorphic?)
      return nil if reflection.macro == :has_and_belongs_to_many || reflection.polymorphic?
      return nil unless loaded_constant?(reflection.class_name)
      reflection.klass
    rescue NameError, NotImplementedError
      nil
    end

    def loaded_constant?(name)
      scope = Object
      name.to_s.delete_prefix("::").split("::").each do |part|
        return false if !scope.const_defined?(part, false) || scope.autoload?(part)
        scope = scope.const_get(part, false)
      end
      true
    end

    # The compiler writes a hop's tables unquoted (`INNER JOIN
    # market_definitions ON market_definitions.id = ...`) where
    # `reflection_join_sql` quotes them, so the hop is matched with its
    # quotes optional.
    def alias_join_hop(owner, reflection, key)
      ["INNER JOIN", "LEFT OUTER JOIN"].each do |kind|
        plain = reflection_join_sql(owner, reflection, kind)
        pattern = Regexp.new(Regexp.escape(plain).gsub('"', '"?'))
        index = @joins.index { |frag| frag.match?(pattern) }
        next if index.nil?
        aliased = reflection_join_sql(owner, reflection, kind, key)
        own_lists
        frag = @joins[index]
        match = frag.match(pattern)
        # The hops after it join off its table: they name the alias.
        table = reflection.klass.table_name.to_s
        tail = match.post_match.gsub(Regexp.new("(?<![\w\"])\"?#{Regexp.escape(table)}\"?\."), "\"#{key}\".")
        @joins[index] = match.pre_match + aliased + tail
      end
    end

    # A hop `where(<name>: {...})` can alias: a direct, unscoped,
    # non-polymorphic association whose name is not its table's.
    def alias_join_reflection?(owner, reflection)
      return false if reflection.nil?
      # A SQLite tree's reflection is the shared one, a name alone.
      return false unless reflection.respond_to?(:through_reflection?)
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

    # The table a `"table.column"` where key names, other than this
    # relation's own; nil for any other key.
    def dotted_reference(key)
      return nil if key.is_a?(Array)
      text = key.to_s
      return nil unless text.include?(".")
      qualifier = text.split(".").first.to_s.delete('"')
      return nil if qualifier.empty? || qualifier == @table || qualifier == @model.table_name.to_s
      qualifier
    end

    # A non-Array `?` or `:name` bind's SQL. A Relation is its own SQL
    # (Rails' `replace_bind_variable`); anything else is quoted.
    def bind_scalar_sql(value)
      return value.to_sql if value.is_a?(Relation)
      ActiveRecord.adapter.escape_value(value)
    end

    # A nested where hash's column on its owner's table
    # (`where(condition_responders: {condition_id:})`): the owner's own
    # `alias_attribute` names the column, as `where(subdomain:)` does on
    # the model itself. The shared `hash_conditions` calls this so the
    # owner's untyped reflection stays out of the typed body.
    def nested_column_name(owner, col)
      owner.nil? ? col.to_s : owner._attribute_name(col.to_s)
    end
  end
end

module ActiveRecord
  # `where(assoc: record)` over a composite-key association (Core's
  # yugabyte_overrides / query_constraints keys): the reflection's
  # `_roundhouse_key_pairs` name each [foreign key, primary key] pair, and
  # every pair is matched, as Rails' AssociationQueryValue does for a
  # composite foreign key.
  module CompositeAssociationKeyPredicates
    # A join over a composite key matches every pair: the reflection's
    # `[foreign key side, referenced side]` pairs, read as
    # `[target column, owner column]` (a belongs_to's foreign-key side is
    # the owner's).
    def join_key_pairs(reflection)
      pairs = reflection.respond_to?(:_roundhouse_key_pairs) ? reflection._roundhouse_key_pairs : nil
      return super if pairs.nil?
      reflection.belongs_to? ? pairs.map { |pair| [pair[1], pair[0]] } : pairs
    end

    def association_key_predicates(reflection, val, note)
      pairs = reflection.respond_to?(:_roundhouse_key_pairs) ? reflection._roundhouse_key_pairs : nil
      return super if pairs.nil?
      polymorphic = reflection.polymorphic?
      if val.is_a?(Array) || val.is_a?(Relation)
        return polymorphic_composite_list_predicates(reflection, pairs, val.to_a, note) if polymorphic
        return composite_list_predicates(pairs, val.to_a)
      end
      raise NotImplementedError, "roundhouse: where(#{reflection.name}: ...) with the composite key " \
        "#{pairs.map { |pair| pair[0] }.inspect} takes a record, a list of records or nil" unless val.nil? || val.is_a?(Base)
      out = pairs.map do |pair|
        [pair[0], column_predicate(pair[0], val.nil? ? nil : val.public_send(pair[1]), note)]
      end
      # A polymorphic key names its type too (`ledger_type = 'Payments::Account'`),
      # which also seeds the type of a record built through the relation.
      if polymorphic
        type = reflection.foreign_type.to_s
        out.unshift([type, column_predicate(type, val.nil? ? nil : val.class.polymorphic_name, note)])
      end
      out
    end

    # `where(ledger: [records])` over a polymorphic composite key: Rails'
    # PolymorphicArrayValue groups the records by their class and matches
    # each group by the type column and the key pairs, the groups ORed.
    def polymorphic_composite_list_predicates(reflection, pairs, records, note)
      type = reflection.foreign_type.to_s
      groups = records.group_by { |record| record.class.polymorphic_name }.map do |name, group|
        keys = composite_list_predicates(pairs, group)[0][1]
        "(#{column_predicate(type, name, note)} AND #{keys})"
      end
      return [[pairs[0][0], "1=0"]] if groups.empty?
      [[pairs[0][0], groups.length == 1 ? groups[0] : "(#{groups.join(" OR ")})"]]
    end

    # A list of records over a composite key is Rails' tuple match: one
    # AND group of every key pair per record, ORed (an empty list matches
    # nothing).
    def composite_list_predicates(pairs, records)
      groups = records.map do |record|
        "(" + pairs.map { |pair| column_predicate(pair[0], record.public_send(pair[1])) }.join(" AND ") + ")"
      end
      return [[pairs[0][0], "1=0"]] if groups.empty?
      [[pairs[0][0], groups.length == 1 ? groups[0] : "(#{groups.join(" OR ")})"]]
    end
  end
  Relation.prepend(CompositeAssociationKeyPredicates)

  # `where(variants: { inventory_item_variants: { inventory_item_id: 1 } })`:
  # a Hash value under a key that is no column of the nested model nests a
  # further table (Rails' `associated_table` walking the hops), qualified by
  # its own key and typed by the model of that hop. The dynamic walk lives
  # here, outside the typed gate; the leaf conditions are the shared body's.
  module RelationNestedHops
    private

    def nested_conditions(table, owner, conditions, parts, named)
      return super unless conditions.any? { |col, v| v.is_a?(Hash) && nested_hop?(owner, col) }

      conditions.each do |col, v|
        if v.is_a?(Hash) && nested_hop?(owner, col)
          inner, inner_owner = Relation.new(owner).send(:nested_table, col)
          nested_conditions(inner, inner_owner, v, parts, named)
        else
          super(table, owner, { col => v }, parts, named)
        end
      end
    end

    def nested_hop?(owner, col)
      owner.respond_to?(:schema_columns) && !owner.schema_columns.include?(owner._attribute_name(col.to_s).to_sym)
    end
  end
  Relation.prepend(RelationNestedHops)

  # Rails' `check_if_method_has_arguments!` drops blank order arguments
  # (`compact_blank`): `reorder(nil)` clears the order and orders by nothing.
  module RelationBlankOrder
    def order_parts!(parts)
      super(parts.reject { |part| part.nil? || part == "" })
    end
  end
  Relation.prepend(RelationBlankOrder)

  # A PostgreSQL row already carries the Time its driver decoded.
  module RelationDecodedTimes
    def cast_schema_value(value, column)
      return value if value.is_a?(Time) && @model.schema_time_columns.include?(column)

      super
    end
  end
  Relation.prepend(RelationDecodedTimes)

  # Rails' InsertAll takes the relation's create attributes
  # (`attributes.merge!(scope_attributes)`): an association's foreign key
  # and a default scope's `where(island: n)` go into every row of
  # `owner.snapshots.insert_all!(rows)`, over the row's own value. The
  # shared Relation hands the rows to the model as given, which left a
  # default-scoped table's `island` NULL.
  module InsertScopeAttributes
    def insert_all(rows, **options)
      super(_rows_with_scope_attributes(rows), **options)
    end

    def insert_all!(rows, **options)
      super(_rows_with_scope_attributes(rows), **options)
    end

    def upsert_all(rows, **options)
      super(_rows_with_scope_attributes(rows), **options)
    end

    private

    def _rows_with_scope_attributes(rows)
      scope = scope_attributes
      return rows if scope.empty?
      rows.map do |row|
        merged = row.dup
        scope.each do |name, value|
          merged.delete(name.to_s)
          merged[name.to_sym] = value
        end
        merged
      end
    end
  end
  Relation.prepend(InsertScopeAttributes)
end

module ActiveRecord
  # Rails' `sanitize_forbidden_attributes` on a `where` condition: an
  # `ActionController::Parameters` (anything answering `permitted?`) is
  # unwrapped to its Hash when permitted and refused with
  # `ForbiddenAttributesError` when not, so
  # `milestones.where(milestone_params.slice(:name, :value))` filters by
  # column rather than reaching the raw-SQL path as the object's `to_s`.
  module RelationPermittedParametersCondition
    def where(condition = nil, *args)
      if !condition.nil? && condition.respond_to?(:permitted?)
        raise ActiveModel::ForbiddenAttributesError unless condition.permitted?
        condition = condition.to_h
      end
      super(condition, *args)
    end
  end
  Relation.prepend(RelationPermittedParametersCondition)
end

module ActiveRecord
  # Rails 8.2 `ActiveRecord::Batches` (relation/batches.rb): the batch
  # iterators page by the cursor columns (the primary key by default), in
  # `of:` rows per query, each page found after the last row of the one
  # before. CRuby-only: the options are keywords the shared relation's
  # monomorphic `find_each` / `in_batches` do not take, and the shared
  # bodies stay for the strict targets.
  module Batches
    # `in_batches` without a block (Rails' BatchEnumerator): the batches as
    # an Enumerable of relations, with the bulk writes run batch by batch.
    class BatchEnumerator
      include Enumerable

      attr_reader :relation, :of, :start, :finish

      def initialize(of:, start:, finish:, relation:, cursor:, order:, use_ranges:)
        @of = of
        @relation = relation
        @start = start
        @finish = finish
        @cursor = cursor
        @order = order
        @use_ranges = use_ranges
      end

      def each(&block)
        return to_enum(:each) unless block
        @relation.in_batches(of: @of, start: @start, finish: @finish, load: false, cursor: @cursor, order: @order,
                             use_ranges: @use_ranges, &block)
      end

      def each_record
        return to_enum(:each_record) unless block_given?
        @relation.in_batches(of: @of, start: @start, finish: @finish, load: true, cursor: @cursor, order: @order,
                             use_ranges: @use_ranges) do |batch|
          batch.to_a.each { |record| yield record }
        end
      end

      def delete_all
        total = 0
        each { |batch| total += batch.delete_all }
        total
      end

      def update_all(updates)
        total = 0
        each { |batch| total += batch.update_all(updates) }
        total
      end

      def touch_all(*args, **kwargs)
        total = 0
        each { |batch| total += batch.touch_all(*args, **kwargs) }
        total
      end

      def destroy_all
        each(&:destroy_all)
        nil
      end

      def inspect
        "#<#{self.class} #{@relation.inspect} of=#{@of} start=#{@start.inspect} finish=#{@finish.inspect}>"
      end
    end
  end

  module RelationBatches
    DEFAULT_ORDER = :asc

    # `find_each` yields each record, a page at a time. Without a block it
    # is an Enumerator whose size is the rows the bounds leave.
    def find_each(start: nil, finish: nil, batch_size: 1000, error_on_ignore: nil, order: DEFAULT_ORDER, cursor: nil, &block)
      unless block
        return to_enum(:find_each, start: start, finish: finish, batch_size: batch_size, error_on_ignore: error_on_ignore,
                                   order: order, cursor: cursor) do
          batch_bounded(start, finish, order, cursor).size
        end
      end
      find_in_batches(start: start, finish: finish, batch_size: batch_size, error_on_ignore: error_on_ignore, order: order,
                      cursor: cursor) do |records|
        records.each(&block)
      end
      self
    end

    # `find_in_batches` yields each page as an Array of records.
    def find_in_batches(start: nil, finish: nil, batch_size: 1000, error_on_ignore: nil, order: DEFAULT_ORDER, cursor: nil, &block)
      unless block
        return to_enum(:find_in_batches, start: start, finish: finish, batch_size: batch_size,
                                         error_on_ignore: error_on_ignore, order: order, cursor: cursor) do
          total = batch_bounded(start, finish, order, cursor).size
          (total - 1).div(batch_size) + 1
        end
      end
      in_batches(of: batch_size, start: start, finish: finish, load: true, error_on_ignore: error_on_ignore, order: order,
                 cursor: cursor) do |batch|
        yield batch.to_a
      end
      self
    end

    # `in_batches` yields each page as a relation (`load: true` keeps the
    # rows it already read), or answers a BatchEnumerator without a block.
    # `use_ranges: true` yields the page as the id range it spans in place
    # of an IN list of its ids.
    def in_batches(of: 1000, start: nil, finish: nil, load: false, error_on_ignore: nil, order: DEFAULT_ORDER,
                   use_ranges: nil, cursor: nil, &block)
      cursor = batch_cursor(cursor)
      dirs = batch_directions(order, cursor)
      unless block
        return Batches::BatchEnumerator.new(of: of, start: start, finish: finish, relation: self, cursor: cursor,
                                            order: order, use_ranges: use_ranges)
      end
      if !@orders.empty? && error_on_ignore
        raise ArgumentError, "Scoped order is ignored, it's forced to be batch order."
      end
      return batch_loaded_relation(of, start, finish, cursor, dirs, &block) unless @records.nil?
      batch_unloaded_relation(of, start, finish, load, use_ranges, cursor, dirs, &block)
      nil
    end

    protected

    def batch_cursor(cursor)
      cursor = @model.primary_key if cursor.nil?
      names = Array(cursor).map(&:to_s)
      raise ArgumentError, "Primary key not included in the custom select clause" if names.empty?
      names
    end

    def batch_directions(order, cursor)
      dirs = Array(order).map { |one| one.to_s.downcase.to_sym }
      dirs.each do |dir|
        raise ArgumentError, ":order must be :asc or :desc or an array consisting of :asc or :desc, got #{order.inspect}" unless dir == :asc || dir == :desc
      end
      dirs = Array.new(cursor.length, dirs[0]) if dirs.length == 1
      raise ArgumentError, "Cursor and order must have the same length" unless dirs.length == cursor.length
      dirs
    end

    def batch_column(name)
      "#{@table}.#{@model._attribute_name(name)}"
    end

    # The rows from `values` on in the batch order (`side` :after) or up to
    # them (:before), the cursor read lexicographically: each column
    # beyond the equal ones before it, the last one inclusive when asked.
    def batch_condition(cursor, dirs, values, side, inclusive)
      values = [values] unless values.is_a?(Array) && cursor.length > 1
      raise ArgumentError, "Cursor and bound must have the same length" unless values.length == cursor.length
      clauses = []
      cursor.each_index do |i|
        parts = []
        i.times { |j| parts << "#{batch_column(cursor[j])} = #{ActiveRecord.adapter.escape_value(values[j])}" }
        greater = (side == :after) == (dirs[i] == :asc)
        op = greater ? ">" : "<"
        op = "#{op}=" if inclusive && i == cursor.length - 1
        parts << "#{batch_column(cursor[i])} #{op} #{ActiveRecord.adapter.escape_value(values[i])}"
        clauses << "(#{parts.join(" AND ")})"
      end
      clauses.join(" OR ")
    end

    def batch_bounded(start, finish, order, cursor)
      cursor = batch_cursor(cursor)
      dirs = batch_directions(order, cursor)
      relation = spawn
      relation = relation.batch_bound(batch_condition(cursor, dirs, start, :after, true)) unless start.nil?
      relation = relation.batch_bound(batch_condition(cursor, dirs, finish, :before, true)) unless finish.nil?
      relation
    end

    public

    # One more condition on a copy, as SQL written over the cursor columns.
    def batch_bound(sql)
      copy = spawn
      copy.batch_bound!(sql)
    end

    def batch_bound!(sql)
      own_lists
      unload_records
      push_where("(#{sql})")
      self
    end

    # Rows already read for the batch it stands for, as `load: true`
    # hands them on.
    def batch_loaded!(records)
      @records = records
      self
    end

    private

    def batch_unloaded_relation(of, start, finish, load, use_ranges, cursor, dirs)
      batch_limit = of
      remaining = @limit
      batch_limit = remaining if !remaining.nil? && remaining < batch_limit
      # `order` takes bare identifiers, so the table is named unquoted.
      table = @model.table_name.to_s
      qualifier = table.match?(/\A[A-Za-z_][A-Za-z0-9_]*\z/) ? "#{table}." : ""
      order_terms = cursor.each_index.map { |i| "#{qualifier}#{@model._attribute_name(cursor[i])} #{dirs[i] == :asc ? "ASC" : "DESC"}" }
      relation = reorder(*order_terms).limit(batch_limit)
      relation = relation.batch_bound(batch_condition(cursor, dirs, start, :after, true)) unless start.nil?
      relation = relation.batch_bound(batch_condition(cursor, dirs, finish, :before, true)) unless finish.nil?
      by_key = cursor.length == 1 && cursor[0] == Array(@model.primary_key).map(&:to_s).first && !use_ranges
      lower = start.nil? ? nil : batch_condition(cursor, dirs, start, :after, true)
      batch_relation = relation
      loop do
        if load
          records = batch_relation.to_a
          break if records.empty?
          rows = records
          last = cursor.map { |name| records.last[name] }
          last = last[0] if cursor.length == 1
        else
          rows = batch_relation.pluck(*cursor)
          break if rows.empty?
          last = rows.last
        end
        yielded = if by_key
          keys = load ? records.map { |record| record[cursor[0]] } : rows
          spawn.where(@model.primary_key => keys)
        else
          bounded = spawn
          bounded = bounded.batch_bound(lower) unless lower.nil?
          bounded.batch_bound(batch_condition(cursor, dirs, last, :before, true))
        end
        yielded.batch_loaded!(records) if load
        yield yielded
        break if rows.length < batch_limit
        unless @limit.nil?
          remaining -= rows.length
          break if remaining <= 0
          relation = relation.limit(remaining) if remaining < batch_limit
        end
        lower = batch_condition(cursor, dirs, last, :after, false)
        batch_relation = relation.batch_bound(lower)
      end
    end

    # A relation that has its rows already pages over them in memory
    # (Rails' batch_on_loaded_relation).
    def batch_loaded_relation(of, start, finish, cursor, dirs)
      raise NotImplementedError, "roundhouse: in_batches over a loaded relation with mixed :asc and :desc cursor order" unless dirs.uniq.length <= 1
      key = ->(record) { cursor.map { |name| record[name] } }
      records = @records.sort_by(&key)
      records = records.reverse if dirs[0] == :desc
      records = records.select { |record| (key.call(record) <=> Array(start)) != (dirs[0] == :asc ? -1 : 1) } unless start.nil?
      records = records.select { |record| (key.call(record) <=> Array(finish)) != (dirs[0] == :asc ? 1 : -1) } unless finish.nil?
      records = records.first(@limit) unless @limit.nil?
      records.each_slice(of) do |slice|
        keys = slice.map { |record| record[cursor[0]] }
        yield spawn.where(@model.primary_key => keys).batch_loaded!(slice)
      end
      nil
    end
  end
  Relation.prepend(RelationBatches)

  # `Model.find_each` / `find_in_batches` / `in_batches`: the same on the
  # model's relation (Rails delegates them through `all`).
  module BaseBatches
    def find_each(**options, &block)
      all.find_each(**options, &block)
    end

    def find_in_batches(**options, &block)
      all.find_in_batches(**options, &block)
    end

    def in_batches(**options, &block)
      all.in_batches(**options, &block)
    end
  end
  Base.singleton_class.prepend(BaseBatches)

  # Rails 8.2 Relation surface the shared relation leaves out.
  module RelationRailsSurface
    # `count(column)` counts the rows where the column is not NULL
    # (`COUNT(col)`, `COUNT(DISTINCT col)` on a distinct relation); `count`
    # alone and the block form keep the shared relation's answers.
    def count(column_name = nil, &block)
      if block
        raise ArgumentError, "Column name argument is not supported when a block is passed." unless column_name.nil?
        return super(&block)
      end
      return super() if column_name.nil? || column_name.to_s == "all" || column_name.to_s == "*"
      return 0 if @null
      raise NotImplementedError, "roundhouse: #{@model}.group(…).count(#{column_name.inspect}) is not ported" unless @groups.empty?
      expression = count_column_expression(column_name)
      expression = "DISTINCT #{expression}" if @distinct && !expression.match?(/\ADISTINCT\s/i)
      rows = ActiveRecord.adapter.select_rows(append_join_where("#{cte_prefix}SELECT COUNT(#{expression}) AS n FROM #{from_source}"))
      rows.length == 0 ? 0 : rows[0]["n"].to_i
    end

    # Rails' `invert_where` (`where_clause.invert`): the relation's
    # conditions negated as one. With no condition Rails writes `NOT ()`,
    # which PostgreSQL refuses; here it stays no condition. A relation
    # emptied by `where(id: [])` (always false) turns always true.
    def invert_where
      spawn.invert_where!
    end

    def invert_where!
      own_lists
      @null = false
      unload_records
      return self if @wheres.empty?
      clause = "NOT (#{@wheres.join(" AND ")})"
      @wheres = []
      @where_null = []
      @where_parts = {}
      @scope_attributes = {}
      @opaque_wheres = true
      push_where(clause)
      self
    end

    # Rails' `except(*skips)`: the relation without those clauses.
    def except(*skips)
      spawn.except!(*skips)
    end

    def except!(*skips)
      own_lists
      unload_records
      skips.flatten.each { |skip| unscope_clause(skip.to_sym) }
      clear_null_unless_forced
      self
    end

    # Rails' `Relation#reset`: forget the loaded rows.
    def reset
      unload_records
      self
    end

    # `update!(id = :all, attributes)`: `update` with the bang's raises.
    def update!(id = :all, attributes)
      return each { |record| record.update!(attributes) } if id == :all
      scoping do
        if id.is_a?(Array)
          id.map { |one| update!(one, attributes) }
        else
          record = find(id)
          record.update!(attributes)
          record
        end
      end
    end

    def first_or_create(attributes = nil, &block)
      first || create(attributes || {}, &block)
    end

    def first_or_create!(attributes = nil, &block)
      first || create!(attributes || {}, &block)
    end

    private

    # Rails' `aggregate_column`: a column of the model is the table's, a
    # `table.column` or any other text is SQL as written.
    def count_column_expression(column_name)
      text = column_name.to_s
      if text.match?(/\A\w+\z/)
        name = @model._attribute_name(text)
        return "#{@table}.#{name}" if @model.schema_columns.include?(name.to_sym)
      end
      text
    end
  end
  Relation.prepend(RelationRailsSurface)
end
