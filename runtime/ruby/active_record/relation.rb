# Multi-id finder misses need the pluralizer even without app view helpers.
require_relative "../inflector"

module ActiveRecord
  # A Relation's `includes`/`preload`, deferred until a record asks.
  #
  # `load_records` used to batch-load every included association the
  # moment the rows arrived. On a page whose records only feed a cached
  # collection (campfire's room page: 40 messages, their rich text,
  # creators, boosts and attachments), the cache key needs each record's
  # id and updated_at, and on a hit nothing reads an association at all:
  # the batch loads were most of the page's database time. Now each record
  # carries this object, and the first association read on any of them
  # (an emitted reader calls `_await_preload`) runs the same batched
  # preload for the whole group, once. A miss issues the same queries as
  # before, later; a hit issues none of them.
  class PendingPreload
    def initialize(model, records, specs)
      @model = model
      @records = records
      @specs = specs
      @done = false
    end

    def run
      return nil if @done
      @done = true
      @model.preload_associations(@records, @specs)
      nil
    end
  end

  # A lazy, chainable query builder — the metaprogramming-free analog of
  # ActiveRecord::Relation. Lowered model code drives it: `scope`s become
  # class methods that take/return a Relation, associations return one,
  # and a query chain (`Model.where(...).order(...).limit(...)`) is a
  # sequence of Relation method calls that only touches the database at a
  # terminal (`to_a`/`each`/`first`/`count`/…).
  #
  # No `method_missing`, no `define_method`: every method is written out.
  # The model is held as a plain class-object value (`@model`) whose
  # `_table_sql` / `instantiate` class methods supply the per-model facts;
  # calling them is ordinary dispatch.
  #
  # Database access and value escaping go through `ActiveRecord.adapter`
  # (the `AdapterInterface`) rather than the raw `Db` primitive, so the
  # whole class types against the same adapter contract `Base` uses — no
  # target-specific surface leaks in here. Chain methods mutate and return
  # `self`; lowered chains are linear (build then terminate), so a fresh
  # Relation per chain start is enough isolation.
  #
  # Terminals memoize: the first `to_a` loads and caches the records
  # (Rails' loaded-relation contract), so `map` + `each` + `empty?` on
  # the same relation hit the database once, not once per call — and
  # record mutations made between terminals (lobsters' current_vote
  # stamping) survive to the render. Every chain method drops the cache:
  # app code does re-chain after a terminal (`rel = rel.where(...)`
  # returns this same object, mutated), and a stale cache there would
  # serve the pre-refinement rows.
  class Relation
    # Rails' Relation includes Enumerable and iterates its loaded records
    # (`each` below), so every Enumerable method it does not name itself
    # (`filter_map`, `reject`, `flat_map`, `each_with_index`, `min_by`,
    # `zip`, `tally`, …) answers over the records, as a loaded Array
    # would. The methods written out in this class keep their relation
    # meaning (`select(:id)`, `count`, `sum(:col)`, `first`, `find(id)`,
    # `include?`) and take the Enumerable form only when given a block.
    include Enumerable

    # `default_scoped` false is `Model.unscoped`: the class's
    # `default_scope` (Base#_apply_default_scope) is not applied.
    def initialize(model, default_scoped = true)
      @model = model
      @table = model._table_sql
      @wheres = []
      @where_parts = {}
      @lock = nil
      @joins = []
      @orders = []
      @groups = []
      @havings = []
      @select_sql = nil
      @distinct = false
      @readonly = false
      @limit = nil
      @offset = nil
      @includes = []
      @skip_preloading = false
      # Rails' NullRelation flag: `Model.none` and `where(id: [])` must not
      # hit the database. Chain methods call `unload_records`, which keeps
      # an empty loaded cache when `@null` is set so terminals stay quiet.
      @null = false
      @records = nil
      @scope_attributes = {}
      @create_with = {}
      # Parallel to `@wheres`: true when that clause forced NullRelation
      # (`none` / empty IN). Unscope clears `@null` only when none remain —
      # never by scanning SQL text (a value like title "1=0" must not stick).
      @where_null = []
      @opaque_wheres = false
      @from = nil
      @ctes = []
      @shared_lists = false
      @friendly = false
      @default_scoped = default_scoped
      # Inside a `scoping` block of the model, `Model.all` is that scope
      # (a copy, or merged into an STI subclass's relation) in place of
      # the default scope (scoping/named.rb `all`).
      scope = default_scoped ? model.current_scope : nil
      if scope.nil?
        # An STI subclass's type condition, which `unscoped` keeps.
        model._apply_type_condition(self)
        model._apply_default_scope(self) if default_scoped && model._default_scope_applies?
      else
        model._apply_type_condition(self) unless scope.model.equal?(model)
        inherit_scope(scope)
      end
    end

    # `scope.clone` for `all` under a current scope: its query state and
    # its `extending` modules.
    def inherit_scope(scope)
      scope.where_clauses.each { |w| push_where(w) }
      scope.join_clauses.each { |j| @joins << j }
      scope.order_clauses.each { |o| @orders << o }
      @scope_attributes = scope.scope_attributes
      @default_scoped = scope.default_scoped?
      extensions = []
      meta = scope.singleton_class #: Class
      chain = meta.ancestors #: Array[Module]
      chain.each do |mod|
        break if mod.equal?(scope.class)
        extensions.unshift(mod) unless mod.equal?(meta)
      end
      extensions.each { |mod| extend(mod) }
      self
    end

    def model
      @model
    end

    def default_scoped?
      @default_scoped
    end

    def join_clauses
      @joins
    end

    def order_clauses
      @orders
    end

    # Relation#scoping: the block runs with this relation as the model's
    # current scope (`Model.all`, the class's finders and `unscoped`'s
    # `current_scope` read it), and the previous one is restored after.
    # `all_queries: true` also makes it the model's global scope.
    def scoping(all_queries: nil)
      if @model.global_current_scope(true) && all_queries == false
        raise ArgumentError, "Scoping is set to apply to all queries and cannot be unset in a nested block."
      end
      previous = @model.current_scope(true)
      @model.current_scope = self
      previous_global = nil #: untyped
      if all_queries
        previous_global = @model.global_current_scope(true)
        @model.global_current_scope = self
      end
      begin
        yield
      ensure
        @model.current_scope = previous
        @model.global_current_scope = previous_global if all_queries
      end
    end

    # Rails' `where_values_hash`: the relation's `where` equalities on its
    # own columns, by column name. IN lists, which Rails includes, are
    # not tracked here and are left out.
    def where_values_hash
      out = {}
      @scope_attributes.each { |col, val| out[col.to_s] = val }
      out
    end

    # Rails' `scope_for_create`: the equality-only `where_values_hash`
    # (IN lists left out, as in Rails) with the `create_with` values
    # over it.
    def scope_for_create
      out = where_values_hash
      @create_with.each { |col, val| out[col.to_s] = val }
      out
    end

    # Rails' `create_with(attrs)`: attributes a record built through this
    # relation starts with, winning over the `where` equalities and
    # losing to the attributes the caller gives. A `belongs_to` name is
    # kept as the association assignment it is (`new(service: record)`);
    # `create_with(nil)` resets them.
    def create_with(attrs)
      own_lists
      @create_with = attrs.nil? ? {} : @create_with.merge(attrs.to_h.transform_keys(&:to_sym))
      self
    end

    # Rails' `Relation#unscoped`: the model's relation with nothing
    # applied, its default scope included.
    def unscoped
      ActiveRecord::Relation.new(@model, false)
    end

    # `extending(Mod)`: Mod's methods answer on this relation (and its
    # spawns: `clone` keeps the singleton class), ahead of Relation's own,
    # so an override's `super` reaches the Relation method. A model's
    # `default_scope { extending(Mod) }` puts it on every relation.
    def extending(*modules)
      modules.each { |mod| self.extend(mod) }
      self
    end

    # Rails' `Relation#build` / `create` / `create!`: a record of the
    # model with the relation's scope attributes, under the relation's
    # default scoping (an unscoped relation's record gets no
    # default-scope attributes). Rails' `Relation#new` is this `build`:
    # there is no `new` here (the NO `new` HERE note below explains why),
    # so `lower::default_scope` calls `build` for an `X.unscoped` chain's
    # `new`, and `scope_chain` builds a typed relation's through the
    # model.
    def build(attrs = {})
      seed = scope_attributes
      merged = seed.empty? ? attrs : seed.merge(attrs)
      return @model.new(merged) if @default_scoped
      @model.unscoped { @model.new(merged) }
    end

    def create(attrs = {}, &block)
      record = build(attrs)
      block.call(record) if block
      record.save
      record
    end

    def create!(attrs = {}, &block)
      record = build(attrs)
      block.call(record) if block
      raise RecordInvalid, record unless record.save
      record
    end

    # Drop the loaded-records memo. A null relation keeps `[]` so a later
    # `to_a` / `ids` / `exists?` does not re-query through `(1 = 0)`.
    def unload_records
      @records = @null ? [] : nil
    end

    # Rails NullRelation — set only from `none` / a lasting non-negated
    # empty-IN `where`, never from the pure predicate builder.
    def null_relation?
      @null
    end

    def mark_null!
      @null = true
      unload_records
    end

    # True when a hash condition contains an empty IN list (`id: []`).
    def hash_has_empty_in?(condition)
      condition.each do |_key, val|
        if val.is_a?(Array)
          return true if val.empty?
        elsif val.is_a?(Hash)
          return true if hash_has_empty_in?(val)
        end
      end
      false
    end

    # Rails' `Relation#spawn`: a new relation that shares this one's
    # query state but not its accumulator arrays. Chain methods still
    # mutate in place; scopes call `spawn` on entry so a fork like
    # campfire's sidebar (`visible.with_direct_rooms` beside
    # `visible.with_ordered_room.without_direct_rooms`) does not let
    # one branch's joins/orders/wheres pollute the other.
    #
    # Copy-on-write: `clone` already shares the list objects. Mark both
    # sides shared and copy the lists on the first mutating chain method
    # instead of copying eight arrays on every scope entry. Accumulators
    # are copied element-wise (no `Array#dup` — keep the element type
    # the typer already knows); `@records` is shared until a chain
    # method clears it, matching Rails' loaded-spawn contract.
    def spawn
      @shared_lists = true
      clone
    end

    def own_lists
      if @shared_lists
        @shared_lists = false
        take_query_lists(
          @wheres,
          @where_null,
          @joins,
          @orders,
          @groups,
          @havings,
          @ctes,
          @includes,
          @scope_attributes
        )
      end
      self
    end

    # List accumulators go through copy_* so inference types the
    # parameters. Scalars come from `clone` (shallow ivar copy).
    def take_query_lists(wheres, where_null, joins, orders, groups, havings, ctes, includes, attrs)
      @wheres = copy_string_list(wheres)
      @where_null = copy_bool_list(where_null)
      @joins = copy_string_list(joins)
      @orders = copy_string_list(orders)
      @groups = copy_string_list(groups)
      @havings = copy_string_list(havings)
      @ctes = copy_string_list(ctes)
      @includes = copy_symbol_list(includes)
      @scope_attributes = copy_scope_attributes(attrs)
      self
    end

    def copy_string_list(xs)
      out = []
      xs.each { |x| out << x }
      out
    end

    def copy_bool_list(xs)
      out = []
      xs.each { |x| out << x }
      out
    end

    def copy_symbol_list(xs)
      out = []
      xs.each { |x| out << x }
      out
    end

    def copy_scope_attributes(attrs)
      attrs
    end

    # ---- chain methods (return self) --------------------------------

    # `with_recursive(parents: [base, step])` — a recursive common table
    # expression, each named part the UNION ALL of its relations (Rails
    # 7.1). Rendered ahead of the SELECT; `from("parents")` then reads
    # from it. lobsters walks a comment's ancestors this way on the reply
    # page (`Comment#parents`).
    def with_recursive(ctes)
      own_lists
      unload_records
      ctes.each do |name, parts|
        # Build the UNION list with pushes rather than `map.join`: the
        # body typer's Array#map still returns Untyped, so the join
        # terminal would stay Ty::Var under RBS-seeded typing.
        sql_parts = []
        parts.each { |p| sql_parts << p.bound_sql }
        @ctes << "#{name} AS (#{sql_parts.join(" UNION ALL ")})"
      end
      self
    end

    # `from("parents")` — the FROM source, in place of this model's own
    # table. The String form only: Rails also takes a relation (a
    # subquery) there, which no corpus call writes.
    def from(source)
      unload_records
      @from = source
      self
    end

    # `where(hash)` / `where("raw sql")` / `where("a = ? AND b = ?", x, y)`
    # / `where(["a IN (?)", values])` (Rails' sanitize_sql_array form).
    def where(condition = nil, *args)
      add_condition(condition, args, false, true)
      self
    end

    # An association's scope: `where(fk => owner.id)` that ALSO presets
    # what a `create` through this relation writes. The compiler's
    # association seed uses this so the FK is recorded even when the
    # ordinary `where` path would refuse it as non-seedable.
    def where_scope(condition)
      @scope_attributes = condition
      add_condition(condition, [], false)
      self
    end

    # Rails' `scope_for_create`: the attributes a record built through
    # this relation starts with. Empty for a relation nobody scoped.
    def scope_attributes
      @create_with.empty? ? @scope_attributes : @scope_attributes.merge(@create_with)
    end

    # `where.not(...)` is lowered to `not(...)` on the relation: negate the
    # condition back onto this relation. Negated predicates never seed.
    def not(condition = nil, *args)
      add_condition(condition, args, true)
      self
    end

    # `where.missing(:assoc)` — WhereChain#missing: rows with no
    # associated record. `where` with no condition answers the relation
    # itself (the chain methods, `not` above among them, live here), so
    # the hop lands on this method. A LEFT OUTER JOIN to the association
    # and a NULL key on its table, as Rails writes it.
    def missing(*associations)
      associations.each do |name|
        reflection = reflect_join_hop(@model, name)
        left_outer_joins(name)
        push_where("(#{reflection.klass._table_sql}.#{reflection_key_column(reflection)} IS NULL)")
      end
      self
    end

    # `where.associated(:assoc)` — WhereChain#associated: rows with an
    # associated record. An INNER JOIN and a non-NULL key on its table.
    def associated(*associations)
      associations.each do |name|
        reflection = reflect_join_hop(@model, name)
        joins(name)
        push_where("NOT (#{reflection.klass._table_sql}.#{reflection_key_column(reflection)} IS NULL)")
      end
      self
    end

    # The column `missing` / `associated` test for NULL: the target's
    # primary key (the association primary key of a belongs_to).
    def reflection_key_column(reflection)
      key = reflection.belongs_to? ? reflection.association_primary_key : reflection.klass.primary_key
      key = key.first if key.is_a?(Array)
      key.to_s
    end

    # Rails' `excluding(…)` — everything in this relation except what is
    # named. Rails accepts records, arrays of records, and relations, and
    # all three already have a spelling here: `column_predicate` reads a
    # `Base` as its id, an `Array` as an IN list, and a `Relation` as an
    # IN subquery. So this is a negated primary-key condition and nothing
    # more — the polymorphism it looks like it needs is the polymorphism
    # `where` already carries.
    #
    # The one-argument case unwraps rather than passing the splat array
    # through: `excluding(some_relation)` has to reach the SUBQUERY
    # branch, and wrapped in an array it would reach the IN-list branch
    # and escape a Relation object into the SQL text.
    def excluding(*records)
      val = records.length == 1 ? records[0] : records
      own_lists
      unload_records
      # Empty exclusion is a no-op (Rails). Never mark_null!: `NOT (1=0)`
      # is always true — sticky null would invert the relation.
      return self if val.is_a?(Array) && val.empty?
      pred = column_predicate(@model.primary_key.to_s, val)
      push_where("NOT (#{pred})") unless pred.empty?
      self
    end

    # `without` — Rails' own alias for `excluding`, on both Relation and
    # Enumerable. campfire's sidebar splits its memberships in two and
    # writes `all_memberships.without(@direct_memberships)` for the
    # remainder.
    def without(*records)
      excluding(*records)
    end

    # `rel.or(other)` — Rails' Relation#or: this relation's accumulated
    # WHERE conjunction OR'd with the other's, grouped as one condition.
    # Rails requires matching structure (joins/limit) on both sides;
    # the receiver's structural clauses are kept here. An empty side is
    # an always-true condition (matches Rails: no filter to OR against).
    def or(other)
      # Rails NullRelation#or: left-null yields the other side; right-null
      # (including both-null) leaves the receiver.
      return other.spawn if @null && !other.null_relation?
      return self if other.null_relation?
      unload_records
      mine = @wheres.length > 0 ? @wheres.join(" AND ") : "1=1"
      other_wheres = other.where_clauses
      theirs = other_wheres.length > 0 ? other_wheres.join(" AND ") : "1=1"
      @opaque_wheres = true
      @wheres = ["((#{mine}) OR (#{theirs}))"]
      @where_null = [false]
      self
    end

    # `and(other)` — the receiver's conditions AND the other relation's
    # (relation/query_methods.rb's `and!`: `where_clause.and`). Rails
    # requires structurally compatible relations; as for `or`, the
    # receiver's structural clauses are kept.
    def and(other)
      own_lists
      unload_records
      other.where_clauses.each { |w| push_where(w) }
      self
    end

    # Answers whether it actually PUSHED a clause — a nil or empty
    # condition pushes none. `find_by` is the caller that needs the
    # answer: it borrows a predicate and gives it back, so it has to
    # know whether there is one to pop. The chain methods ignore it.
    #
    # `note` (`where`, not `find_by`'s borrowed predicate): the hash's
    # equalities also seed records built through the relation
    # (`note_equality`).
    def add_condition(condition, args, negate, note = false)
      own_lists
      unload_records
      return false if condition.nil?
      parts = nil
      sql = if condition.is_a?(Hash)
        built = hash_conditions(condition, note)
        parts = @hash_parts
        built
      elsif condition.is_a?(Array)
        substitute_binds(condition[0].to_s, condition.drop(1) + args)
      else
        substitute_binds(condition.to_s, args)
      end
      return false if sql == ""
      clause = negate ? "NOT (#{sql})" : "(#{sql})"
      clause = negate ? "NOT (#{sql})" : "(#{sql})"
      empty_in = !negate && condition.is_a?(Hash) && hash_has_empty_in?(condition)
      push_where(clause, empty_in)
      # A Hash's equalities are separate predicates in Rails, which
      # `unscope(where:)` removes one by one. Reassigned, not written
      # into: a spawned relation shares the Hash.
      @where_parts = @where_parts.merge({ clause => parts }) unless parts.nil?
      # Empty IN is NullRelation only on a lasting positive where — never
      # under `not` / temporary find_by·exists? (those restore via caller).
      mark_null! if empty_in
      true
    end

    # `@wheres` and `@where_null` are parallel lists: `@where_null[i]` is
    # true when clause i forced NullRelation. Every push and pop goes
    # through these so the lists cannot drift apart.
    def push_where(sql, null_clause = false)
      @wheres << sql
      @where_null << null_clause
      self
    end

    def pop_where
      @where_null.pop
      @wheres.pop
    end

    # `order` on a LOADED relation sorts the loaded records in memory
    # rather than dropping them and re-querying. This is the shape a
    # preloaded association takes through a scope: campfire's message
    # partial renders `message.boosts.ordered` for every message on the
    # page, and with `includes(boosts: :booster)` already loaded those
    # boosts, re-querying them was one round trip per message -- the room
    # page's last N+1. (Rails re-queries here too, and hides it behind
    # its fragment cache; a per-request renderer cannot.)
    #
    # Only a term that names a bare column, optionally with a table
    # prefix and ASC/DESC, sorts in memory; anything else (an expression,
    # a function, RANDOM()) drops the memo and the SQL path answers. The
    # sort is STABLE and applies the terms last-to-first, so ties under
    # the first term keep the loaded order -- which is the table's row
    # order, the same order SQLite hands back for tied keys -- and a
    # page sorted here matches one sorted by the database byte for byte.
    def order(*parts)
      own_lists
      terms = parts.map { |p| order_term(p) }
      terms.each { |t| @orders << t }
      loaded = @records
      return self if loaded.nil?
      # An explicit copy rather than `dup`: built by pushes, the copy has
      # the records array's own type where a `dup` of a nilable read does
      # not, and it is assigned back into `@records` below. An index loop
      # rather than `each`, whose block parameter the typer cannot derive
      # from a receiver it only knows as nilable.
      sorted = []
      i = 0
      while i < loaded.length
        sorted << loaded[i]
        i += 1
      end
      if sort_in_place!(sorted, terms)
        @records = sorted
      else
        unload_records
      end
      self
    end

    # `order(Arel.sql("…"))` — a caller-authored ordering fragment, kept
    # as written (Rails takes an `Arel.sql` literal past its column-name
    # check; campfire's `reorder(Arel.sql("+messages.created_at"))`
    # keeps SQLite off an index). `lower::arel_sql_order` renames the
    # call; the bare String `order` still validates. A loaded relation
    # reads again rather than guess at a fragment's sort.
    def order_sql(fragment)
      own_lists
      @orders << fragment
      unload_records
      self
    end

    def reorder_sql(fragment)
      @orders = []
      order_sql(fragment)
    end

    # Rails requires at least one argument to the public `reorder` API.
    # Aggregate calculations need to discard ordering internally; keep this
    # operation distinct from the public, argument-requiring `reorder` API.
    def clear_ordering
      @orders = []
      @records = nil
      self
    end

    # `reorder(*parts)` — Rails' "replace the ordering": drop every term
    # gathered so far, then order by these. Same in-memory resort as
    # `order` when the records are already loaded.
    def reorder(*parts)
      raise ArgumentError, "The method .reorder() must contain arguments." if parts.empty?
      @orders = []
      order(*parts)
    end

    # `lock` / `lock(false)` / `lock("FOR SHARE")` — the row-locking
    # clause closing the SELECT (query_methods.rb `lock!`: a String is
    # the clause as written, true or nil is `FOR UPDATE`, anything else
    # removes it; arel's visitor writes it after LIMIT and OFFSET).
    def lock(locks = true)
      @records = nil
      @lock = if locks.is_a?(String)
        locks
      elsif locks == true || locks.nil?
        "FOR UPDATE"
      end
      self
    end

    # What `unscope(:name)` may name (query_methods.rb
    # VALID_UNSCOPING_VALUES).
    UNSCOPING_VALUES = [
      :where, :select, :group, :order, :lock, :limit, :offset, :joins, :left_outer_joins,
      :annotate, :includes, :eager_load, :preload, :from, :readonly, :having, :optimizer_hints, :with
    ].freeze
    private_constant :UNSCOPING_VALUES

    # `unscope(:order, where: :col, …)` — drop a clause of the query
    # (query_methods.rb `unscope!`). A Symbol names the clause to reset;
    # `where:` takes the columns whose conditions go, whatever their
    # predicate (equality, IN, NOT …).
    def unscope(*args)
      raise ArgumentError, "The method .unscope() must contain arguments." if args.empty?
      own_lists
      unload_records
      valid_scopes = UNSCOPING_VALUES #: Array[Symbol]
      args.flatten.each do |scope|
        if scope.is_a?(Symbol)
          scope = :left_outer_joins if scope == :left_joins
          unless valid_scopes.include?(scope)
            raise ArgumentError, "Called unscope() with invalid unscoping argument ':#{scope}'. Valid arguments are :#{valid_scopes.join(", :")}."
          end
          unscope_clause(scope)
        elsif scope.is_a?(Hash)
          scope.each do |key, target|
            raise ArgumentError, "Hash arguments in .unscope(*args) must have :where as the key." if key != :where
            unscope_where_columns(target.is_a?(Array) ? target : [target])
          end
        else
          raise ArgumentError, "Unrecognized scoping: #{args.inspect}. Use .unscope(where: :attribute_name) or .unscope(:order), for example."
        end
      end
      clear_null_unless_forced
      # Retained null must keep the empty cache (`[]`), not `nil`, or a
      # limited `size` path would query again.
      unload_records
      self
    end

    # NullRelation clears when no forcing clause remains. Tracked
    # structurally through `@where_null` — never by searching SQL text for
    # `1=0`, so a value like title "1=0" cannot stick it.
    def clear_null_unless_forced
      return nil unless @null
      forced = false
      i = 0
      while i < @where_null.length
        forced = true if @where_null[i]
        i += 1
      end
      @null = false unless forced
      nil
    end
    private :clear_null_unless_forced

    def unscope_clause(scope)
      case scope
      when :where
        @wheres = []
        @where_null = []
        @where_parts = {}
        @scope_attributes = {}
        @opaque_wheres = false
      when :select then @select_sql = nil
      when :group then @groups = []
      when :order then @orders = []
      when :lock then @lock = nil
      when :limit then @limit = nil
      when :offset then @offset = nil
      when :joins, :left_outer_joins then @joins = []
      when :includes, :eager_load, :preload then @includes = []
      when :from then @from = nil
      when :readonly then @readonly = false
      when :having then @havings = []
      when :with then @ctes = []
      end
      nil
    end
    private :unscope_clause

    # `where_clause.except(*columns)`: a hash condition's predicate goes
    # when it is on one of `columns` (the attribute name, on any table); a
    # negated one only when every predicate in it is. Raw-SQL clauses are
    # never touched, as in Rails.
    def unscope_where_columns(columns)
      if @opaque_wheres
        raise NotImplementedError, "unscope(where:) after merge or or: the merged conditions are not attributed to columns here, so they would silently survive"
      end
      names = columns.map { |c| unscope_column_name(c.to_s) }
      kept = []
      kept_null = []
      parts_left = {}
      @wheres.each_with_index do |clause, idx|
        parts = @where_parts[clause]
        if parts.nil?
          kept << clause
          kept_null << @where_null[idx]
          next
        end
        remaining = parts.reject { |part| names.include?(unscope_column_name(part[0])) }
        if remaining.length == parts.length || (clause.start_with?("NOT (") && remaining.length > 0)
          kept << clause
          kept_null << @where_null[idx]
          parts_left[clause] = parts
        elsif remaining.length > 0
          rebuilt = "(#{remaining.map { |part| part[1] }.join(" AND ")})"
          kept << rebuilt
          # An empty IN renders `1=0`; the clause is still forcing when
          # that predicate is one of those left.
          kept_null << (@where_null[idx] && remaining.any? { |part| part[1] == "1=0" })
          parts_left[rebuilt] = remaining
        end
      end
      @wheres = kept
      @where_null = kept_null
      @where_parts = parts_left
      @scope_attributes = @scope_attributes.reject { |col, _| names.include?(col.to_s) }
      nil
    end
    private :unscope_where_columns

    # A column as `unscope` compares it: the bare attribute name, whatever
    # table it was written against (`"comments.col"`, `comments: { col: }`).
    # Rails matches by attribute name on any table, so a joined table's
    # predicate on the same column goes too.
    def unscope_column_name(name)
      return name unless name.include?(".")
      name.split(".").last.to_s
    end
    private :unscope_column_name

    # Rails' mutating spellings. This Relation's chain methods already
    # mutate in place and return self, so the bang forms are the same
    # operation under a second name (bodies duplicated rather than
    # splat-forwarded — strict targets inline, not forward, rest args).
    def where!(condition = nil, *args)
      add_condition(condition, args, false, true)
      self
    end

    def order!(*parts)
      own_lists
      terms = parts.map { |p| order_term(p) }
      terms.each { |t| @orders << t }
      loaded = @records
      return self if loaded.nil?
      # An explicit copy rather than `dup`: built by pushes, the copy has
      # the records array's own type where a `dup` of a nilable read does
      # not, and it is assigned back into `@records` below. An index loop
      # rather than `each`, whose block parameter the typer cannot derive
      # from a receiver it only knows as nilable.
      sorted = []
      i = 0
      while i < loaded.length
        sorted << loaded[i]
        i += 1
      end
      if sort_in_place!(sorted, terms)
        @records = sorted
      else
        unload_records
      end
      self
    end

    # Sort `sorted` (a copy of the loaded records) by `terms` in place;
    # true when every term was a bare column, false when one was not --
    # the caller then drops the memo and the database orders instead.
    # Stable insertion sort: the memo is a page, not a table, and
    # stability is the property that keeps tied rows in database order.
    def sort_in_place!(sorted, terms)
      i = terms.length - 1
      while i >= 0
        col = order_column(terms[i])
        return false if col.nil?
        desc = order_descending?(terms[i])
        # Keys once per row, not once per comparison: `attributes` builds
        # a Hash on every call.
        keys = sorted.map { |r| order_key_of(r, col) }
        n = sorted.length
        j = 1
        while j < n
          moving = sorted[j]
          key = keys[j]
          k = j - 1
          while k >= 0
            cmp = compare_order_keys(keys[k], key)
            return false if cmp.nil?
            cmp = -cmp if desc
            break if cmp <= 0
            sorted[k + 1] = sorted[k]
            keys[k + 1] = keys[k]
            k -= 1
          end
          sorted[k + 1] = moving
          keys[k + 1] = key
          j += 1
        end
        i -= 1
      end
      true
    end

    # One row's value under `col`, as the database would order it. The
    # synthesized `_attribute_snapshot` hash carries every column BUT the primary
    # key, and datetimes as their raw stored text (which orders as the
    # database orders them); `id` is read off the record itself.
    def order_key_of(record, col)
      return record.id if col == "id"
      record._attribute_snapshot[col]
    end

    # The bare column an order term names -- `"created_at"` and
    # `"boosts.created_at DESC"` both answer `"created_at"` -- or nil for
    # anything that is not one column with an optional ASC/DESC.
    def order_column(term)
      words = term.strip.split(/\s+/)
      return nil if words.length == 0 || words.length > 2
      col = words[0]
      dot = col.rindex(".")
      col = col[(dot + 1)..] unless dot.nil?
      return nil unless col.match?(/\A[A-Za-z_][A-Za-z0-9_]*\z/)
      if words.length == 2
        dir = words[1].upcase
        return nil unless dir == "ASC" || dir == "DESC"
      end
      col
    end

    # Whether an order term (already accepted by `order_column`) says DESC.
    def order_descending?(term)
      words = term.strip.split(/\s+/)
      words.length == 2 && words[1].upcase == "DESC"
    end

    # Flip ASC/DESC so `last_n` can LIMIT the tail in SQL. A term with
    # no direction is ASC (SQLite and Rails). `order_term` joins a Hash
    # into one comma-separated string (`"a ASC, b DESC"`), and a raw
    # `"created_at DESC, id DESC"` is stored as one `@orders` entry, so
    # each comma-separated fragment is reversed on its own.
    def reverse_order_term(term)
      t = term.strip
      out = ""
      start = 0
      i = 0
      n = t.length
      while i <= n
        comma = false
        comma = true if i < n && t[i] == ","
        if i == n || comma
          part = t[start, i - start].to_s.strip
          if part.length > 0
            out = "#{out}, " if out.length > 0
            out = "#{out}#{reverse_one_order_term(part)}"
          end
          start = i + 1
        end
        i += 1
      end
      out
    end

    def reverse_one_order_term(term)
      t = term.strip
      upper = t.upcase
      if upper.end_with?(" DESC")
        "#{t[0, t.length - 5]} ASC"
      elsif upper.end_with?(" ASC")
        "#{t[0, t.length - 4]} DESC"
      else
        "#{t} DESC"
      end
    end

    # SQLite's ordering of two attribute values: NULL sorts first, then
    # like compares with like. nil for a pair this cannot order, which
    # sends the caller back to the database.
    def compare_order_keys(a, b)
      return 0 if a.nil? && b.nil?
      return -1 if a.nil?
      return 1 if b.nil?
      a <=> b
    end

    # The value is a request param as often as a literal
    # (`limit(params[:per_page])`), and it is spliced into the SQL, so
    # both pass through Rails' own casts here: `limit` is
    # `sanitize_limit` (`Integer()`, which raises on anything that is
    # not an integer) and `offset` is `build_arel`'s `to_i`. nil clears
    # either, as in Rails.
    def limit(n)
      unload_records
      # Split rather than `n.nil? ? nil : sql_limit(n)`: Spinel cannot
      # unify nil with Integer in a conditional expression.
      if n.nil?
        @limit = nil
      else
        @limit = sql_limit(n)
      end
      self
    end

    def offset(n)
      unload_records
      if n.nil?
        @offset = nil
      else
        @offset = n.to_i
      end
      self
    end

    # ---- page / per / paginate: LIMIT / OFFSET arithmetic ------------
    #
    # The catalog types `page`, `per`, and `paginate` as builders and
    # the readers below as terminals. `count` already leaves LIMIT and
    # OFFSET out of its SQL (`count_sql`), which is the unpaginated
    # total `total_count` needs. Not modeled: `padding`, `without_count`,
    # `max_per_page` / `max_pages`, and wrapping a loaded Array.
    #
    # The readers go through locals rather than doing arithmetic on the
    # ivars: a runtime ivar reads as `T | Nil` (see `ActionController::
    # Page`).

    # `page(n)`: page `n` at the app's default page size. A nil, blank,
    # non-numeric or non-positive `n` is page 1 (`to_i`).
    def page(num = nil)
      per_page = Rails.application.pagination_default_per_page
      n = num.to_s.to_i
      n = 1 if n < 1
      limit(per_page)
      offset((n - 1) * per_page)
    end

    # Same builder as `page` under the `paginate` spelling. A
    # positional page number, or `page:` / `per_page:` keywords.
    # `per(nil)` is a no-op (`per` only applies a numeric string).
    def paginate(num = nil, page: nil, per_page: nil)
      self.page(page || num).per(per_page)
    end

    # `per(n)`: the same page at `n` rows. A nil, blank or negative `n`
    # (the `/^\d/` test) leaves the relation as it is, so
    # `per(params[:per])` without the parameter keeps the default size;
    # `per(0)` is `limit(0)`.
    def per(num)
      text = num.to_s
      return self unless text.match?(/\A\d/)
      n = text.to_i
      return limit(0) if n == 0
      page_now = current_page
      limit(n)
      offset((page_now - 1) * n)
    end

    def limit_value
      @limit
    end

    def offset_value
      @offset
    end

    # 1 for a relation that was never paged (divide-by-nil limit);
    # `per(0)` raises ZeroDivisionError.
    def current_page
      per_page = @limit
      return 1 if per_page.nil?
      raise ZeroDivisionError, "Current page was incalculable. Perhaps you called .per(0)?" if per_page == 0
      skipped = @offset
      skipped = 0 if skipped.nil? || skipped < 0
      skipped / per_page + 1
    end

    def total_count
      count
    end

    # Rounded up; 0 for an empty relation, which makes page 1 of
    # nothing out of range rather than the last page. A relation that
    # was never paged is one page.
    def total_pages
      per_page = @limit
      return 1 if per_page.nil?
      raise ZeroDivisionError, "Total pages was incalculable. Perhaps you called .per(0)?" if per_page == 0
      (total_count + per_page - 1) / per_page
    end

    def first_page?
      current_page == 1
    end

    # Loaded non-empty short page cannot have a successor — skip COUNT.
    # Empty pages are ambiguous (page 1 of nothing vs. out of range).
    def last_page?
      per_page = @limit
      return true if per_page.nil?
      raise ZeroDivisionError, "Total pages was incalculable. Perhaps you called .per(0)?" if per_page == 0
      r = @records
      return true if !r.nil? && r.length > 0 && r.length < per_page
      current_page == total_pages
    end

    def out_of_range?
      current_page > total_pages
    end

    def next_page
      return nil if last_page? || out_of_range?
      current_page + 1
    end

    def prev_page
      return nil if first_page? || out_of_range?
      current_page - 1
    end

    def group(*parts)
      own_lists
      unload_records
      # Symbols qualify against this relation's table (Rails renders
      # `GROUP BY "tags"."id"`), so a grouped column stays unambiguous
      # once a join brings in a second table carrying the same column
      # name. Raw strings (expressions, pre-qualified columns) ride
      # verbatim.
      parts.each do |p|
        @groups << (p.is_a?(Symbol) ? "#{@table}.#{p}" : p.to_s)
      end
      self
    end

    def having(condition, *args)
      own_lists
      unload_records
      @havings << substitute_binds(condition.to_s, args)
      self
    end

    # `joins("INNER JOIN memberships ON …")` — a raw SQL fragment, which
    # is what reaches this runtime. The ASSOCIATION form
    # (`joins(:users)`) is resolved at transpile time by
    # `lower::scope_chain`, which owns the only table that knows a
    # join's ON clause; a Symbol arriving here means that lowering
    # DECLINED, and appending the bare name produces `FROM rooms users`
    # — which SQLite reads as an alias, so the query runs and answers
    # the wrong rows.
    #
    # `AssocRegistry`'s own doc says an unresolvable shape is "left
    # untouched (visible at runtime rather than silently mis-joined)".
    # It was not visible: this method made it silent. Raising is what
    # that sentence always meant, and it names the association so the
    # ledger line and the failure agree.
    #
    # Found via `Rooms::Direct.all.joins(:users)` — an STI subclass is
    # not in the registry, so nothing could resolve it.
    # An identical join is added ONCE, as Rails' `joins_values` are
    # uniq'd: two scopes that each `joins(:story)` — lobsters'
    # `on_stories_not_authored_by.above_average` — render one INNER JOIN,
    # where appending both made SQLite reject every column of the joined
    # table as ambiguous.
    def joins(spec)
      own_lists
      unload_records
      frag = join_fragment(spec)
      @joins << frag unless @joins.include?(frag)
      self
    end

    def left_outer_joins(spec)
      own_lists
      unload_records
      frag = join_fragment(spec, "LEFT OUTER JOIN")
      @joins << frag unless @joins.include?(frag)
      self
    end

    # The SQL of a `joins` / `left_outer_joins` argument. A String is
    # the author's SQL. An association spec the lowering did not expand
    # (the receiver's model is only known here, as in `base.left_joins(
    # :payment_credentials)` on a relation built from a variable) is
    # resolved against the model's reflections, as JoinDependency does:
    # a Symbol is one hop, a Hash (`joins(user: :memberships)`) joins the
    # nested hops off the hop's target, an Array is several.
    #
    # A hop the reflections cannot express raises naming it: a scoped or
    # polymorphic association, a habtm, an STI target, a self join
    # (Rails aliases its table) and a model answering no reflections.
    # Appending a guess would answer the wrong rows.
    def join_fragment(spec, kind = "INNER JOIN")
      return spec if spec.is_a?(String)
      reflected_join_sql(@model, spec, kind)
    end

    def reflected_join_sql(owner, spec, kind)
      case spec
      when String
        spec
      when Symbol
        reflection_join_sql(owner, reflect_join_hop(owner, spec), kind)
      when Array
        spec.map { |el| reflected_join_sql(owner, el, kind) }.join(" ")
      when Hash
        spec.map do |name, nested|
          reflection = reflect_join_hop(owner, name)
          head = reflection_join_sql(owner, reflection, kind)
          "#{head} #{reflected_join_sql(reflection.klass, nested, kind)}"
        end.join(" ")
      else
        raise ArgumentError, "joins(#{spec.inspect}): not an association name, hash, array or SQL string"
      end
    end

    def reflect_join_hop(owner, name)
      reflection = owner.respond_to?(:reflect_on_association) ? owner.reflect_on_association(name.to_sym) : nil
      return reflection unless reflection.nil?
      raise ArgumentError, "joins(:#{name}): #{owner} has no association #{name} to join, or answers no reflections " \
        "(reflect_on_association is emitted where the app calls it)"
    end

    # JoinDependency's join for one reflection: a `through` is its
    # through hop then its source hop off the through model, every hop
    # carrying `kind`.
    def reflection_join_sql(owner, reflection, kind)
      reflection.check_eager_loadable! if reflection.respond_to?(:check_eager_loadable!)
      if reflection.through_reflection?
        through = reflection.through_reflection
        source = reflection.source_reflection
        if through.nil? || source.nil?
          raise ArgumentError, "joins(:#{reflection.name}): the through association of #{owner} has no through or source reflection"
        end
        first = reflection_join_sql(owner, through, kind)
        return "#{first} #{reflection_join_sql(through.klass, source, kind)}"
      end
      name = "joins(:#{reflection.name})"
      if reflection.macro == :has_and_belongs_to_many
        raise NotImplementedError, "roundhouse: #{name} on #{owner}: a has_and_belongs_to_many join through its implicit join table is not resolved at runtime"
      end
      if reflection.polymorphic?
        raise ArgumentError, "#{name} on #{owner}: a polymorphic belongs_to cannot be joined (Rails raises EagerLoadPolymorphicError)"
      end
      reflection.options # raises by name for a scope or option the compiler did not carry
      target = reflection.klass
      if target._type_conditioned?
        raise NotImplementedError, "roundhouse: #{name} on #{owner}: the target #{target} is an STI subclass; its type condition is not resolved at runtime"
      end
      table = target._table_sql
      own = owner._table_sql
      if table == own
        raise NotImplementedError, "roundhouse: #{name} on #{owner}: a self join needs a table alias, which is not resolved at runtime"
      end
      belongs = reflection.belongs_to?
      theirs = join_key_list(belongs ? reflection.association_primary_key : reflection.foreign_key)
      ours = join_key_list(belongs ? reflection.foreign_key : reflection.active_record_primary_key)
      conditions = []
      index = 0
      while index < theirs.length
        conditions << "#{table}.#{theirs[index]} = #{own}.#{ours[index]}"
        index += 1
      end
      type = reflection.type
      unless type.nil?
        conditions << "#{table}.#{type} = '#{owner.polymorphic_name.to_s.gsub("'", "''")}'"
      end
      "#{kind} #{table} ON #{conditions.join(" AND ")}#{target._join_scope_sql}"
    end

    # A reflection's key is one column or an Array of them (composite).
    def join_key_list(key)
      key.is_a?(Array) ? key.map { |k| k.to_s } : [key.to_s]
    end

    # `left_joins` — Rails alias for `left_outer_joins`.
    def left_joins(spec)
      left_outer_joins(spec)
    end

    # `select(:id, :username, "raw AS x")` — the PROJECTION, and the
    # only thing this name means here. Symbols qualify against this
    # relation's table (as Rails renders them); raw strings ride
    # verbatim.
    #
    # WITH A BLOCK Rails means a different method sharing the name:
    # Enumerable's filter over the loaded records, answering an Array.
    # That form lives on `filter` below, and `relation_select_block`
    # lowers every relation-typed `select { … }` call site onto it — so
    # this method answers a Relation and nothing else.
    #
    # It is worth saying why the fork is a lowering rather than a
    # `specs.empty?` branch here, which is what it used to be. A method
    # answering `Relation | Array` types its whole receiver chain POLY,
    # and on the strict targets poly is not merely the slow path: it is
    # a DIFFERENT dispatch path. spinel's does not apply the
    # braceless-keyword-args → trailing-positional-Hash conversion, so
    # `Story.select(:id).where(merged_story_id: id)` bound `where`'s
    # optional `condition` to its `nil` default and dropped the filter
    # on the floor — lobsters' `/s/:story_id` rendered the comments of
    # every merged story instead of its own, with no warning anywhere.
    #
    # Zero specs therefore means the lowering did not fire, which is a
    # compiler bug and not a shape to guess at: the old branch's failure
    # mode (`@select_sql` an EMPTY string, so the next hop emitted
    # `SELECT  FROM memberships …`) was silent until something forced
    # the query. Raise instead.
    def select(*specs, &block)
      return filter(&block) if block
      raise ArgumentError, "select: no columns (a block form reached the projection)" if specs.empty?
      unload_records
      cols = []
      specs.each do |spec|
        cols << (spec.is_a?(Symbol) ? "#{@table}.#{spec}" : spec.to_s)
      end
      @select_sql = cols.join(", ")
      self
    end

    def distinct
      unload_records
      @distinct = true
      self
    end

    # Rails' `readonly(value = true)`: the records the relation loads are
    # marked readonly (`readonly!`), so saving or destroying one raises
    # ReadOnlyRecord. Core's `readonly_has_many` scopes its association
    # with it (`build_readonly_scope`).
    def readonly(value = true)
      @records = nil
      @readonly = value
      self
    end

    # `includes`/`preload`/`eager_load` — eager-load hints. The specs
    # (Symbols, or Hashes for nested includes like `story: :user`) are
    # recorded here and executed by `to_a`, which hands them to the
    # model's synthesized `preload_associations` (batched `IN` loads
    # into the `_preload_<assoc>` caches). Models without a synthesized
    # override inherit Base's no-op and stay lazy (correct, just N+1).
    def includes(*names)
      own_lists
      unload_records
      names.each { |n| @includes << n }
      self
    end

    def preload(*names)
      own_lists
      unload_records
      names.each { |n| @includes << n }
      self
    end

    # `skip_preloading!` — load the rows without running the recorded
    # `includes`/`preload` specs. The specs stay on the relation, so a
    # caller can apply them later, to just the records it needs, with
    # `preload_associations`. campfire's message pages load this way and
    # preload only the messages its fragment cache missed.
    def skip_preloading!
      unload_records
      @skip_preloading = true
      self
    end

    # `preload_associations(records)` — run this relation's recorded
    # preload specs against `records`, loaded here or anywhere else, the
    # batched `IN` loads `load_records` would have run on its own rows.
    def preload_associations(records)
      @model.preload_associations(records, @includes) if @includes.length > 0
      records
    end

    def eager_load(*names)
      own_lists
      unload_records
      names.each { |n| check_eager_loadable(@model, n) }
      names.each { |n| @includes << n }
      self
    end

    # An eager load joins each hop, which Rails refuses for a scope that
    # takes the owner (reflection.rb `check_eager_loadable!`). Hops the
    # model answers no reflection for are left to the preloader.
    def check_eager_loadable(owner, spec)
      case spec
      when Symbol, String
        reflection = owner.respond_to?(:reflect_on_association) ? owner.reflect_on_association(spec.to_sym) : nil
        reflection.check_eager_loadable! if reflection.respond_to?(:check_eager_loadable!)
        reflection
      when Array
        spec.each { |el| check_eager_loadable(owner, el) }
      when Hash
        spec.each do |name, nested|
          reflection = check_eager_loadable(owner, name)
          check_eager_loadable(reflection.klass, nested) if reflection.respond_to?(:klass)
        end
      end
    end

    # `references(:assoc)` — Rails' marker that a string condition
    # mentions an eager-loaded table. The preload machinery here decides
    # what to load from `eager_load`/`includes` alone, so the marker
    # carries no state; accept and ignore it so marker-bearing chains
    # stay chainable (lobsters' filters page).
    def references(*names)
      names
      self
    end

    # `merge(other)` — fold another relation's WHEREs in. v1 handles the
    # common case (merging a same-table scope's conditions).
    def merge(other)
      own_lists
      unload_records
      other.where_clauses.each do |w|
        push_where(w)
        @opaque_wheres = true
      end
      mark_null! if other.null_relation?
      self
    end

    def where_clauses
      @wheres
    end

    # `rel.arel` — the relation reified as its SELECT text, for the
    # `.arel.exists` correlated-subquery idiom
    # (`where.not(HiddenStory.….arel.exists)`). Captures the SQL at the
    # call: later chain mutations don't flow into it (matches lowered
    # usage, where `.arel` ends its chain).
    def arel
      Arel::SelectManager.new(bound_sql)
    end

    def none
      own_lists
      unload_records
      push_where("(1 = 0)", true)
      mark_null!
      self
    end

    # `reload` — drop the loaded records; the next terminal re-queries and
    # reflects committed changes.
    def reload
      unload_records
      self
    end

    # Rails' `load` — force the query now and memoize the records.
    def load
      to_a
      self
    end

    # Rails' `load_async` schedules the query on a background pool and
    # answers the relation; the first read waits for the rows. Loading
    # now is the same observable answer — this runtime has no async
    # executor to overlap the query with, and lobsters' story page
    # (`@story.comments…load_async`) reads the records a few lines on.
    def load_async
      load
    end

    # An eager load's records, handed to the relation that would have
    # gone and fetched them. A `has_many :through` reader answers a
    # Relation (the join lives on the intermediate table, so there is no
    # direct-fk query to materialize), while `includes(:tags)` preloads
    # through `_preload_tags` into the owner's cache ivar — this is the
    # seam between the two: the reader builds its joined relation and
    # seeds the loaded-records memo with what the preload already
    # fetched, so no query runs and the declared return type stays
    # Relation on every path through the reader.
    #
    # `loaded` is a separate argument because the cache cannot answer
    # the question: every association ivar starts `[]` in `initialize`,
    # which is indistinguishable from an association that loaded and
    # found nothing. False is a plain no-op.
    #
    # Chaining on afterwards drops the seed like any other chain method
    # (`where`/`order`/… all clear `@records`), so a caller that
    # narrows a preloaded relation re-queries rather than filtering a
    # stale set — Rails' behavior for a loaded relation, and the reason
    # the join conditions stay on the relation instead of this method
    # answering the bare Array.
    def preloaded(records, loaded)
      @records = records if loaded
      self
    end

    # The model class this relation queries — Rails' Relation#klass
    # (lobsters' Search switches on it to pick per-model joins).
    def klass
      @model
    end

    # Rails delegates these to the model class (relation/delegation.rb:
    # `delegate :table_name, :quoted_table_name, :primary_key,
    # :connection, :columns_hash, to: :klass`; `arel_table` likewise).
    def primary_key
      @model.primary_key
    end

    def table_name
      @model.table_name
    end

    def arel_table
      @model.arel_table
    end

    # ---- terminals --------------------------------------------------

    # Loads once, then serves the memoized records (see the class
    # comment). Hands back a shallow copy each call — Rails' `to_a`
    # contract — so a caller sorting or appending to the result can't
    # corrupt the cache; the record objects themselves stay shared.
    def to_a
      cached = @records
      return cached.dup unless cached.nil?
      records = load_records
      @records = records
      records.dup
    end

    # The rows, hydrated. With no explicit `select`, the projection is
    # the model's own column list and the model hydrates typed records
    # straight from the statement (`_hydrate_all`) — the positions are
    # fixed at compile time, so no String-keyed Hash per row. An
    # explicit `select` can project anything, and keeps the Hash path.
    def load_records
      return [] if @null
      records = if @select_sql.nil?
        @model._hydrate_all(select_sql_with(@model._columns_sql))
      else
        # A partial projection: the columns it left out answer
        # `has_attribute?` false (`Base#_note_unloaded`).
        rows = ActiveRecord.adapter.select_rows(bound_sql)
        rows.map do |row|
          record = @model.instantiate(row)
          record._note_unloaded(row)
          record
        end
      end
      records.each { |record| record.readonly! } if @readonly
      if @includes.length > 0 && !@skip_preloading && records.length > 0
        pending = ActiveRecord::PendingPreload.new(@model, records, @includes.dup)
        records.each { |record| record._pend_preload(pending) }
      end
      records
    end

    # Implicit array conversion — Rails delegates `to_ary` to the
    # loaded records, which is what lets `[story, relation].flatten`
    # splice the relation's records into the surrounding Array
    # (Array#flatten recurses into elements that respond to to_ary).
    # `relation.to_set` — Enumerable's, on the loaded records (campfire's
    # `Rooms::Direct.find_or_create_for(...).users.to_set`, comparing
    # direct-room members whatever their order).
    def to_set
      Set.new(to_a)
    end

    def to_ary
      to_a
    end

    # `relation == array` — Rails compares the LOADED RECORDS, and this
    # covers BOTH orders. Ruby's `Array#==` hands the comparison over to
    # the other operand whenever that operand answers `to_ary` (which
    # the method above does), so `[ message ] == Message.search("eel")`
    # arrives here as `relation == [ message ]`. Without it that
    # expression fell through to `Object#==` — reference identity
    # between an Array and a Relation, which is false for every input,
    # so `assert_equal <array>, <relation>` was an assertion no emitted
    # tree could pass. campfire's `message_searchable_test` is three
    # tests written entirely in that shape.
    #
    # Relation-to-relation compares records too, rather than Rails'
    # `to_sql` equality: two relations that differ only in how they were
    # BUILT are the same result set here, and the result set is what a
    # caller is asking about.
    #
    # PRIMARY KEYS, not `==` on the elements, and that is the whole
    # point rather than a shortcut. Rails answers this through
    # `ActiveRecord::Core#==`, where two objects are the same record
    # when they share a class and a saved id — a row read twice is two
    # objects. This runtime has no such `Base#==`: an operator
    # DEFINITION in `base.rb` reaches every strict target, and no
    # emitter renames one to its host's spelling (measured — python
    # wrote `def ==(self, other)` verbatim and every emitted tree
    # stopped at a SyntaxError). A relation's records are all one model
    # by construction, so comparing ids here is that rule with the class
    # half already decided. Record-to-record `==` remains identity, and
    # closing it means teaching each emitter the rename.
    def ==(other)
      mine = to_a
      theirs = other.is_a?(ActiveRecord::Relation) ? other.to_a : other
      return false if !theirs.is_a?(Array)
      return false if mine.length != theirs.length
      ids_of(mine) == ids_of(theirs)
    end

    # `filter { |r| … }` — Enumerable's filter over the loaded records.
    # `select { |r| … }` is Rails' other spelling of this method and
    # arrives here renamed (`relation_select_block`), which leaves the
    # projection `select(*specs)` above monomorphic.
    def filter
      out = []
      loaded_records.each { |x| out << x if yield x }
      out
    end

    # `relation + array` — Rails materializes and concatenates
    # (`to_a + other`), yielding a plain Array. The set operations
    # (`&`, `|`, `-`) are delegated to the loaded records the same
    # way (ActiveRecord::Delegation's array-method delegation);
    # lobsters intersects `story.tags & filtered_tags`.
    #
    # The other operand may be a Relation too — `filtered_tags` is
    # one — and Rails' Array operators take it through `to_ary`, so it
    # is loaded here the way `==` below loads it; handed over as is, a
    # Relation reached Array#& and raised TypeError. Membership is by
    # PRIMARY KEY for the reason `==` gives: Rails' `Array#&` asks the
    # records' `eql?`/`hash`, which ActiveRecord::Core answers by class
    # and id, and this runtime has no such `Base#==` — a row loaded by
    # each side is two objects, and identity would intersect them to
    # nothing. Like Array's, `&` and `|` drop repeats; `-` does not.
    def +(other)
      to_a + set_operand(other)
    end

    def &(other)
      id_filter(to_a, operand_ids(other), true)
    end

    def |(other)
      to_a + id_filter(set_operand(other), ids_of(to_a), false)
    end

    def -(other)
      id_filter(to_a, operand_ids(other), false)
    end

    # The records whose id is (`keep`) or is not in `ids`.
    def id_filter(records, ids, keep)
      records.select { |r| ids.include?(r.id) == keep }
    end

    def set_operand(other)
      other.is_a?(ActiveRecord::Relation) ? other.to_a : other
    end

    def operand_ids(other)
      ids_of(set_operand(other))
    end

    def ids_of(records)
      records.map { |r| r.id }
    end

    # Loaded: scan the cache. Unloaded: exists?(id), not ids (ORDER BY).
    def include?(record)
      return false if record.nil?
      rid = record.id
      return false if rid.nil?
      key = @model._cast_primary_key(rid)
      return false if key.nil?
      loaded = @records
      unless loaded.nil?
        return loaded.any? { |x| x.id == key }
      end
      exists?(key)
    end

    # Memoized rows, not a dup. each/find_each return self.
    def loaded_records
      records = @records
      if records.nil?
        records = load_records
        @records = records
      end
      records
    end

    # The Array methods Rails' ActiveRecord::Delegation hands to the
    # loaded records (`delegate :join, :[], :sample, :reverse, :rotate,
    # :compact, :shuffle, :slice, :index, :rindex, :intersect?, … to:
    # :records`), over the same `loaded_records` the block forms read.
    def [](*args)
      loaded_records[*args]
    end

    def slice(*args)
      loaded_records.slice(*args)
    end

    def join(separator = $,)
      loaded_records.join(separator)
    end

    def sample(*args)
      loaded_records.sample(*args)
    end

    def shuffle(*args)
      loaded_records.shuffle(*args)
    end

    def reverse
      loaded_records.reverse
    end

    def rotate(count = 1)
      loaded_records.rotate(count)
    end

    def compact
      loaded_records.compact
    end

    def index(*args, &block)
      loaded_records.index(*args, &block)
    end

    def rindex(*args, &block)
      loaded_records.rindex(*args, &block)
    end

    def intersect?(other)
      loaded_records.intersect?(other.to_a)
    end

    def each
      return to_enum(:each) { loaded_records.length } unless block_given?
      records = loaded_records
      i = 0
      n = records.length
      while i < n
        yield records[i]
        i += 1
      end
      self
    end

    # `index_by { |r| key }` — the records as a Hash keyed by the
    # block's value, last write winning on duplicates (Rails'
    # contract; lobsters keys tag filters by id).
    def index_by
      h = {}
      loaded_records.each { |x| h[yield x] = x }
      h
    end

    # `find_each` — Rails batches; corpus sizes make one load the same
    # answer. Loop duplicated from `each`: a nested `{ |x| yield x }`
    # left `x` as TyVar under Bar A.
    def find_each
      records = loaded_records
      i = 0
      n = records.length
      while i < n
        yield records[i]
        i += 1
      end
      self
    end

    # `find_in_batches` — Rails yields successive Arrays of rows.
    # Corpus sizes make one load the same answer as find_each; yield
    # the whole page as a single batch. Campfire's unread fanout /
    # push paths call this on memberships.
    def find_in_batches
      records = loaded_records
      yield records
      self
    end

    # Not split into `of:`-row relations: one batch is the same answer at corpus sizes (as `find_in_batches`), so the block and the enumerator are this relation.
    def in_batches(of: 1000, order: nil)
      yield self if block_given?
      self
    end

    # Via loaded_records (not to_a): no shallow Array copy of the
    # memoized rows. to_a keeps its Rails dup contract for callers that
    # mutate the returned array.
    def map
      loaded_records.map { |x| yield x }
    end

    # Enumerable's `to_h { |rec| [k, v] }` over the materialized rows
    # (campfire's push pool test keys each subscription's badge by its
    # endpoint this way).
    def to_h
      loaded_records.to_h { |x| yield x }
    end

    # `collect` is Enumerable's second name for `map`, and Rails
    # relations answer it because they delegate the whole of Enumerable
    # to `to_a`. campfire's membership extension reaches it
    # (`Array(users).collect { … }` where `users` is a relation).
    # Duplicated rather than aliased: strict targets want a real
    # definition, not an alias, and a body forwarding to `map` would
    # have to forward the block too.
    def collect
      loaded_records.map { |x| yield x }
    end

    # `group_by { |rec| key }` — Enumerable's grouping over the
    # materialized rows (lobsters threads comments with
    # `@comments.group_by(&:parent_comment_id)`). fetch-then-insert
    # rather than `Hash.new { [] }` (no default-proc portability) or
    # `[]=`-chaining on a maybe-missing key.
    def group_by
      out = {}
      loaded_records.each do |rec|
        k = yield rec
        arr = out.fetch(k, nil)
        if arr.nil?
          arr = []
          out[k] = arr
        end
        arr << rec
      end
      out
    end

    # `partition { |r| … }` — Enumerable's two-way split over the
    # materialized rows, `[matching, rest]`. campfire's account page
    # writes `@administrators, @members = users.partition(&:administrator?)`
    # straight off a `User.where(...)`.
    def partition
      loaded_records.partition { |x| yield x }
    end

    # `detect { |r| … }` — Enumerable's first match, nil when none.
    # (`find` is NOT this: on a Relation that is Rails' find-by-id.)
    def detect
      to_a.detect { |x| yield x }
    end

    # `sort_by { |r| key }` — Enumerable's sort over the materialized
    # rows. Distinct from `order`, which is SQL: this one sorts by a
    # value the block computes in Ruby, which is why campfire reaches
    # for it to sort direct rooms by their room's `updated_at`.
    def sort_by
      to_a.sort_by { |x| yield x }
    end

    # `inject(initial) { |acc, x| ... }` — the accumulator form the
    # corpus uses (vote-hash batchers). The no-initial and Symbol forms
    # aren't modeled; callers pass an explicit seed.
    def inject(initial)
      acc = initial
      to_a.each { |x| acc = yield(acc, x) }
      acc
    end

    # `each_with_object(memo) { |r, memo| … }` — Enumerable's fold that
    # threads one mutable memo and answers it. lobsters builds its vote
    # lookup tables this way straight off a query
    # (`Vote.where(…).select(…).each_with_object({}) { |v, memo| … }`,
    # on every comment listing).
    def each_with_object(memo)
      to_a.each { |x| yield(x, memo) }
      memo
    end

    def first
      prior = @limit
      keyed = order_by_key("ASC")
      @limit = 1
      rows = to_a
      @limit = prior
      @orders = [] if keyed
      # The one-row load must NOT stay memoized: `@records` is the
      # loaded-relation cache a later `each`/`map` reads, and a cache
      # holding the single row `first` asked for would answer those with
      # one row out of many. Same terminal rule as `pluck`.
      unload_records
      rows.length == 0 ? nil : rows[0]
    end

    # `take` — a row with no ordering imposed: Rails leaves the order to
    # the database, so unlike `first` no key order is added.
    def take
      prior = @limit
      @limit = 1
      rows = to_a
      @limit = prior
      @records = nil
      rows.length == 0 ? nil : rows[0]
    end

    # Rails' `ordered_relation` for `first` and `first(n)`: a relation
    # with no ORDER BY of its own, not yet loaded, orders by the model's
    # key columns (`_order_columns`: `query_constraints_list` once the
    # call has run, else the primary key). PostgreSQL answers an
    # unordered SELECT in heap order, which an UPDATE changes. Whether it
    # added the order, so the caller can drop it again.
    def order_by_key(direction)
      return false unless @records.nil? && @orders.empty?
      terms = key_order_terms(direction)
      return false if terms.empty?
      own_lists
      terms.each { |term| @orders << term }
      true
    end

    # A model without a primary key has no order columns, and Rails then
    # leaves the relation unordered.
    def key_order_terms(direction)
      columns = @model.query_constraints_list || [@model.primary_key]
      terms = []
      columns.each { |column| terms << "#{@table}.#{column} #{direction}" unless column.nil? || column.to_s.empty? }
      terms
    end

    # `first!` — like `first`, but raises `RecordNotFound` (→ 404 in the
    # dispatch layer) instead of returning nil when the relation is empty.
    def first!
      record = first
      raise RecordNotFound.new("Couldn't find #{@model.name}", @model.name, @model.primary_key) if record.nil?
      record
    end

    # `last!` and `take!`, the same bang over `last` and `take`.
    def last!
      record = last
      raise RecordNotFound.new("Couldn't find #{@model.name}", @model.name, @model.primary_key) if record.nil?
      record
    end

    def take!
      record = take
      raise RecordNotFound.new("Couldn't find #{@model.name}", @model.name, @model.primary_key) if record.nil?
      record
    end

    def last
      rows = last_n(1)
      rows.length == 0 ? nil : rows[0]
    end

    # Rails' `first(n)` / `last(n)` — the COUNTED forms, which answer an
    # Array of up to n records where the bare forms answer one record or
    # nil. Split into their own names rather than overloaded onto
    # `first`/`last` with an optional arg: the return type differs by
    # arity, which a strict target cannot express on one method (see the
    # monomorphize-polymorphic-APIs rule). `scope_chain` renames the call
    # site once it has PROVEN the receiver is a relation, so an Array
    # receiver keeps `Array#first(n)` — lobsters' `split.first(words * 2)`
    # must not be rewritten.
    #
    # Also a TERMINAL, so the borrowed `@limit` is restored — as
    # `first` and `pick` already do — and the one-page load is not
    # left memoized. Otherwise a relation that is paged and then
    # counted carries the page size into the count.
    def first_n(n)
      n = sql_limit(n)
      prior = @limit
      keyed = order_by_key("ASC")
      @limit = n
      rows = to_a
      @limit = prior
      @orders = [] if keyed
      unload_records
      rows
    end

    # Last n in relation order (Rails does not reverse the page).
    # Unloaded and unwindowed: reverse ORDER BY, LIMIT n, reverse rows.
    # Loaded or already LIMIT/OFFSET: in-memory tail of that window.
    def last_n(n)
      n = sql_limit(n)
      loaded = @records
      unless loaded.nil?
        return loaded_tail(loaded, n)
      end
      unless @limit.nil? && @offset.nil?
        return to_a.last(n)
      end
      own_lists
      prior_limit = @limit
      prior_orders = []
      i = 0
      while i < @orders.length
        prior_orders << @orders[i]
        i += 1
      end
      if @orders.empty?
        key_order_terms("DESC").each { |term| @orders << term }
      else
        reversed = []
        i = 0
        while i < @orders.length
          reversed << reverse_order_term(@orders[i])
          i += 1
        end
        @orders = reversed
      end
      @limit = n
      rows = to_a
      @limit = prior_limit
      @orders = prior_orders
      unload_records
      out = []
      i = rows.length - 1
      while i >= 0
        out << rows[i]
        i -= 1
      end
      out
    end

    # The last n of an already-loaded page, still in relation order.
    def loaded_tail(loaded, n)
      start = loaded.length - n
      start = 0 if start < 0
      out = []
      i = start
      while i < loaded.length
        out << loaded[i]
        i += 1
      end
      out
    end

    # `sole`'s loaded-relation reading: the first two (or fewer) of an
    # already-loaded page, still in relation order — the loaded-cache
    # counterpart to `loaded_tail`, and every `loaded`-touching step of
    # it (the cap, the indexing loop) lives in THIS top-level helper
    # rather than inline in `sole` itself. `@records`'s own ivar type
    # carries an untyped/poly component, and `sole` also assigns the
    # unloaded branch's `to_a` Array to a `rows` local of its own;
    # splitting the loaded branch's work out to a call boundary, same
    # as `loaded_tail`, gives `loaded` here its own concrete `Array
    # [Base]` type from the narrowed (non-nil) argument rather than
    # carrying that poly residue into `sole`'s body, which is what the
    # ceiling tests on this runtime's type precision caught.
    def loaded_sole_rows(loaded)
      cap = loaded.length < 2 ? loaded.length : 2
      out = []
      i = 0
      while i < cap
        out << loaded[i]
        i += 1
      end
      out
    end

    # `sole`'s unloaded-probe size: `min(2, limit)`, nil meaning the
    # usual 2, clamped at 0 or above. A top-level helper rather than a
    # local computed inline in `sole` with an if/else (one arm the
    # literal `2`, the other derived from `@limit`): merging those two
    # origins into one local left it an unresolved type variable where
    # `sole` assigns it back to `@limit` — same `Integer? -> Integer`
    # call-boundary fix as `loaded_head`, above.
    def sole_probe_limit(limit)
      return 2 if limit.nil?
      return 0 if limit < 0
      return 2 if limit > 2
      limit
    end

    def count(&block)
      return loaded_records.count(&block) if block
      return 0 if @null
      rows = ActiveRecord.adapter.select_rows(bound_count_sql)
      rows.length == 0 ? 0 : rows[0]["n"].to_i
    end

    # `sum(:col)` / `sum("<sql expr>")` — SQL SUM over the relation.
    # Returns Float: both corpus consumers are float arithmetic
    # (lobsters' hotness math); an Integer-column caller would want
    # column typing here, ledgered when one appears.
    def sum(expr = nil, &block)
      # Rails: with a block the argument is the initial value and the sum
      # is Enumerable's over the records; without one it names the column.
      return loaded_records.sum(expr.nil? ? 0 : expr, &block) if block
      raise ArgumentError, "sum: a column name or a block" if expr.nil?
      return 0.0 if @null
      term = expr.is_a?(Symbol) ? "#{@table}.#{expr}" : expr.to_s
      sql = "SELECT COALESCE(SUM(#{term}), 0) AS n FROM #{@table}"
      sql = "#{sql} #{@joins.join(" ")}" if @joins.length > 0
      sql = "#{sql} WHERE #{@wheres.join(" AND ")}" if @wheres.length > 0
      rows = ActiveRecord.adapter.select_rows(sql)
      rows.length == 0 ? 0.0 : rows[0]["n"].to_f
    end

    # `minimum(:column)` / `maximum(:column)` — SQL extrema over the
    # current relation. The aggregate replaces the projection and ordering,
    # while the relation's filters, grouping and pagination remain in place.
    def minimum(expr)
      extreme(expr, "MIN")
    end

    def maximum(expr)
      extreme(expr, "MAX")
    end

    def extreme(expr, function)
      # Grouped extrema answer a Hash (possibly empty); ungrouped null is nil.
      return {} if @null && @groups.length > 0
      return nil if @null
      column = expr.to_s.to_sym
      raise ArgumentError, "unknown aggregate column: #{expr}" unless @model.schema_columns.include?(column)
      return grouped_extreme(column.to_s, function) if @groups.length > 0
      scoped = spawn
      projection = "#{function}(#{@table}.#{ActiveRecord.adapter.quote_column_name(column)}) AS value"
      projection = "#{@select_sql}, #{projection}" if @havings.length > 0 && !@select_sql.nil?
      scoped.select(projection)
      scoped.clear_ordering
      rows = ActiveRecord.adapter.select_rows(scoped.bound_sql)
      return nil if rows.length == 0
      value = rows[0]["value"]
      cast_schema_value(value, column)
    end
    private :extreme

    # Grouped calculations return a key-to-extreme Hash, as Rails does.
    # Aliasing each group expression keeps both scalar keys and composite
    # Array keys stable even when a group expression is qualified.
    def grouped_extreme(column, function)
      names = []
      selects = []
      @groups.each_with_index do |group, index|
        name = "__rh_group_#{index}"
        names << name
        selects << "#{group} AS #{name}"
      end
      selects << "#{function}(#{@table}.#{ActiveRecord.adapter.quote_column_name(column)}) AS value"
      scoped = spawn
      selects.unshift(@select_sql) if @havings.length > 0 && !@select_sql.nil?
      scoped.select(selects.join(", "))
      scoped.clear_ordering
      rows = ActiveRecord.adapter.select_rows(scoped.bound_sql)
      grouped = {}
      rows.each do |row|
        key_values = []
        names.each_with_index do |name, index|
          value = row[name]
          group = @groups[index]
          group_column = schema_group_column(group)
          key_values << (group_column.nil? ? value : cast_schema_value(value, group_column))
        end
        key = names.length == 1 ? key_values[0] : key_values
        grouped[key] = cast_schema_value(row["value"], column.to_sym)
      end
      grouped
    end
    private :grouped_extreme

    # Recover a model-column group key only from a bare or explicitly
    # model-qualified identifier. Arbitrary SQL and joined-table columns
    # remain untyped rather than borrowing a coincidentally named schema field.
    def schema_group_column(group)
      expression = group.strip
      table_name = @model.table_name.to_s
      quoted_table = "\"#{table_name.gsub("\"", "\"\"")}\""
      prefixes = ["#{@table}.", "#{table_name}.", "#{quoted_table}."]
      column = nil
      prefixes.each do |prefix|
        if expression.start_with?(prefix)
          column = expression[prefix.length..]
          break
        end
      end
      column = expression if column.nil?
      if column.start_with?("\"") && column.end_with?("\"")
        column = column[1...-1].gsub("\"\"", "\"")
      end
      return nil unless column.match?(/\A[A-Za-z_][A-Za-z0-9_]*\z/)
      candidate = column.to_sym
      @model.schema_columns.include?(candidate) ? candidate : nil
    end
    private :schema_group_column

    # Raw aggregate rows need the same schema-selected conversions as
    # hydrated model fields; otherwise SQLite returns temporal values as text.
    def cast_schema_value(value, column)
      return nil if value.nil?
      return ActiveSupport.cast_boolean(value) if @model.schema_boolean_columns.include?(column)
      return ActiveSupport.parse_db_time(value) if @model.schema_time_columns.include?(column)
      return cast_schema_date(value) if @model.schema_date_columns.include?(column)
      return value.to_f if @model.schema_decimal_columns.include?(column)
      value
    end
    private :cast_schema_value

    # Not `ActiveSupport.parse_db_date`: Spinel has it only with the Date package, which redefines this; a date-free app has no date column.
    def cast_schema_date(value)
      value
    end
    private :cast_schema_date

    # `group(:col).count` — Rails hands back a Hash of group-key =>
    # COUNT. The group_count lowering renames the grouped chain's
    # terminal to this method, so the scalar `count` keeps its
    # Integer return (no polymorphic count). Single group expression
    # (the corpus shape); Rails' multi-group array keys would need a
    # composite key here first. HAVING and DISTINCT ride the same SQL
    # as `to_sql` so the Hash only contains surviving groups, and a
    # distinct projection counts distinct values per group (#343).
    def group_count
      return {} if @null
      key = @groups.join(", ")
      # DISTINCT uses a subquery so a multi-column / aliased `select`
      # never lands inside SQLite's one-expression COUNT(DISTINCT …).
      # HAVING filters groups on the original grouped relation first
      # (source columns and aggregates), then distinct-count rows that
      # belong to survivors. Non-distinct keeps `@select_sql` so HAVING
      # can name selected aliases, matching `count_sql`.
      sql = if @distinct
        inner = append_join_where(
          "#{cte_prefix}SELECT DISTINCT #{distinct_count_columns}, #{key} AS k FROM #{from_source}"
        )
        if @havings.length > 0
          survivors = append_group_having(
            append_join_where("#{cte_prefix}SELECT #{key} AS __rh_k FROM #{from_source}")
          )
          clause = "#{key} IN (SELECT __rh_k FROM (#{survivors}) AS __rh_surv)"
          inner = if @wheres.length > 0
            "#{inner} AND #{clause}"
          else
            "#{inner} WHERE #{clause}"
          end
        end
        "SELECT k, COUNT(*) AS n FROM (#{inner}) AS __rh_gc GROUP BY k"
      else
        proj = if @select_sql.nil?
          "#{key} AS k, COUNT(*) AS n"
        else
          "#{@select_sql}, #{key} AS k, COUNT(*) AS n"
        end
        append_group_having(
          append_join_where("#{cte_prefix}SELECT #{proj} FROM #{from_source}")
        )
      end
      h = {}
      rows = ActiveRecord.adapter.select_rows(sql)
      # One own column groups by values read through its declared type.
      own = @groups.length == 1 && key.start_with?("#{@table}.") ? key.delete_prefix("#{@table}.") : nil
      rows.each do |row|
        k = own.nil? ? row["k"] : @model._attribute_deserialize(own, row["k"])
        h[k] = row["n"].to_i
      end
      h
    end

    # Loaded: cache length. Unloaded: exists? (SELECT 1 LIMIT 1).
    def empty?
      return true if @null
      r = @records
      r.nil? ? !exists? : r.length == 0
    end

    def any?(*pattern, &block)
      return loaded_records.any?(*pattern, &block) if block || !pattern.empty?
      return false if @null
      r = @records
      r.nil? ? exists? : r.length > 0
    end

    # Rails loads for blank?; empty? keeps the existence probe.
    # lower::blank folds typed sites; these cover untyped dispatch.
    def blank?
      empty?
    end

    def present?
      !empty?
    end

    def presence
      empty? ? nil : self
    end

    # Enumerable#none? without a block is empty? (uses the loaded cache).
    def none?(*pattern, &block)
      return loaded_records.none?(*pattern, &block) if block || !pattern.empty?
      empty?
    end

    # Exactly one row. Unloaded: SELECT 1 LIMIT 2. No block form.
    def one?(*pattern, &block)
      return loaded_records.one?(*pattern, &block) if block || !pattern.empty?
      r = @records
      return r.length == 1 unless r.nil?
      probe_existence(2) == 1
    end

    # More than one row. Unloaded: SELECT 1 LIMIT 2.
    def many?(&block)
      return loaded_records.count(&block) > 1 if block
      r = @records
      return r.length > 1 unless r.nil?
      probe_existence(2) > 1
    end

    # Block form of Enumerable#all? over the materialized rows (the
    # runtime `Base.where` fallback returns a Relation, and dynamic
    # call-sites treat it as the array Rails hands back).
    def all?
      ok = true
      loaded_records.each { |x| ok = false unless yield x }
      ok
    end

    # Hash conditions share where's predicate builder; scalar conditions
    # select the primary key. Unloaded: exists_sql (SELECT 1 LIMIT 1).
    # No argument asks for any row; `exists?(nil)` is false, as Rails'
    # `return false if !conditions`. The argument is a splat so "absent"
    # and `nil` stay distinct without an untyped default.
    def exists?(*conditions)
      raise ArgumentError, "wrong number of arguments (given #{conditions.length}, expected 0..1)" if conditions.length > 1
      return false if @null
      return false if @limit == 0
      if conditions.empty?
        r = @records
        return r.length > 0 unless r.nil?
        return probe_existence(1) > 0
      end
      id = conditions[0]
      # Rails' `return false if !conditions` — only nil/false are falsy.
      return false if id.nil? || id == false
      own_lists
      # Popped for the same reason `find` and `find_by` pop: a terminal
      # that answered a question must not narrow the relation it was
      # asked on.
      sql = if id.is_a?(Hash)
        hash_conditions(id)
      else
        "#{@table}.#{@model.primary_key} = #{bind_literal(@model._attribute_for_database(@model.primary_key, id))}"
      end
      push_where(sql) unless sql.empty?
      begin
        probe_existence(1) > 0
      ensure
        pop_where unless sql.empty?
      end
    end

    # How many probe rows `exists_sql(n)` returns. Shared by `exists?`,
    # `one?`, and `many?` so cardinality questions never hydrate.
    def probe_existence(n)
      return 0 if @null
      return 0 if @limit == 0
      ActiveRecord.adapter.select_rows(bound_exists_sql(n)).length
    end

    # `count > n` without a COUNT(*): same FROM/JOIN/WHERE as
    # `count_sql` (LIMIT/OFFSET/ORDER ignored), then `LIMIT 1 OFFSET n`.
    # Does not mutate the relation — `offset(n).exists?` would, and a
    # loaded `exists?` would ignore that offset.
    def more_than?(n)
      return false if @null
      return true if n < 0
      sql = "#{cte_prefix}SELECT 1 AS one FROM #{from_source}"
      sql = "#{sql} #{@joins.join(" ")}" if @joins.length > 0
      sql = "#{sql} WHERE #{@wheres.join(" AND ")}" if @wheres.length > 0
      sql = "#{sql} LIMIT 1 OFFSET #{n}"
      ActiveRecord.adapter.select_rows(sql).length > 0
    end

    def length
      loaded_records.length
    end

    # Rails' `Relation#size`: length when loaded; COUNT when unloaded
    # and unbounded. A LIMIT/OFFSET window must not answer the table
    # total — `limit(2).size` is at most 2 — so the limited path counts
    # a `SELECT 1` subquery rather than `count_sql` (which omits LIMIT
    # so page totals see the unpaginated result set).
    def size
      r = @records
      return r.length unless r.nil?
      return count if @limit.nil? && @offset.nil?
      prior_orders = @orders
      @orders = []
      # Same DISTINCT pitfall as exists_sql: `SELECT DISTINCT 1` collapses
      # every matching row into one, so `distinct.limit(5).size` would
      # answer 1. Project the primary key when distinct so the outer
      # COUNT sees separate rows under LIMIT.
      cols = @distinct ? "#{@table}.#{@model.primary_key}" : "1 AS one"
      inner = select_sql_with(cols)
      @orders = prior_orders
      sql = "SELECT COUNT(*) AS n FROM (#{inner}) AS __rh_size"
      rows = ActiveRecord.adapter.select_rows(sql)
      rows.length == 0 ? 0 : rows[0]["n"].to_i
    end

    # `delete_all` — bulk DELETE scoped by the accumulated WHEREs.
    # Rails contract: no callbacks, no per-row loads, returns the
    # affected-row count. ORDER/LIMIT don't apply to bulk ops.
    def delete_all
      return 0 if @null
      sql = "DELETE FROM #{@table}#{scoped_write_where}"
      ActiveRecord.adapter.execute_ddl(sql)
      ActiveRecord.adapter.changes
    end

    # `destroy_all` — load the scoped records and destroy each one, so
    # `before_destroy` / `after_destroy` and any dependent-association
    # cleanup RUN. Deliberately not `delete_all` with a different name:
    # Rails draws exactly this line, and campfire depends on the
    # callback half (`Search#trim_recent_searches` prunes a user's
    # search history through it). The Array of destroyed records is
    # Rails' return value too.
    def destroy_all
      records = to_a
      records.each { |r| r.destroy }
      records
    end

    # `destroy_by(conditions)` — Rails' `where(conditions).destroy_all`,
    # and spelled as exactly that so the callback contract `destroy_all`
    # documents above carries over unchanged. campfire's
    # `Room#memberships.revoke_from` is the caller.
    def destroy_by(conditions)
      where(conditions).destroy_all
    end

    # `delete_by(conditions)` — `destroy_by`'s callback-skipping twin,
    # the same line Rails draws between `destroy_all` and `delete_all`.
    def delete_by(conditions)
      where(conditions).delete_all
    end

    # `update_all(...)` — bulk UPDATE scoped by the accumulated WHEREs.
    # Hash form (`update_all(user_id: 3)`) escapes values; String form
    # (`update_all("hits = hits + 1")`) is trusted verbatim, same as
    # Rails. Returns the affected-row count.
    def update_all(updates)
      return 0 if @null
      set_sql = if updates.is_a?(Hash)
        parts = []
        updates.each do |key, val|
          parts.push("#{key} = #{ActiveRecord.adapter.escape_value(@model._attribute_for_database(key.to_s, val))}")
        end
        parts.join(", ")
      else
        updates.to_s
      end
      sql = "UPDATE #{@table} SET #{set_sql}#{scoped_write_where}"
      ActiveRecord.adapter.execute_ddl(sql)
      ActiveRecord.adapter.changes
    end

    # `touch_all(:col)` — Rails' bulk touch: `updated_at` (when the table
    # has one) and the named column set to the current time, by one
    # UPDATE scoped like `update_all`, no callbacks. lobsters marks its
    # inbox read this way after every inbox page (`@notifications
    # .where(read_at: nil).touch_all(:read_at)`). One column rather than
    # Rails' `*names`: that is every corpus call, and a splat of names
    # would reach the SQL untyped.
    def touch_all(name = nil)
      return 0 if @null
      now = ActiveRecord.adapter.escape_value(ActiveSupport.db_now)
      parts = []
      parts.push("updated_at = #{now}") if @model.schema_columns.include?(:updated_at)
      parts.push("#{name} = #{now}") unless name.nil?
      return 0 if parts.empty?
      ActiveRecord.adapter.execute_ddl("UPDATE #{@table} SET #{parts.join(", ")}#{scoped_write_where}")
      ActiveRecord.adapter.changes
    end

    # The WHERE clause a bulk WRITE takes — the one place `delete_all`
    # and `update_all` differ from every read on this class.
    #
    # A read appends `@joins` to its FROM; SQL has no such place in a
    # DELETE or an UPDATE, and dropping the join while KEEPING the
    # conditions that name it emits a statement about a table that is
    # not in the query. campfire's `User#deactivate` runs
    # `memberships.without_direct_rooms.delete_all`, whose scope is
    # `joins(:room).where.not(room: { type: "Rooms::Direct" })`, and it
    # produced `DELETE FROM memberships WHERE … AND NOT (room.type =
    # 'Rooms::Direct')` — "no such column: room.type".
    #
    # Rails answers the same way: scope the write by a subquery on the
    # primary key, which is where the join CAN live. The `IN (SELECT …)`
    # form is what SQLite supports (it has no `DELETE … USING`), and it
    # is portable to every adapter this runtime might grow.
    #
    # Returns "" for the unscoped case so an unconditional `delete_all`
    # still emits a bare `DELETE FROM <table>` — Rails' truncate-shaped
    # statement, not a subquery over every row.
    def scoped_write_where
      return "" if @wheres.length == 0 && @joins.length == 0
      if @joins.length == 0
        return " WHERE #{@wheres.join(" AND ")}"
      end
      key = "#{@table}.#{@model.primary_key}"
      inner = "SELECT #{key} FROM #{@table} #{@joins.join(" ")}"
      inner = "#{inner} WHERE #{@wheres.join(" AND ")}" if @wheres.length > 0
      " WHERE #{@model.primary_key} IN (#{inner})"
    end

    # `pluck(:col)` — a single column projected to an Array of its raw
    # values (strings as stored; callers coerce).
    #
    # THE PROJECTION IS RESTORED, and that is not housekeeping. `pluck`
    # is a TERMINAL: Rails builds it a query of its own and leaves the
    # receiver alone, so the same relation can be plucked and then
    # loaded. Leaving `users.id AS v` behind meant the NEXT `to_a` on
    # that object hydrated whole records out of a one-column row —
    # every field blank, every id 0, no error anywhere. campfire's
    # `Rooms::Direct.find_or_create_for` does exactly that: `find_for`
    # plucks the user ids, then hands the SAME relation to `grant_to`,
    # which built memberships for user 0.
    #
    # Rails' `pluck(*column_names)`: one column answers its values, two
    # or more answer one Array per row in the order named
    # (`pluck(:id, :root_inventory_group_id).to_h`, Core's
    # MigratedNamedQuantityInitializer).
    def pluck(*cols)
      if cols.length == 0
        raise NotImplementedError, "roundhouse: #{@model}.pluck with no column names is not ported"
      end
      return [] if @null
      prior = @select_sql
      # A column with a declared type reads through it, as Rails' does; an
      # alias names its column.
      names = cols.map { |col| @model._attribute_name(col.to_s) }
      projections = []
      names.each_with_index { |name, i| projections << "#{@table}.#{name} AS v#{i}" }
      @select_sql = projections.join(", ")
      rows = ActiveRecord.adapter.select_rows(bound_sql)
      @select_sql = prior
      if names.length == 1
        name = names[0]
        return rows.map { |row| @model._attribute_deserialize(name, row["v0"]) }
      end
      rows.map do |row|
        values = []
        names.each_with_index { |name, i| values << @model._attribute_deserialize(name, row["v#{i}"]) }
        values
      end
    end

    # `pick(col)` — Rails' `limit(1).pluck(col).first`: the single value
    # from the first row, or nil when the relation matches nothing.
    # Lobsters reads every Keystore counter through it.
    def pick(*cols)
      prior = @limit
      @limit = 1
      rows = pluck(*cols)
      @limit = prior
      rows.length == 0 ? nil : rows[0]
    end

    # `ids` — primary keys, cast through the model's key type. Reads
    # `@model.primary_key` (not a hard-coded `id` column) and casts the
    # way `find` does, so uuid / string keys survive (#310).
    def ids
      return [] if @null
      cached = @records
      unless cached.nil?
        # `preloaded` may seed integer primary keys, not only records.
        return cached.map { |row| row.is_a?(Base) ? row.id : row }
      end
      # Rails' ids on a composite primary key: one key tuple per record.
      return to_a.map { |record| record.id } if @model.composite_primary_key?
      prior = @select_sql
      key = @model.primary_key
      @select_sql = "#{@table}.#{key} AS v"
      rows = ActiveRecord.adapter.select_rows(bound_sql)
      @select_sql = prior
      rows.map { |row| @model._primary_key_from_database(row["v"]) }
    end

    # `find(id)` — the row with that primary key, RAISING
    # `RecordNotFound` when there is none. That raise is Rails' whole
    # distinction between `find` and the `find_by` below it, and it is
    # what turns a missing record into a 404 instead of a nil that
    # NoMethodErrors somewhere later. This answered nil until now, and
    # campfire's autocomplete is where that showed: `Current.user.rooms
    # .find(params[:room_id]).users` on a room the user is not a member
    # of read "undefined method 'users' for nil" — the test asserting
    # `assert_raises ActiveRecord::RecordNotFound` on exactly that
    # request.
    #
    # The id predicate is POPPED afterwards: like `pluck`, this is a
    # terminal, and a `WHERE id = 3` left on the relation would silently
    # narrow every later use of it to that one row. Popped BEFORE the
    # raise for the same reason — an exception a caller rescues must not
    # leave the relation altered.
    #
    # The messages are Rails 8.1's (`raise_record_not_found_exception!`)
    # minus the ` [WHERE ...]` suffix Rails appends for a scoped
    # relation: Rails renders it from Arel with `?` binds, while the
    # wheres here are SQL text with the values already filled in, so
    # the suffix could not match. An unscoped relation's message is
    # exactly Rails'.
    def find(id = nil, &block)
      return loaded_records.find(&block) if block
      if @model.composite_primary_key?
        raise NotImplementedError, "roundhouse: find on a relation of #{@model.name}, whose primary key is composite, is not supported"
      end
      return friendly_find(id) if @friendly
      find_primary_key(id)
    end

    # `Model.friendly` / `relation.friendly` (friendly_id's `all.extending(
    # FriendlyId::FinderMethods)`): from here on `find` is friendly_id's
    # finder. The flag rides the relation through its chain, as the
    # extension does in Rails (`Article.friendly.includes(:x).find(s)`,
    # `Article.with_discarded.friendly.find(s)`). Only a model that
    # declared `friendly_id` (lower::friendly_id) has the class-side
    # configuration `friendly_find` reads; any other raises there.
    def friendly
      @friendly = true
      self
    end

    # friendly_id 5.x `FinderMethods#find` for one id, in the gem's order:
    #
    #   1. anything but a String (Integer, nil, Array, ...) is
    #      `unfriendly_id?`, so the plain find: `find(nil)` is "without an
    #      ID", `find(3)` is the primary key. A String is never unfriendly
    #      ("123" answers nil, not true, to `friendly_id?`; neither
    #      short-circuits).
    #   2. `find_by(slug_column => id)`; with `:history`, then the newest
    #      `friendly_id_slugs` row for (sluggable_type, slug), joined to
    #      this relation, so its other conditions still apply.
    #   3. no match, and the String could be a primary key
    #      (`potential_primary_key?`): the plain find, which raises
    #      Rails' own not-found for a missing row. A "123" slug beats the
    #      row with id 123, as in the gem.
    #   4. otherwise RecordNotFound with the gem's message.
    #
    # Not modeled: `find(id, allow_nil: true)` and `find(a, b)` (the
    # gem's `args.count != 1` pass-through).
    def friendly_find(id = "")
      return find_primary_key(id) unless id.is_a?(String)
      record = friendly_first(id)
      return record unless record.nil?
      return find_primary_key(id) if FriendlyIdFinder.potential_primary_key?(id, @model._string_primary_key)
      column = @model._friendly_slug_column
      raise RecordNotFound.new("can't find record with friendly id: #{id.inspect}", @model.name, column, id)
    end

    # `first_by_friendly_id`: the slug column, then (history) the slug
    # table. `find_by` pops its predicate, so the relation is unchanged.
    #
    # `.to_s` before `.to_sym`: on a model without friendly_id the base
    # `_friendly_slug_column` only raises, and Spinel reads a raise-only
    # class method called through a class value as nil. That model never
    # reaches here (it has no `friendly`), but the program must still
    # compile.
    def friendly_first(id = "")
      record = find_by({ @model._friendly_slug_column.to_s.to_sym => id })
      return record unless record.nil?
      return nil unless @model._friendly_history
      friendly_history_first(id)
    end

    # history.rb `slug_table_record`: this relation joined to
    # `friendly_id_slugs`, restricted to (this model's type, the slug),
    # newest slug row first, projecting the model's columns. The join,
    # predicate and ordering are popped again; the projection and limit
    # restored: a terminal must not alter the relation it was asked on.
    def friendly_history_first(id = "")
      own_lists
      prior_select = @select_sql
      prior_limit = @limit
      @select_sql = nil
      @limit = 1
      @joins << "INNER JOIN friendly_id_slugs ON friendly_id_slugs.sluggable_id = #{@table}.#{@model.primary_key}"
      push_where("friendly_id_slugs.sluggable_type = #{ActiveRecord.adapter.escape_value(@model.name)} AND friendly_id_slugs.slug = #{ActiveRecord.adapter.escape_value(id)}")
      @orders << "friendly_id_slugs.id DESC"
      begin
        rows = load_records
      ensure
        @orders.pop
        pop_where
        @joins.pop
        @select_sql = prior_select
        @limit = prior_limit
      end
      rows.length == 0 ? nil : rows[0]
    end

    def find_primary_key(id)
      # Rails compacts the ids first, so `find(nil)` has none: the
      # "without an ID" form, with no id, as `Base.find(nil)` raises.
      raise RecordNotFound.new("Couldn't find #{@model.name} without an ID", @model.name, @model.primary_key) if id.nil?
      return find_ids(id) if id.is_a?(Array)
      key = @model._cast_primary_key(id)
      prior_limit = @limit
      own_lists
      push_where("#{@table}.#{@model.primary_key} = #{bind_literal(@model._attribute_serialize(@model.primary_key, key))}")
      begin
        @limit = 1
        rows = load_records
        record = rows.length == 0 ? nil : rows[0]
      ensure
        @limit = prior_limit
        pop_where
      end
      if record.nil?
        raise RecordNotFound.new("Couldn't find #{@model.name} with '#{@model.primary_key}'=#{id.inspect}", @model.name, @model.primary_key, id)
      end
      record
    end

    # Array form: deduplicate BEFORE the column cast, as Rails does.
    # Unordered relations follow the requested IDs (after slicing by
    # offset/limit); explicitly ordered relations follow their SQL order.
    # Read directly rather than through to_a: its loaded cache belongs to
    # the original relation and must neither mask nor remember this filter.
    def find_ids(ids)
      ids = ids.uniq
      return [] if ids.empty?
      return [find_primary_key(ids[0])] if ids.length == 1
      prior_limit = @limit
      prior_offset = @offset
      prior_select = @select_sql
      expected = ids.length
      if @orders.empty?
        ids = ids[prior_offset || 0, prior_limit || ids.length] || []
        expected = ids.length
      else
        expected = prior_limit if !prior_limit.nil? && expected > prior_limit
        expected = ids.length - prior_offset if !prior_offset.nil? && ids.length - prior_offset < expected
      end
      keys = ids.map { |id| @model._cast_primary_key(id) }
      sql_ids = keys.map { |key| ActiveRecord.adapter.escape_value(@model._attribute_serialize(@model.primary_key, key)) }.join(", ")
      own_lists
      push_where((keys.empty? ? "1=0" : "#{@table}.#{@model.primary_key} IN (#{sql_ids})"))
      begin
        if @orders.empty?
          @limit = nil
          @offset = nil
        end
        @select_sql = "#{prior_select}, #{@table}.#{@model.primary_key}" unless prior_select.nil?
        rows = load_records
      ensure
        @limit = prior_limit
        @offset = prior_offset
        @select_sql = prior_select
        pop_where
      end
      if rows.length != expected
        listed = ids.map { |each_id| each_id.inspect }.join(", ")
        raise RecordNotFound.new("Couldn't find all #{Inflector.pluralize_word(@model.name, 2)} with '#{@model.primary_key}': (#{listed}) (found #{rows.length} results, but was looking for #{expected}).", @model.name, @model.primary_key, ids)
      end
      if @orders.empty?
        keys.map { |key| rows.find { |row| row.id == key } }
      else
        rows
      end
    end

    # A TERMINAL, so its predicate is POPPED — the same rule `find`
    # above spells out, and omitting it here cost campfire its entire
    # room page. `find_messages` asks `messages.find_by(id:
    # params[:message_id])` to decide between two pagings and then
    # pages THE SAME relation; on a plain `/rooms/1` the id is nil, so
    # the probe left `WHERE id IS NULL` behind and `last_page` answered
    # zero rows against a room holding a hundred. The page still came
    # back 200, complete and well-formed, with an empty message list —
    # which is exactly the failure the app's own tests cannot see.
    #
    # Borrow a predicate for a terminal (`find_by` / `find_sole_by`):
    # push via `add_condition`, run the body, then restore `@wheres`,
    # `@null`, and `@records` so empty IN cannot stick NullRelation and
    # a loaded relation keeps its memo whether the body returns or raises.
    def with_borrowed_condition(conditions)
      prior_null = @null
      prior_records = @records
      pushed = add_condition(conditions, [], false)
      begin
        yield
      ensure
        pop_where if pushed
        @null = prior_null
        @records = prior_records
      end
    end

    # `add_condition` answers whether it pushed — a nil or empty
    # condition pushes nothing — so the pop is guarded by that rather
    # than issued unconditionally.
    def find_by(conditions)
      with_borrowed_condition(conditions) { take }
    end

    # `find_by!` — `find_by` that raises `RecordNotFound` on no match.
    def find_by!(conditions)
      record = find_by(conditions)
      raise RecordNotFound.new("Couldn't find #{@model.name}", @model.name, @model.primary_key) if record.nil?
      record
    end

    # `sole` (Rails 7.0) — the relation's one record: `RecordNotFound`
    # when it matches none, `SoleRecordExceeded` when it matches more.
    # Rails reads `first(2)` and checks the size, so one query of at
    # most two rows tells the three cases apart.
    #
    # A LOADED relation decides from the memoized `@records` instead of
    # re-querying — Rails' own `first(2)` reads the loaded Array once
    # `loaded?` — so a caller that already has the page in memory (an
    # eager-loaded association, a prior `each`) costs no second trip to
    # the database.
    #
    # Unloaded, `first_n` cannot serve the probe: it always asks for
    # its own `n`, which would override a SMALLER existing `@limit`
    # (`limit(1).sole` must probe 1 row, not 2, so two matching rows
    # settle as the first one rather than `SoleRecordExceeded`; a
    # `limit(0)` relation must probe 0 and always read as
    # `RecordNotFound`), and it unconditionally clears `@records`
    # afterward, which would discard a cache this branch never had to
    # begin with. So the probe borrows and restores `@limit` itself,
    # capped at the relation's own limit (nil meaning the usual 2,
    # clamped at 0 or above), and restores `@records` to nil — what it
    # was, since this branch only runs when the relation is unloaded —
    # rather than to whatever the probe's own LIMIT happened to cache.
    def sole
      loaded = @records
      unless loaded.nil?
        rows = loaded_sole_rows(loaded)
        raise RecordNotFound.new("Couldn't find #{@model.name}", @model.name, @model.primary_key) if rows.length == 0
        raise SoleRecordExceeded, "Wanted only one #{@model.name}" if rows.length > 1
        return rows[0]
      end
      prior_limit = @limit
      @limit = sole_probe_limit(prior_limit)
      rows = to_a
      @limit = prior_limit
      unload_records
      raise RecordNotFound.new("Couldn't find #{@model.name}", @model.name, @model.primary_key) if rows.length == 0
      raise SoleRecordExceeded, "Wanted only one #{@model.name}" if rows.length > 1
      rows[0]
    end

    # `find_sole_by(conditions)` — Rails' `where(conditions).sole`.
    # Shares `with_borrowed_condition` with `find_by` so empty IN cannot
    # sticky-null the receiver after the sole probe (which may raise).
    def find_sole_by(conditions)
      with_borrowed_condition(conditions) { sole }
    end

    # NO `new` HERE, AND NOT BY OVERSIGHT. Rails builds records through
    # a relation (`User.active_bots.new`), but under spinel this class's
    # constructor is already `sp_Relation_new`, so an instance method of
    # that name emits a second definition of the same C symbol and the
    # program does not compile (`conflicting types for
    # 'sp_Relation_new'`). One landed briefly and turned every spinel
    # job red. Both forms are served by a call-site rewrite in
    # lower::scope_chain instead — the association one inside a threaded
    # class-method body, the scope one on the relation receiver itself
    # (`User.active_bots.new` -> `User.new(User.active_bots
    # .scope_attributes)`). Ledgered in docs/pipeline/runtime.md.

    # `first_or_initialize` — the first matching row, or a new unsaved
    # record when there is none. The caller assigns the remaining
    # attributes before `save` (the write path this serves), so the built
    # record starts blank rather than pre-filled from the where-conditions.
    def first_or_initialize
      record = first
      record.nil? ? @model.new : record
    end

    # Rails' `find_or_create_by` — the first row matching `conditions`,
    # or a saved new record carrying them. campfire's `Search.record` is
    # `find_or_create_by(query: query).touch`, reached through
    # `user.searches`, and the SCOPE is the point: the created row must
    # belong to that user, which is what `scope_attributes` carries.
    #
    # The caller's own conditions go on the OUTSIDE of the merge, which
    # is Rails' order — an explicit value wins over the scope's. Same
    # rule `scope_chain::merge_scope_attributes` applies at lower time
    # for the plain constructors; here the merge is the runtime's,
    # because the query half needs the same conditions anyway.
    #
    # Reading `scope_attributes` AFTER the find is safe: `find_by`
    # appends to `@wheres`, and only `where_scope` ever writes the
    # create-seed slot.
    def find_or_create_by(conditions, &block)
      record = find_by(conditions)
      return record if !record.nil?
      created = @model.new(scope_attributes.merge(conditions))
      block.call(created) if block
      created.save
      created
    end

    # Rails' `create_or_find_by`: `transaction(requires_new: true) {
    # create(attributes, &block) }`, and when the INSERT trips a unique
    # index (RecordNotUnique, the savepoint rolled back) the row someone
    # else created: `find_by!(attributes)`, locked (`lock.find_by!`) when
    # a transaction is open around the call. The savepoint is what lets
    # the surrounding PostgreSQL transaction survive the failed INSERT.
    def create_or_find_by(conditions, &block)
      @model.transaction(requires_new: true) { create(conditions, &block) }
    rescue RecordNotUnique
      create_or_find_by_existing(conditions)
    end

    # `create_or_find_by!`: as above with `create!`, so an invalid record
    # raises RecordInvalid instead of coming back unsaved.
    def create_or_find_by!(conditions, &block)
      @model.transaction(requires_new: true) { create!(conditions, &block) }
    rescue RecordNotUnique
      create_or_find_by_existing(conditions)
    end

    def create_or_find_by_existing(conditions)
      if @model.connection.transaction_open?
        where(conditions).lock.find_by!(conditions)
      else
        find_by!(conditions)
      end
    end
    private :create_or_find_by_existing

    # Rails' `find_or_create_by!`: `find_by(attributes) ||
    # create!(attributes, &block)`.
    def find_or_create_by!(conditions, &block)
      record = find_by(conditions)
      return record if !record.nil?
      created = build(conditions)
      block.call(created) if block
      raise RecordInvalid, created unless created.save
      created
    end

    # Rails' `find_or_initialize_by`: `find_by(attributes) ||
    # new(attributes, &block)`, the new record unsaved and carrying the
    # relation's scope attributes and the given ones. Core's
    # `ThemeTemplate.find_or_initialize_by_name_normalized` falls back to
    # `find_or_initialize_by(shop_id:, prefix:, name:)`.
    def find_or_initialize_by(conditions, &block)
      record = find_by(conditions)
      return record if !record.nil?
      built = build(conditions)
      block.call(built) if block
      built
    end

    # ---- SQL composition --------------------------------------------

    # Rails' `to_sql`: the SELECT with every value written in, which is
    # what a caller printing or splicing the SQL expects. Terminals here
    # read `bound_sql` instead, whose hash-condition values are still
    # marked for binding (see `bind_literal`).
    def to_sql
      Relation.inline_sql(bound_sql)
    end

    def bound_sql
      select_sql_with("#{@table}.*")
    end

    # This relation rendered as a CONDITION value — `where(id: other)`
    # and `excluding(other)` both put one relation inside another, and a
    # subquery must project exactly ONE column. Rails projects the
    # primary key when no explicit `select` was given; `to_sql`'s
    # `<table>.*` is right at top level and wrong here, and sqlite says
    # so out loud: "sub-select returns 5 columns - expected 1".
    def to_subquery_sql
      select_sql_with("#{@table}.#{@model.primary_key}")
    end

    # Shared body. `default_cols` is the projection when the caller
    # never said `select(...)`; an explicit one always wins.
    def select_sql_with(default_cols)
      cols = @select_sql.nil? ? default_cols : @select_sql
      distinct = @distinct ? "DISTINCT " : ""
      sql = "#{cte_prefix}SELECT #{distinct}#{cols} FROM #{from_source}"
      sql = "#{sql} #{@joins.join(" ")}" if @joins.length > 0
      sql = "#{sql} WHERE #{@wheres.join(" AND ")}" if @wheres.length > 0
      sql = "#{sql} GROUP BY #{@groups.join(", ")}" if @groups.length > 0
      sql = "#{sql} HAVING #{@havings.join(" AND ")}" if @havings.length > 0
      sql = "#{sql} ORDER BY #{@orders.join(", ")}" if @orders.length > 0
      if !@limit.nil?
        sql = "#{sql} LIMIT #{@limit}"
      elsif !@offset.nil?
        # SQLite needs LIMIT even for offset-only pagination.
        sql = "#{sql} LIMIT -1"
      end
      sql = "#{sql} OFFSET #{@offset}" unless @offset.nil?
      sql = "#{sql} #{@lock}" unless @lock.nil?
      sql
    end

    # JOIN + WHERE only — shared by count_sql / exists_sql so DISTINCT
    # and GROUP arms do not re-paste the ladder.
    def append_join_where(sql)
      sql = "#{sql} #{@joins.join(" ")}" if @joins.length > 0
      sql = "#{sql} WHERE #{@wheres.join(" AND ")}" if @wheres.length > 0
      sql
    end

    def append_group_having(sql)
      sql = "#{sql} GROUP BY #{@groups.join(", ")}" if @groups.length > 0
      sql = "#{sql} HAVING #{@havings.join(" AND ")}" if @havings.length > 0
      sql
    end

    def count_sql
      Relation.inline_sql(bound_count_sql)
    end

    def bound_count_sql
      # DISTINCT / GROUP BY must count the result-set shape, not the
      # underlying rows (#343). Scalar `count` on a grouped relation
      # counts groups; the Hash form is `group_count`. LIMIT/OFFSET
      # stay off this SQL so a windowed relation still answers the
      # unwindowed total.
      if !@groups.empty?
        # Keep an explicit projection so HAVING can name selected
        # aliases (`select("COUNT(*) AS n").having("n > 1")`).
        cols = @select_sql.nil? ? "1 AS one" : @select_sql
        dist = @distinct ? "DISTINCT " : ""
        inner = append_group_having(
          append_join_where("#{cte_prefix}SELECT #{dist}#{cols} FROM #{from_source}")
        )
        return "SELECT COUNT(*) AS n FROM (#{inner}) AS __rh_count"
      end
      if @distinct
        inner = append_join_where(
          "#{cte_prefix}SELECT DISTINCT #{distinct_count_columns} FROM #{from_source}"
        )
        return "SELECT COUNT(*) AS n FROM (#{inner}) AS __rh_count"
      end
      append_join_where("#{cte_prefix}SELECT COUNT(*) AS n FROM #{from_source}")
    end

    # Projection COUNT(DISTINCT …) / `SELECT DISTINCT` use for a
    # distinct relation: an explicit `select` list when present, else
    # the primary key, qualified against the active FROM source.
    def distinct_count_columns
      if !@select_sql.nil?
        @select_sql
      elsif @from.nil?
        "#{@table}.#{@model.primary_key}"
      elsif @joins.length > 0
        # Bare pk is ambiguous once another joined table also has
        # that column (`from("parents").joins(...).distinct.count`).
        "#{from_source}.#{@model.primary_key}"
      else
        @model.primary_key.to_s
      end
    end

    # `SELECT 1 AS one … LIMIT n` for existence probes. Drops ORDER BY
    # and never hydrates. Keeps joins / WHERE / GROUP / HAVING / OFFSET.
    def exists_sql(n)
      Relation.inline_sql(bound_exists_sql(n))
    end

    def bound_exists_sql(n)
      # DISTINCT 1 collapses every row into one — `distinct.many?` would
      # always be false. Project the primary key so each distinct row
      # still occupies a probe slot under LIMIT n.
      cols = if @distinct
        "DISTINCT #{@table}.#{@model.primary_key} AS one"
      else
        "1 AS one"
      end
      sql = append_group_having(
        append_join_where("#{cte_prefix}SELECT #{cols} FROM #{from_source}")
      )
      # Respect an existing relation LIMIT: many?/one? on limit(1) must
      # not look past the window.
      lim = n
      unless @limit.nil?
        lim = @limit < n ? @limit : n
      end
      sql = "#{sql} LIMIT #{lim}"
      sql = "#{sql} OFFSET #{@offset}" unless @offset.nil?
      sql
    end

    # ---- helpers ----------------------------------------------------

    # `WITH RECURSIVE a AS (…), b AS (…) ` or nothing.
    def cte_prefix
      return "" if @ctes.empty?
      "WITH RECURSIVE #{@ctes.join(", ")} "
    end

    # The FROM source: `from(...)`'s, else this model's table.
    def from_source
      src = @from
      src.nil? ? @table : src
    end

    # A hash of conditions ANDed: `{is_deleted: false, user_id: 3}` ->
    # `is_deleted = 0 AND user_id = 3`. Array value -> `IN`, nil ->
    # `IS NULL`, nested Hash -> qualified `table.col = ...`.
    def hash_conditions(hash, note = false)
      parts = []
      named = []
      hash.each do |key, val|
        if val.is_a?(Hash)
          val.each do |col, v|
            predicate = column_predicate("#{key}.#{col}", v)
            parts << predicate
            named << ["#{key}.#{col}", predicate]
          end
        else
          # An alias names its column (`where(subdomain:)`).
          column = @model._attribute_name(key.to_s)
          reflection = association_key_reflection(column)
          if reflection.nil?
            predicate = column_predicate(column, val, note)
            parts << predicate
            named << [column, predicate]
          else
            association_key_predicates(reflection, val, note).each do |col, predicate|
              parts << predicate
              named << [col, predicate]
            end
          end
        end
      end
      @hash_parts = named
      parts.join(" AND ")
    end

    # The `belongs_to` a condition key NAMES, when it is not a column of
    # this model: Rails' predicate builder expands `where(service: record)`
    # to the foreign key (`service_id = record.id`) and, polymorphic, the
    # type column as well (`expand_from_hash`, `table.associated_with?`),
    # through the model's reflections. nil for a column, a name no
    # association has, and a model that answers no reflections.
    def association_key_reflection(column)
      return nil if column.include?(".") || @model.schema_columns.include?(column.to_sym)
      return nil unless @model.respond_to?(:reflect_on_association)
      reflection = @model.reflect_on_association(column.to_sym)
      return nil if reflection.nil? || !reflection.respond_to?(:belongs_to?) || !reflection.belongs_to?
      reflection
    end

    # `[column, predicate]` per column the association key expands to.
    # A polymorphic key names its type too (the record's
    # `polymorphic_name`; one OR group per type for a list of records).
    # A composite foreign key takes one record (its key columns zipped
    # with the association's primary key); a list of them is named as
    # unsupported rather than answered wrong.
    def association_key_predicates(reflection, val, note)
      fk = reflection.foreign_key
      if fk.is_a?(Array)
        raise NotImplementedError, "roundhouse: where(#{reflection.name}: ...) with the composite foreign key " \
          "#{fk.inspect} takes one record or nil" unless val.nil? || val.is_a?(Base)
        pks = reflection.association_primary_key
        pks = [pks] unless pks.is_a?(Array)
        return fk.each_with_index.map do |col, i|
          col = col.to_s
          [col, column_predicate(col, val.nil? ? nil : val.public_send(pks[i]), note)]
        end
      end
      fk = fk.to_s
      return [[fk, column_predicate(fk, val, note)]] unless reflection.polymorphic?
      type = reflection.foreign_type.to_s
      if val.is_a?(Array) || val.is_a?(Relation)
        groups = {}
        (val.is_a?(Relation) ? val.to_a : val).each do |record|
          name = record.is_a?(Base) ? record.class.polymorphic_name : nil
          (groups[name] ||= []) << (record.is_a?(Base) ? record.id : record)
        end
        return [[fk, "1=0"]] if groups.empty?
        ors = groups.map do |name, ids|
          "(#{column_predicate(type, name)} AND #{column_predicate(fk, ids)})"
        end
        return [[fk, ors.length == 1 ? ors[0] : "(#{ors.join(" OR ")})"]]
      end
      name = val.is_a?(Base) ? val.class.polymorphic_name : nil
      [[type, column_predicate(type, name, note)], [fk, column_predicate(fk, val, note)]]
    end

    # Unqualified columns are qualified with this relation's own table
    # (as Rails does for hash conditions) so a condition survives `merge`
    # into a JOINed query where the bare name would be ambiguous —
    # `hidden_stories.user_id`, not `user_id`, after `joins(:hidings)`.
    # Hash predicates name columns, not SQL expressions. Quote each component
    # so PostgreSQL preserves case and reserved words; keep already quoted names.
    def quote_column_path(path)
      path.split(".").map do |part|
        if part.start_with?('"') && part.end_with?('"')
          part
        else
          '"' + part.gsub('"', '""') + '"'
        end
      end.join(".")
    end

    def column_predicate(col, val, note = false)
      # `col` or `table.col`, each part a bare identifier (`sql_ident`'s
      # allowlist) or one already quoted as PostgreSQL writes it
      # (`"Order"`, inner quotes doubled); anything else raises before
      # any SQL is built.
      parts = col.split(".", -1)
      unless parts.size <= 2 && parts.all? { |part| part.match?(/\A(?:[A-Za-z_][A-Za-z0-9_]*|"(?:[^"]|"")+")\z/) }
        raise ArgumentError, "SQL identifier \"" + col + "\" is not a column name"
      end
      qcol = col.include?(".") ? quote_column_path(col) : "#{@table}.#{quote_column_path(col)}"
      # A column of this model with a declared type (`attribute`) binds
      # what the type serializes, as Rails' QueryAttribute does.
      own = col.include?(".") ? nil : col
      if !own.nil? && @model._attribute_force_equality?(own, val)
        # A value the declared type compares whole (a serialized column's
        # `type:` class, an Array or a Hash): predicate_builder.rb's
        # `force_equality?` binds it as one value, not as an IN list.
        "#{qcol} = #{ActiveRecord.adapter.escape_value(@model._attribute_for_database(own, val))}"
      elsif val.is_a?(Relation)
        # A relation value is Rails' subquery form —
        # `where(story_id: Tagging.where(...).select(:story_id))` →
        # `story_id IN (SELECT taggings.story_id FROM taggings …)`.
        # The inner relation renders inline; its values were escaped
        # as its own conditions were added. `to_subquery_sql`, not
        # `to_sql`: a relation with no explicit `select` must project
        # its primary key here, not every column.
        "#{qcol} IN (#{val.to_subquery_sql})"
      elsif val.is_a?(Array)
        # Record elements read their id — Rails' IN-of-records form
        # (`where(comment: comments)`, lobsters Vote.comments_flags);
        # scalar elements escape as-is. An empty list is Rails' none
        # predicate (`1=0`): never `IN ()`. Pure — NullRelation is set by
        # `mark_null!` from `none` / lasting positive `where` only.
        ids = val.map { |x| x.is_a?(Base) ? x.id : x }
        ids = ids.map { |x| @model._attribute_for_database(own, x) } unless own.nil?
        # An empty list matches nothing: Arel renders `1=0` (`IN ()` is a
        # syntax error), which `where.not` negates to match everything.
        return "1=0" if ids.empty?
        "#{qcol} IN (#{escape_list(ids)})"
      elsif val.nil?
        note_equality(own, nil) if note
        "#{qcol} IS NULL"
      elsif val.is_a?(Base)
        # A whole record under a (fk-renamed) key reads its id —
        # `where(user: user)` after the key lowered to `user_id`. The
        # static `v && v.id` narrowing was dropped in favor of this
        # runtime dispatch: hash values from untyped scope params can
        # be a record OR a collection, and only the runtime knows.
        note_equality(own, val.id) if note
        "#{qcol} = #{bind_literal(own.nil? ? val.id : @model._attribute_for_database(own, val.id))}"
      else
        note_equality(own, val) if note
        "#{qcol} = #{bind_literal(own.nil? ? val : @model._attribute_for_database(own, val))}"
      end
    end

    # Rails' `scope_for_create` reads a relation's `where` equalities on
    # its own columns, so `Doc.where(name: "q").new` and
    # `shelf.docs.where(name: "r").create!` come back with `name` set. A
    # list, relation or joined-table value only filters, as in Rails.
    # Reassigned rather than written into: a spawned relation shares the
    # Hash (`copy_scope_attributes`).
    def note_equality(own, val)
      return nil if own.nil?
      col = own.to_sym
      @scope_attributes = @scope_attributes.merge({ col => val }) if @model.schema_columns.include?(col)
      nil
    end

    # For positional binds, split only the original `?` placeholders,
    # keeping escaped values verbatim. Preserve trailing empty parts so
    # missing binds leave their `?` intact. (`String#sub` used to search
    # already-inserted values, so a `?` or backslash in a bind leaked
    # into the next placeholder.)
    def substitute_binds(sql, args)
      first = args[0]
      return substitute_named_binds(sql, first) if args.length == 1 && first.is_a?(Hash)
      parts = sql.split("?", -1)
      result = parts[0].to_s
      index = 0
      while index < parts.length - 1
        if index < args.length
          result = result + bind_value_sql(args[index])
        else
          result = result + "?"
        end
        result = result + parts[index + 1].to_s
        index += 1
      end
      result
    end

    # One `?` bind's SQL: Rails quotes an Array (an `IN (?)` list) as its
    # escaped elements joined by commas, and an empty one as NULL.
    def bind_value_sql(value)
      return ActiveRecord.adapter.escape_value(value) unless value.is_a?(Array)
      return "NULL" if value.empty?
      escape_list(value)
    end

    # Rails' named binds: `having("COUNT(*) = :size AND … IN (:user_ids)",
    # size: 2, user_ids: [3, 5])`, campfire's direct-room lookup
    # (basecamp/once-campfire#310). Each `:name` the Hash answers is
    # replaced by its escaped value, and an Array value by a
    # comma-separated list. A `::` cast is left alone, as is a name the
    # Hash does not have. Before this, the placeholders reached SQLite
    # unbound and read as NULL: every lookup missed and every Ping
    # created a new direct room.
    def substitute_named_binds(sql, binds)
      out = ""
      i = 0
      n = sql.length
      while i < n
        ch = sql[i, 1].to_s
        prev = i > 0 ? sql[i - 1, 1].to_s : ""
        nxt = sql[i + 1, 1].to_s
        if ch == ":" && prev != ":" && nxt != ":" && named_bind_start?(nxt)
          j = i + 1
          j += 1 while j < n && named_bind_char?(sql[j, 1].to_s)
          name = sql[i + 1, j - i - 1].to_s
          key = name.to_sym
          if binds.key?(key)
            value = binds[key]
            out = out + (value.is_a?(Array) ? bind_value_sql(value) : ActiveRecord.adapter.escape_value(value))
            i = j
            next
          end
        end
        out = out + ch
        i += 1
      end
      out
    end

    def named_bind_start?(ch)
      (ch >= "a" && ch <= "z") || (ch >= "A" && ch <= "Z") || ch == "_"
    end

    def named_bind_char?(ch)
      named_bind_start?(ch) || (ch >= "0" && ch <= "9")
    end

    def escape_list(vals)
      out = []
      vals.each { |v| out << ActiveRecord.adapter.escape_value(v) }
      out.join(", ")
    end

    # ---- bound values -----------------------------------------------
    #
    # Rails binds a hash condition's value (`where(is_deleted: false)`
    # sends `is_deleted = ?`), and SQLite plans a bound statement
    # differently from one with the value written in. lobsters' comments
    # page is the case that made it matter: with `is_deleted = 0` and
    # `is_moderated = 0` inline, SQLite built a bloom filter over every
    # story before a LIMIT 20 scan, 61x the VM steps of Rails' bound
    # statement for the same rows.
    #
    # Conditions here are SQL text that `or`, `not`, `merge` and
    # subqueries splice together, so a value travels inside that text:
    # its escaped literal between \x01 and \x02. The adapter lifts the
    # marks to placeholders when it runs the read (`lift_binds`); every
    # other exit (`to_sql`, writes) drops them (`inline_sql`), which is
    # the SQL this class wrote before. A literal holding either control
    # byte is left unmarked, so a mark is always ours.
    #
    # Only these sites mark: hash-condition scalars and primary-key
    # lookups, the values Rails binds. String conditions
    # (`where("a = ?", x)`) and IN lists stay inline, as Rails writes
    # them.
    def bind_literal(val)
      text = ActiveRecord.adapter.escape_value(val)
      return text if text.include?("\x01") || text.include?("\x02")
      "\x01" + text + "\x02"
    end

    # The SQL with every mark replaced by its literal.
    def self.inline_sql(sql)
      return sql unless sql.include?("\x01")
      lift_binds(sql)[1].to_s
    end

    # `[bound, inline, kind, value, kind, value, …]`: `bound` has a `?`
    # where each liftable mark was, `inline` has the literal, and each
    # pair says how to bind one placeholder, in order — "i" an integer,
    # "s" a string. A mark holding any other literal (NULL, a float,
    # an X'' blob) stays inline in both. String literals, quoted
    # identifiers and comments, where inline user text can sit, are
    # copied whole, so a control byte inside one is never read as a
    # mark. The fast path below still skips quoted identifiers (a
    # model's column list has dozens; scanning each doubled the cost of
    # every hydrating read) — the scan path must still honor them so a
    # `--` or `/*` inside `"…"` is not treated as a comment that
    # swallows later marks (sql_identifiers InjectionPurchase).
    #
    # `numbered` writes PostgreSQL's `$1`, `$2`, … where `?` stands, for
    # a driver whose placeholders are numbered; a `?` that was already
    # in the text (a literal, a JSON operator) is never touched.
    def self.lift_binds(sql, numbered = false)
      # Fast path: split at the marks, and know each mark sits outside a
      # string literal because the text before it holds an even number
      # of quotes (a literal opens and closes with one, and doubles any
      # inside). Comments, an odd count or a torn mark take the scan.
      return lift_binds_scan(sql, numbered) if sql.include?("--") || sql.include?("/*")
      pieces = sql.split("\x01", -1)
      out = ["", ""]
      head = pieces[0].to_s
      bound = [head]
      inline = [head]
      quotes = quote_count(head)
      i = 1
      while i < pieces.length
        piece = pieces[i].to_s
        close = piece.index("\x02")
        return lift_binds_scan(sql, numbered) if close.nil? || quotes % 2 == 1
        text = piece[0, close].to_s
        rest = piece[close + 1, piece.length - close - 1].to_s
        kind = bind_kind(text)
        if kind == ""
          bound << text
        else
          bound << bind_slot(out, numbered)
          out << kind
          out << (kind == "s" ? text[1, text.length - 2].to_s.gsub("''", "'") : text)
        end
        inline << text
        bound << rest
        inline << rest
        quotes += quote_count(rest)
        i += 1
      end
      out[0] = bound.join
      out[1] = inline.join
      out
    end

    # The placeholder for the next bind `out` will hold.
    def self.bind_slot(out, numbered)
      return "?" unless numbered
      "$" + ((out.length - 2) / 2 + 1).to_s
    end

    def self.quote_count(text)
      n = 0
      at = text.index("'")
      until at.nil?
        n += 1
        at = text.index("'", at + 1)
      end
      n
    end

    # The general lift: a scan that copies string literals, quoted
    # identifiers and comments whole.
    def self.lift_binds_scan(sql, numbered = false)
      out = ["", ""]
      bound = []
      inline = []
      # Each token's next position is searched once and reused until the
      # scan passes it: one pass over the text per token kind. `"` is
      # required so `--` / `/*` inside a quoted identifier are not
      # comments (see lift_binds); the fast path omits it on purpose.
      toks = ["'", "\"", "\x01", "--", "/*"]
      nexts = [-1, -1, -1, -1, -1]
      i = 0
      n = sql.length
      while i < n
        j = n
        t = 0
        while t < toks.length
          at = nexts[t]
          if at < i
            found = sql.index(toks[t], i)
            at = found.nil? ? n : found
            nexts[t] = at
          end
          j = at if at < j
          t += 1
        end
        if j > i
          seg = sql[i, j - i].to_s
          bound << seg
          inline << seg
        end
        break if j >= n
        ch = sql[j, 1].to_s
        k = n
        if ch == "\x01"
          close = sql.index("\x02", j + 1)
          k = close.nil? ? n : close + 1
          text = sql[j + 1, k - j - 2].to_s
          kind = bind_kind(text)
          if kind == ""
            bound << text
          else
            bound << bind_slot(out, numbered)
            out << kind
            out << (kind == "s" ? text[1, text.length - 2].to_s.gsub("''", "'") : text)
          end
          inline << text
        else
          k = sql_token_end(sql, j, ch, n)
          seg = sql[j, k - j].to_s
          bound << seg
          inline << seg
        end
        i = k
      end
      out[0] = bound.join
      out[1] = inline.join
      out
    end

    # One past the end of the string literal, quoted identifier, or
    # comment opening at `j` (a doubled quote stays inside the token).
    def self.sql_token_end(sql, j, ch, n)
      if ch == "-"
        nl = sql.index("\n", j)
        return nl.nil? ? n : nl + 1
      end
      if ch == "/"
        close = sql.index("*/", j + 2)
        return close.nil? ? n : close + 2
      end
      k = j + 1
      while k < n
        at = sql.index(ch, k)
        return n if at.nil?
        return at + 1 unless sql[at + 1, 1].to_s == ch
        k = at + 2
      end
      n
    end

    # "i" for an integer literal that fits 64 bits, "s" for a plain
    # quoted string, "" for anything to leave inline.
    def self.bind_kind(text)
      len = text.length
      return "" if len == 0
      if text[0, 1].to_s == "'"
        return "" if len < 2 || text[len - 1, 1].to_s != "'"
        return "" if text[1, len - 2].to_s.gsub("''", "").include?("'")
        return "s"
      end
      digits = text[0, 1].to_s == "-" ? text[1, len - 1].to_s : text
      return "" if digits.length == 0 || digits.length > 18
      k = 0
      while k < digits.length
        c = digits[k, 1].to_s
        return "" if c < "0" || c > "9"
        k += 1
      end
      "i"
    end

    # Rails' `sanitize_limit`: `Integer(n)`. An integer, or a String
    # spelling one (surrounding space allowed), is that integer; a Float
    # truncates; anything else is the ArgumentError `Integer()` raises —
    # never text in the LIMIT clause.
    def sql_limit(n)
      return n.to_i if n.is_a?(Float)
      text = n.to_s.strip
      unless text.match?(/\A[+-]?\d+\z/)
        raise ArgumentError, "invalid value for Integer(): \"" + n.to_s + "\""
      end
      text.to_i
    end

    # An `order` hash's direction: Rails' `VALID_DIRECTIONS`, else the
    # ArgumentError `validate_order_args` raises.
    def order_direction(dir)
      d = dir.to_s
      return d.upcase if d == "asc" || d == "desc" || d == "ASC" || d == "DESC"
      raise ArgumentError, "Direction \"" + d + "\" is invalid. Valid directions are: " \
        "[:asc, :desc, :ASC, :DESC, \"asc\", \"desc\", \"ASC\", \"DESC\"]"
    end

    # An identifier written into SQL: `col` or `table.col`. This runtime
    # writes names bare, so anything else is ArgumentError rather than a
    # fragment.
    def sql_ident(name)
      c = name.to_s
      unless c.match?(/\A[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)?\z/)
        raise ArgumentError, "SQL identifier \"" + c + "\" is not a column name"
      end
      c
    end

    # An `order` hash's KEY. Same allowlist as `sql_ident`.
    def order_hash_column(col)
      sql_ident(col)
    end

    # Developer SQL like campfire's `order("LOWER(name)")` /
    # `order("LOWER(rooms.name)")`, plus the documented zero-arg
    # `RANDOM()` / `random()`. A request-steered fragment such as
    # `(SELECT 1)` or `SLEEP()` does not match.
    def order_fn_term?(c)
      return true if c == "RANDOM()" || c == "random()"
      c.match?(/\A[A-Za-z_][A-Za-z0-9_]*\([A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)?\)\z/)
    end

    # A string `order` argument: comma-separated `col` / `table.col`
    # with optional ASC/DESC. Static corpus strings (`"id desc"`,
    # `"category asc, tags.tag asc"`) pass; `order(params[:sort])` with
    # SQL does not.
    def order_string(s)
      bits = s.split(",")
      raise ArgumentError, "Order \"" + s + "\" is not a column name" if bits.length == 0
      parts = []
      bits.each do |bit|
        words = bit.strip.split(/\s+/)
        if words.length == 0 || words.length > 2
          raise ArgumentError, "Order \"" + s + "\" is not a column name"
        end
        col = words[0]
        term = order_fn_term?(col) ? col : order_hash_column(col)
        if words.length == 2
          parts << "#{term} #{order_direction(words[1])}"
        else
          parts << term
        end
      end
      parts.join(", ")
    end

    # `order(:col)` / `order("col DESC")` / `order(col: :desc)` /
    # `order(rooms: { updated_at: :desc })` — Rails' nested-hash form
    # for a table-qualified column (campfire's direct-room sidebar).
    # Nested values go through `sql_ident` + `order_direction` so a
    # request-steered hash cannot splice SQL.
    def order_term(p)
      if p.is_a?(Hash)
        format_order_hash(p)
      else
        # A Symbol names an attribute, an alias its column
        # (`order(:subdomain)`); a String is SQL.
        s = p.to_s
        order_string(p.is_a?(Symbol) ? @model._attribute_name(s) : s)
      end
    end

    # Isolated so `each` sees `Hash[Symbol, untyped]` keys as Symbol.
    # `order_term`'s Hash|String union does not narrow, and untyped
    # `col` interpolations blow the Bar B ceiling.
    def format_order_hash(h)
      given = h #: Hash[Symbol, untyped]
      parts = []
      given.each do |col, dir|
        if dir.is_a?(Hash)
          inner_col = dir.keys[0]
          parts << "#{sql_ident(col)}.#{sql_ident(inner_col)} #{order_direction(dir[inner_col])}"
        else
          parts << "#{sql_ident(@model._attribute_name(col.to_s))} #{order_direction(dir)}"
        end
      end
      parts.join(", ")
    end
  end
end
