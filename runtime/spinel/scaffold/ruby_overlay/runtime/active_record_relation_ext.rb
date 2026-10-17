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
      keys.each do |key|
        reflection = @model.reflect_on_association(key.to_sym)
        # A key that is no association of this model may name a hop below
        # the first (`joins(conditions: :owner_market_definition)` then
        # `where.not(owner_market_definition: {...})`), as Rails finds it
        # in its join dependencies.
        alias_nested_join_hop(key.to_sym) if reflection.nil?
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
        @joins[index] = @joins[index].sub(pattern) { aliased }
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
      return composite_list_predicates(pairs, val.to_a) if val.is_a?(Array) || val.is_a?(Relation)
      raise NotImplementedError, "roundhouse: where(#{reflection.name}: ...) with the composite key " \
        "#{pairs.map { |pair| pair[0] }.inspect} takes a record, a list of records or nil" unless val.nil? || val.is_a?(Base)
      pairs.map do |pair|
        [pair[0], column_predicate(pair[0], val.nil? ? nil : val.public_send(pair[1]), note)]
      end
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
end
