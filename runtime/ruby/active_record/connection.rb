# Ruby-family model surfaces: raw-SQL connections and request-key finders.
#
# Rails hands back the adapter itself here; this runtime hands back a
# thin stateless facade over the per-target `Db` primitive shim — just
# the members the corpus reaches for when it drops below the Relation
# layer (lobsters' Keystore upserts, hand-written aggregate queries,
# `quote`/`quote_string` in SQL-building helpers). Statically
# resolvable by construction: fixed methods, no method_missing.
#
# Result rows are `Hash[String, untyped]` — raw SQL is the one place
# the row shape is genuinely dynamic (aliased aggregates, computed
# columns), so a typed bag is the honest contract rather than an
# avoidable erasure.
module ActiveRecord
  # Integer finder inputs must be validated before entering a typed adapter.
  # Keep validity separate from the Integer payload: every signed 64-bit
  # value, including INT64_MIN, is a valid payload, never a nil sentinel.
  # The current SQLite adapter has signed 64-bit integer keys even for
  # schema.rb `id: :integer`; narrower adapter widths need their own metadata.
  class IntegerKeyCast
    attr_reader :valid, :value

    # Validity is separate so every signed Integer can remain a payload.
    def initialize(valid, value)
      @valid = valid
      @value = value
    end

    # Preserve String keys; ordinary scalar inputs use their decimal text.
    def self.input_text(id)
      if id.nil?
        ""
      elsif id.is_a?(String)
        id
      else
        id.to_s
      end
    end

    # Apply Rails decimal-prefix rules and the adapter range before to_i.
    def self.parse(id)
      # An Integer already has a native representation. Keep it intact,
      # including MIN, rather than round-tripping through decimal formatting.
      if id.is_a?(Integer)
        return IntegerKeyCast.new(false, 0) if id < -9223372036854775808
        return IntegerKeyCast.new(false, 0) if id > 9223372036854775807
        return IntegerKeyCast.new(true, id)
      elsif id.is_a?(Float)
        # Numeric inputs truncate their VALUE, not a scientific-notation
        # String prefix. Use an exclusive 2**63 upper bound: Float(MAX)
        # rounds up to 2**63. These comparisons also reject NaN/infinities
        # before to_i, while preserving the exactly representable MIN.
        return IntegerKeyCast.new(false, 0) unless id >= -9223372036854775808.0 && id < 9223372036854775808.0
        return IntegerKeyCast.new(true, id.to_i)
      end
      text = input_text(id)
      i = number_start(text)
      negative = text[i, 1].to_s == "-"
      if negative || text[i, 1].to_s == "+"
        i += 1
      end
      digits = +""
      found = false
      while i < text.length
        char = text[i, 1].to_s
        if digit_value(char) >= 0
          digits = digits + char unless digits.empty? && char == "0"
          return IntegerKeyCast.new(false, 0) if digits.length > 19
          found = true
          i += 1
        elsif char == "_" && found && digit_value(text[i + 1, 1].to_s) >= 0
          # Ruby's decimal to_i accepts single underscores BETWEEN digits.
          # "1_0" is 10; "1__0" stops at the first underscore and is 1.
          i += 1
        else
          break
        end
      end
      return IntegerKeyCast.new(false, 0) unless found
      digits = "0" if digits.empty?
      limit = negative ? "9223372036854775808" : "9223372036854775807"
      return IntegerKeyCast.new(false, 0) if exceeds_limit(digits, limit)
      # Apply the sign BEFORE conversion: +2**63 is not representable,
      # while -2**63 is. Native to_i must never see an overflowing input.
      signed = negative ? "-" + digits : digits
      IntegerKeyCast.new(true, signed.to_i)
    end

    # Locate the optional sign/digits after Ruby-compatible ASCII whitespace.
    def self.number_start(text)
      i = 0
      while i < text.length
        char = text[i, 1].to_s
        if char != " " && char != "\t" && char != "\n" &&
           char != "\r" && char != "\v" && char != "\f"
          return i
        end
        i += 1
      end
      i
    end

    # Decimal ASCII digit, or -1; Unicode numeric characters are not IDs.
    def self.digit_value(char)
      digits = "0123456789"
      i = 0
      while i < digits.length
        return i if digits[i, 1].to_s == char
        i += 1
      end
      -1
    end

    # Compare normalized unsigned text without overflowing a native Integer.
    def self.exceeds_limit(digits, limit)
      return true if digits.length > limit.length
      return false if digits.length < limit.length
      i = 0
      while i < digits.length
        value = digit_value(digits[i, 1].to_s)
        bound = digit_value(limit[i, 1].to_s)
        return true if value > bound
        return false if value < bound
        i += 1
      end
      false
    end
  end

  # Row set from `Connection#execute` / `#exec_query`. Mirrors the
  # slice of `ActiveRecord::Result` the corpus uses: `to_a`, `first`,
  # `each`, `rows`.
  class Result
    def initialize(rows)
      @rows = rows
    end

    def rows
      @rows
    end

    def to_a
      @rows
    end

    def first
      @rows.first
    end

    def each
      @rows.each do |row|
        yield row
      end
      @rows
    end
  end

  class Connection
    # This runtime's only backend. Lobsters branches on this to pick
    # its upsert dialect; the SQLite arm is the one we execute.
    def adapter_name
      "SQLite"
    end

    # Rails `quote`: a full SQL literal, quotes included for strings.
    # `Db.escape_string` already wraps in single quotes (sqlite literal
    # syntax with '' doubling).
    def quote(value)
      if value.nil?
        "NULL"
      elsif value.is_a?(Integer) || value.is_a?(Float)
        value.to_s
      elsif value.is_a?(TrueClass)
        "1"
      elsif value.is_a?(FalseClass)
        "0"
      else
        Db.escape_string(value.to_s)
      end
    end

    # Rails `quote_string`: escaped but UNquoted (callers embed it
    # inside their own quotes).
    def quote_string(str)
      str.gsub("'", "''")
    end

    # Run raw SQL, collecting every row as name→value. DML statements
    # simply produce zero rows. Delegates to the adapter's row loop
    # (which resolves column names once, not rows×cols times).
    def execute(sql)
      Result.new(ActiveRecord.adapter.select_rows(sql))
    end

    # `select_rows(sql)` — Rails' rows as Arrays of values, in column
    # order (campfire's tests read an `EXPLAIN QUERY PLAN`'s detail with
    # `select_rows(…).map(&:last)`).
    def select_rows(sql)
      ActiveRecord.adapter.select_rows(sql).map { |row| row.values }
    end

    # Rails' DatabaseStatements#select_values / #select_value: the first
    # column of every row, and the first column of the first row (nil
    # for no rows). `name` is Rails' log label.
    def select_values(sql, name = nil)
      select_rows(sql).map(&:first)
    end

    def select_value(sql, name = nil)
      row = select_rows(sql).first
      row && row.first
    end

    def exec_query(sql)
      execute(sql)
    end

    # `select_all(sql)` — the same rows under the name Rails' query
    # interface gives them (lobsters' TrafficHelper reads its activity
    # range this way).
    def select_all(sql)
      execute(sql)
    end

    # Rails' `exec_update(sql, name, binds)` / `exec_delete`: DML with
    # positional `?` binds, answering the rows affected. lobsters
    # recomputes a comment's score this way (`UPDATE comments SET …
    # confidence_order = unhex(?) WHERE id = ?`), on every comment and
    # vote save. The binds go through `sanitize_sql`, the escaping every
    # other raw-SQL entry here already uses; `name` is Rails' log label.
    def exec_update(sql, name = nil, binds = [])
      ActiveRecord.adapter.execute_ddl(Base.sanitize_sql([sql] + binds))
      ActiveRecord.adapter.changes
    end

    # lobsters' FullTextSearch drops an index row this way
    # (`DELETE FROM … where rowid = ?`).
    def exec_delete(sql, name = nil, binds = [])
      exec_update(sql, name, binds)
    end

    # lobsters' FullTextSearch adds an index row this way (`INSERT INTO …
    # (rowid, …) values (?, …)`). Rails answers a Result; an INSERT
    # selects nothing, so it is an empty one.
    def exec_insert(sql, name = nil, binds = [])
      exec_update(sql, name, binds)
      Result.new([])
    end
  end

  # The Base half of the Ruby-family model surface. Lives HERE (not base.rb)
  # deliberately: base.rb is transpiled into every strict target's
  # runtime via the runtime_loader tables, and this surface uses
  # begin/rescue (which several emitters don't lower yet) and the
  # Connection class (which those tables don't ship). This file is
  # walked only into the ruby-family trees, and active_record.rb
  # requires it AFTER base.rb so the reopen sees the real class.
  class Base
    # Deferred `includes`/`preload` (ActiveRecord::PendingPreload): the
    # Relation that loaded this record hands it the group's pending
    # preload, and the first association read on any record of the group
    # runs it. Every emitted association reader (and `<assoc>_loaded?`)
    # starts with `_await_preload` (lower::deferred_preload); on a record
    # with nothing pending it is one nil check.
    def _pend_preload(pending)
      @__pending_preload = pending
      nil
    end

    def _await_preload
      pending = @__pending_preload
      return nil if pending.nil?
      @__pending_preload = nil
      pending.run
      nil
    end

    # Keep public input intact until schema-selected normalization, then raise
    # the same RecordNotFound for an invalid key or an absent record.
    # Reject nil before a key-typed adapter can coerce it to a real
    # zero/empty-string key.
    def self.find(id)
      return _find_composite_primary_key(id) if composite_primary_key?
      raise RecordNotFound, "Couldn't find #{name} with id=#{id}" if id.nil?
      return ActiveRecord::Relation.new(self).find(id) if _default_scope_applies?
      result = _find_primary_key_input(id)
      raise RecordNotFound, "Couldn't find #{name} with id=#{id}" if result.nil?
      result
    end

    # Rails' find on a composite primary key (core.rb, finder_methods.rb):
    # one key tuple finds one record, an Array of tuples one per tuple,
    # in order; a missing one raises.
    def self._find_composite_primary_key(id)
      columns = composite_query_constraints_list
      values = [] #: Array[untyped]
      values.concat(id) if id.is_a?(Array)
      if values.empty?
        raise ArgumentError, "Expected an Array of #{columns.inspect} values, got #{id.inspect}"
      end
      return values.map { |key| _find_composite_primary_key(key) } if values.first.is_a?(Array)
      conditions = {}
      columns.each_with_index { |column, i| conditions[column.to_sym] = values[i] }
      record = where(conditions).take
      raise RecordNotFound, "Couldn't find #{name} with #{columns.inspect}=#{values.inspect}" if record.nil?
      record
    end

    # Rails' `composite_primary_key?`: models on a table keyed on several
    # columns that declare no `primary_key` override it (emitted), with
    # `primary_key` as the column list and `id` as the tuple.
    def self.composite_primary_key?
      false
    end

    # Schema-generated models override this with one normalization path
    # before their scalar adapter. The fallback retains the legacy Ruby
    # contract for hand-written subclasses using the generic adapter.
    def self._find_primary_key_input(id)
      key = _cast_primary_key(id)
      return nil if key.nil?
      _adapter_find_by_id(key)
    end

    # Relation and legacy-adapter finders use this shared conversion.
    # The integer parser never converts an out-of-range decimal string.
    def self._cast_primary_key(id)
      return nil if id.nil?
      key_type = _declared_attribute_types[primary_key]
      return key_type.cast(id) if key_type
      return IntegerKeyCast.input_text(id) if _string_primary_key
      cast = IntegerKeyCast.parse(id)
      cast.valid ? cast.value : nil
    end

    # Reject nil before a key-typed adapter can coerce it. Generated
    # models override `_exists_primary_key_input` with schema-selected
    # dispatch (same split as find) so Spinel never compiles String into
    # an Integer adapter slot. No argument asks for any row (Rails'
    # `exists?(conditions = :none)`); the default is a private sentinel so
    # the typed signature does not widen.
    def self.exists?(id = EXISTS_ANY_ROW)
      return false if id.nil?
      return all.exists? if EXISTS_ANY_ROW.equal?(id)
      return all.exists?(id) if _default_scope_applies?
      _exists_primary_key_input(id)
    end
    EXISTS_ANY_ROW = Object.new.freeze
    private_constant :EXISTS_ANY_ROW

    # Fallback for hand-written subclasses using the generic adapter.
    def self._exists_primary_key_input(id)
      key = _cast_primary_key(id)
      return false if key.nil?
      _adapter_exists_by_id?(key)
    end

    # Stateless facade — every member delegates straight to `Db`, so a
    # fresh instance per call is cheap and dodges class-ivar state.
    def self.connection
      ActiveRecord::Connection.new
    end

    # Rails' `Model.sanitize_sql(["insert … values (?, ?)", a, b])` —
    # the array form, which is the one an app writes when it drops
    # below the Relation layer (campfire's `Message::Searchable` keeps
    # its FTS index this way). The head is the fragment and the tail
    # are its binds, escaped left to right, which is exactly what
    # `Relation`'s `where("… ?", x)` already does — same rule, reached
    # from the class instead of from a relation.
    #
    # HERE and not in `base.rb`: this file is the raw-SQL tier the
    # strict targets do not stage, and `sanitize_sql` belongs to it
    # twice over — it escapes through `ActiveRecord.adapter
    # .escape_value`, which is on the RBS adapter contract but NOT on
    # the narrower hand-written Go/Rust adapter interfaces, and its one
    # caller reaches it through `connection.execute`, which lives here
    # too. Put in base.rb it broke `go vet` on every fixture.
    #
    # SPLIT-and-interleave rather than the `sub`-per-bind loop
    # `Relation#substitute_binds` uses: `String#sub` is Ruby vocabulary
    # the Go emitter does not carry either ("sql.Sub undefined"), and
    # Relation gets away with it only because Go does not stage that
    # file. `split` every target speaks.
    #
    # The BIND COUNT is authoritative, not the placeholder count: Ruby
    # drops a trailing empty field, so `"… where rowid = ?"` splits to
    # one part, and appending a bind after each part while binds remain
    # reconstructs it exactly. A `?` with no bind left is dropped, which
    # is the same shape `substitute_binds` leaves it in. Question marks
    # inside single- or double-quoted SQL literals are not placeholders.
    # SQLite does not treat `\` as a string escape — a backslash is
    # literal, so `'\''` ends the quote at the second apostrophe.
    def self.sanitize_sql(statement)
      sql = statement[0].to_s
      out = ""
      bind = 1
      i = 0
      n = sql.length
      quote = nil
      while i < n
        c = sql[i, 1].to_s
        if !quote.nil?
          out = out + c
          quote = nil if c == quote
          i = i + 1
          next
        end
        if c == "'" || c == "\""
          quote = c
          out = out + c
          i = i + 1
          next
        end
        if c == "?"
          if bind < statement.length
            out = out + ActiveRecord.adapter.escape_value(statement[bind])
            bind = bind + 1
          end
          i = i + 1
          next
        end
        out = out + c
        i = i + 1
      end
      out
    end

    # Rails' array-form entry point (`sanitize_sql_array([...])`). Same
    # positional `?` interleave as `sanitize_sql` — apps that name the
    # `_array` form (raw upserts, hand-built fragments) must resolve
    # here (#400). Named Hash / `%s` binds are not modeled yet (would
    # raise the Bar B untyped residual via Hash[untyped] walks).
    def self.sanitize_sql_array(statement)
      sanitize_sql(statement)
    end

    # sanitization.rb's sanitize_sql_like: `string` with each `%` and `_`
    # (and the escape character itself) escaped for a LIKE pattern.
    def self.sanitize_sql_like(string, escape_character = "\\")
      if string.include?(escape_character) && escape_character != "%" && escape_character != "_"
        string = string.gsub(escape_character, '\0\0')
      end

      string.gsub(/(?=[%_])/, escape_character)
    end

    # `Model.transaction { ... }` — the block inside BEGIN/COMMIT, with
    # ROLLBACK + re-raise on any exception. Flat transactions only: the
    # corpus never nests (a nested BEGIN would error in SQLite rather
    # than silently join, which is the honest failure).
    #
    # `isolation:`, `requires_new:`, and `joinable:` are Rails'
    # `DatabaseStatements#transaction` keyword options (the same three
    # `with_lock` forwards — see base.rb). All three are accepted and
    # ignored: no isolation levels, and no SAVEPOINT-backed nesting
    # under this flat implementation. They exist on the signature so a
    # call that passes them (directly, or via `with_lock`) doesn't
    # raise `ArgumentError`.
    def self.transaction(isolation: nil, requires_new: nil, joinable: true)
      Db.exec("BEGIN")
      begin
        result = yield
        Db.exec("COMMIT")
        result
      rescue => e
        Db.exec("ROLLBACK")
        raise e
      end
    end

    # `Model.delete_all` for a model without the lowerer-emitted
    # override: the rows the DELETE removed, read off the statement
    # rather than counted beforehand. Ruby-family-only because
    # `changes` is — see base.rb's default.
    def self.delete_all
      ActiveRecord.adapter.delete_all(table_name)
      ActiveRecord.adapter.changes
    end

    # `Model.update_counters(id, col: delta, …)` — atomic column
    # increments (`col = col + delta`) on one row, skipping validations
    # and callbacks. Returns the affected-row count.
    def self.update_counters(id, counters)
      parts = []
      counters.each do |col, delta|
        parts.push("#{col} = #{col} + #{delta.to_i}")
      end
      sql = "UPDATE #{table_name} SET #{parts.join(", ")} WHERE id = #{ActiveRecord.adapter.escape_value(id)}"
      ActiveRecord.adapter.execute_ddl(sql)
      ActiveRecord.adapter.changes
    end

    # `Model.upsert(attrs, …)` — INSERT that folds into an UPDATE when it
    # collides, in one statement. Rails routes the single-row form
    # through `upsert_all`; so does this.
    def self.upsert(attrs, unique_by: nil, on_duplicate: nil, returning: nil)
      upsert_all([attrs], unique_by: unique_by, on_duplicate: on_duplicate, returning: returning)
    end

    # The predicate of the unique index on `columns` (sorted, joined with
    # ", ") when that index is partial, else "". The lowering overrides it
    # for a model whose table has one. `upsert_all` adds it to the
    # conflict target, as Rails does: SQLite matches a partial index only
    # through its `WHERE`.
    def self._conflict_predicate(columns)
      ""
    end

    # `Model.upsert_all(rows, …)` → SQLite's
    # `INSERT … ON CONFLICT (target) DO UPDATE SET …`.
    #
    # The conflict target is `unique_by` when given, else the model's
    # `primary_key` — which is why that had to become a real per-model
    # value rather than an assumed `id`. Lobsters' Keystore is the case
    # in point: conflicting on `id` would insert a fresh autoincrement
    # row every call and then trip the UNIQUE index on `key`.
    #
    # `on_duplicate:` replaces the generated SET clause with a raw
    # fragment (`Arel.sql("value = value + 1")` — Arel.sql is the
    # identity here, so it arrives as a String). Otherwise every
    # non-conflict column is assigned from `excluded`, matching Rails.
    #
    # NOT Rails-complete, deliberately: no RETURNING (asking for it
    # raises rather than quietly handing back nothing) and no
    # `record_timestamps` stamping — no corpus model upserts a table
    # that has timestamps. Returns the affected-row count, the same
    # currency `update_counters` deals in.
    def self.upsert_all(rows, unique_by: nil, on_duplicate: nil, returning: nil)
      return 0 if rows.length == 0
      if returning
        raise NotImplementedError, "#{name}.upsert_all: RETURNING is not supported"
      end

      cols = rows[0].keys
      target = unique_by.nil? ? primary_key : unique_by
      target_names = target.is_a?(Array) ? target.map { |c| c.to_s } : [target.to_s]

      tuples = rows.map do |row|
        "(" + cols.map { |c| ActiveRecord.adapter.escape_value(row[c]) }.join(", ") + ")"
      end

      assigns = on_duplicate
      if assigns.nil?
        updatable = cols.reject { |c| target_names.include?(c.to_s) }
        # Every column IS the conflict target: there is nothing left to
        # assign, and `DO UPDATE SET` with an empty list is a syntax
        # error. Rails degrades to a no-op insert here too.
        assigns = updatable.length == 0 ? nil : updatable.map { |c| "#{c} = excluded.#{c}" }.join(", ")
      end
      action = assigns.nil? ? "DO NOTHING" : "DO UPDATE SET #{assigns}"

      predicate = _conflict_predicate(target_names.sort.join(", "))
      conflict_where = predicate == "" ? "" : " WHERE #{predicate}"

      sql = "INSERT INTO #{table_name} (#{cols.join(", ")}) VALUES #{tuples.join(", ")}" \
            " ON CONFLICT (#{target_names.join(", ")})#{conflict_where} #{action}"
      ActiveRecord.adapter.execute_ddl(sql)
      ActiveRecord.adapter.changes
    end

    # `self.record_timestamps=` — Rails class-attribute toggling auto
    # timestamp stamping around a bulk write. `fill_timestamps` always
    # stamps (the toggle only matters on write paths); accept and ignore
    # the assignment so the class-side setter resolves.
    def self.record_timestamps=(value)
      value
    end

    def self.record_timestamps
      true
    end

    # inheritance.rb (8.2.0.alpha): `attr_accessor :abstract_class` on the
    # class, so the value is the declaring class's own ivar and a subclass
    # of an abstract class is not abstract. The emitted class body replays
    # `self.abstract_class = true` (emit::ruby::attribute_api).
    def self.abstract_class
      @abstract_class
    end
    
    def self.abstract_class=(value)
      @abstract_class = value
    end
    
    def self.abstract_class?
      @abstract_class == true
    end

    # inheritance.rb's base_class: the class right below Base or below an
    # abstract class (Base for Base itself), whose table and name an STI
    # subclass shares. Rails computes it when the class is created; here
    # it is computed on the call from the same rule.
    def self.base_class
      return self if equal?(ActiveRecord::Base)
      parent = superclass #: untyped
      parent.equal?(ActiveRecord::Base) || parent.abstract_class? ? self : parent.base_class
    end

    def self.base_class?
      base_class.equal?(self)
    end

    # inheritance.rb `class_attribute :store_full_class_name, default:
    # true`: the class's own value once assigned, else its superclass's.
    def self.store_full_class_name
      return @store_full_class_name if @__store_full_class_name_assigned
      return true if equal?(ActiveRecord::Base)
      parent = superclass #: untyped
      parent.store_full_class_name
    end

    def self.store_full_class_name=(value)
      @__store_full_class_name_assigned = true
      @store_full_class_name = value
    end

    def self.store_full_class_name?
      store_full_class_name ? true : false
    end

    # inheritance.rb's polymorphic_name: the value a polymorphic
    # association's `_type` column stores for this class.
    def self.polymorphic_name
      full = base_class.name.to_s
      return full if store_full_class_name
      i = full.rindex("::")
      i ? full[(i + 2)..] : full
    end

    # inheritance.rb's polymorphic_class_for: the class a stored `_type`
    # value names (`constantize`, which is `Object.const_get`). The
    # demodulized form (`store_full_class_name = false`, Rails'
    # compute_type) is not ported.
    def self.polymorphic_class_for(name)
      unless store_full_class_name
        raise NotImplementedError, "roundhouse: error[framework_feature_not_ported]: #{self.name}.polymorphic_class_for(#{name.inspect}): " \
          "with `store_full_class_name = false` Rails resolves the name relative to the class (compute_type), which is not ported"
      end
      Object.const_get(name)
    end

    # The model's column name => cast-type spec table. Emitted models
    # override it from the schema (emit::ruby::attribute_api). The base and
    # an abstract class have no table, so no columns; any other class
    # without the override is a model the tree has no schema for, whose
    # types are unknown rather than empty.
    def self.schema_column_types
      return {} if equal?(ActiveRecord::Base) || abstract_class?
      raise NotImplementedError, "#{name}.type_for_attribute: the tree carries no schema for #{name}"
    end
    
    # attribute_registration.rb (8.2.0.alpha): `attribute_types` is the
    # name => cast type Hash whose default is `Type.default_value`, and
    # `type_for_attribute` takes `attribute_name.to_s` (aliases are not
    # modeled here) and, given a block, fetches with it. The types come from
    # `schema_column_types`, built once per class and name. An entry the
    # type port does not carry raises NotImplementedError when asked for,
    # rather than answering some other type.
    # A type declared with `attribute` (below) answers before the schema's.
    def self.type_for_attribute(attribute_name, &block)
      name = attribute_name.to_s
      declared = _declared_attribute_types[name]
      return declared if declared
      spec = schema_column_types[name]
      if spec.nil?
        return block ? block.call(name) : ActiveModel::Type.default_value
      end
      cache = (@__attribute_types ||= {})
      cache.fetch(name) { cache[name] = _cast_type_for_spec(name, spec) }
    end
    
    def self.attribute_types
      types = Hash.new(ActiveModel::Type.default_value)
      schema_column_types.each_key { |name| types[name] = type_for_attribute(name) }
      types.merge(_declared_attribute_types)
    end
    
    # attribute_registration.rb's `attribute(name, type = nil, default:,
    # **options)`, for a call the class body makes at load: an app macro
    # replayed there (Core's model_uuid_attributes runs `attribute name,
    # :model_uuid_as_integer`). A Symbol type is looked up in
    # ActiveRecord::Type's registry with the class's adapter when the
    # types are first read, not at the call: the class body runs before a
    # connection is configured, and Rails too keeps the declaration
    # pending until the schema loads (attributes.rb's PendingType,
    # `resolve_type_name`). Later declarations win,
    # and a subclass sees its superclasses' (Rails applies the pending
    # types superclass first).
    #
    # The compiled readers, writers, hydration and binds follow a declared
    # type only for the names the compiler gave the dynamic path
    # (`_retypable_attribute_names`, emitted per model). For any other
    # name the call raises NotImplementedError instead of recording a type
    # the compiled accessors would ignore.
    #
    # A `default:` is the value a new record's attribute starts from when
    # it is not given one (attribute_registration.rb's `define_default_
    # attribute`, a UserProvidedDefault): a Proc is called for each
    # record, and the value is cast through the attribute's type. The
    # compiled `initialize` asks for it (`_attribute_default?`) for the
    # columns the compiler saw a `default:` call name; for any other name
    # a default raises like an unconsulted type. A loaded record never
    # consults it (hydration constructs with HYDRATE_ATTRS). One
    # difference: `initialize` calls a Proc default as it builds the
    # record, Rails on the attribute's first read, so a new record that
    # never reads the attribute (nor saves) still costs one call.
    def self.attribute(name, type = nil, default: (no_default = true), **options)
      name = name.to_s
      unless _retypable_attribute_names.include?(name)
        raise NotImplementedError, "#{self.name}.attribute(#{name.inspect}, ...): the compiled accessors for `#{name}` do not consult a declared type"
      end
      unless no_default
        unless _defaulted_attribute_names.include?(name)
          raise NotImplementedError, "#{self.name}.attribute(#{name.inspect}, default: ...): the compiled initialize does not consult a declared default for `#{name}`"
        end
        @__declared_attribute_defaults = (@__declared_attribute_defaults || {}).merge(name => default)
      end
      @__declared_attribute_types = (@__declared_attribute_types || {}).merge(name => [type, options]) if type
      _reset_attribute_types
      nil
    end

    # The class's declared defaults, a superclass's under its own.
    def self._declared_attribute_defaults
      parent = superclass
      inherited = parent <= ActiveRecord::Base ? parent._declared_attribute_defaults : {}
      own = @__declared_attribute_defaults || {} #: Hash[String, untyped]
      inherited.merge(own)
    end

    def self._attribute_default?(name)
      _declared_attribute_defaults.key?(name)
    end

    def self._attribute_default(name)
      value = _declared_attribute_defaults[name]
      value.is_a?(Proc) ? value.call : value
    end
    
    # attribute_methods.rb's `alias_attribute(new_name, old_name)` over a
    # column. The compiler defines the new name's attribute methods and
    # this class's `attribute_aliases` from the call it saw; here the call
    # is checked against them, and an alias the compiled class does not
    # define raises instead of answering nothing.
    def self.alias_attribute(new_name, old_name)
      new_key = new_name.to_s
      old_key = old_name.to_s
      unless attribute_aliases[new_key] == old_key
        raise NotImplementedError, "#{self.name}.alias_attribute(:#{new_key}, :#{old_key}): the compiled class does not define the alias"
      end
      nil
    end
    
    # Rails' `attribute_aliases`, the new name to the column's: a model
    # with an `id` column aliases `id_value` to it. A model with aliases
    # answers its own (compiled).
    def self.attribute_aliases
      return {} if self == ActiveRecord::Base || abstract_class?
      schema_columns.include?(:id) ? { "id_value" => "id" } : {}
    end
    
    def self.attribute_alias?(name)
      attribute_aliases.key?(name.to_s)
    end
    
    def self.attribute_alias(name)
      attribute_aliases[name.to_s]
    end
    
    # The column an attribute name reads (Rails' `attribute_alias(name) ||
    # name`): what `where`, `order` and `pluck` keys resolve through.
    def self._attribute_name(name)
      attribute_aliases[name] || name
    end
    
    # `attrs` with each alias key under its column's (the compiled `new`
    # and `update` assign by column); `attrs` itself when none is one.
    def self._resolve_attribute_aliases(attrs)
      given = attrs #: Hash[Symbol, untyped]
      aliases = attribute_aliases #: Hash[String, String]
      return given unless given.any? { |key, _| aliases.key?(key.to_s) }
      resolved = {} #: Hash[Symbol, untyped]
      given.each { |key, value| resolved[(aliases[key.to_s] || key.to_s).to_sym] = value }
      resolved
    end
    
    # attribute_methods/serialization.rb's `serialize(attr_name, coder:,
    # type:, comparable:, yaml:, **options)`, for a call the class body
    # makes at load with an app coder. The column serializer is built at
    # the call, as Rails builds it (Type::Serialized::Decoration, in the
    # type port), and the column's type, the one declared with
    # `attribute` or else the schema's, is decorated with Type::Serialized
    # when the types are first read, as Rails' `decorate_attributes`
    # does. As for `attribute`, a column whose
    # compiled accessors do not consult a declared type raises
    # NotImplementedError, and so do `attribute` options (`default:`).
    def self.serialize(attr_name, coder: nil, type: Object, comparable: false, yaml: {}, **options)
      name = attr_name.to_s
      decoration = ActiveRecord::Type::Serialized::Decoration.new(self.name, attr_name, coder, type, comparable)
      unless options.empty?
        raise NotImplementedError, "#{self.name}.serialize(#{name.inspect}, #{options.keys.inspect}): `attribute` options on a serialized column are not ported"
      end
      unless _retypable_attribute_names.include?(name)
        raise NotImplementedError, "#{self.name}.serialize(#{name.inspect}, ...): the compiled accessors for `#{name}` do not consult a declared type"
      end
      @__serialized_attributes = (@__serialized_attributes || {}).merge(name => decoration)
      _reset_attribute_types
      nil
    end

    # The attribute names whose compiled accessors consult
    # `type_for_attribute`. Emitted models override it.
    def self._retypable_attribute_names
      []
    end

    # The attribute names whose compiled `initialize` consults a declared
    # default. Emitted models override it.
    def self._defaulted_attribute_names
      []
    end

    # persistence.rb's `query_constraints(*columns_list)`: the columns
    # update, destroy and reload key a record on, in order. The compiled
    # `_adapter_update`/`_adapter_delete`/`_adapter_reload` carry that WHERE
    # for the list the class body names (`_compiled_query_constraints`,
    # emitted per model) and use it when this list is set; any other list
    # raises rather than being recorded and ignored.
    def self.query_constraints(*columns_list)
      list = []
      columns_list.each { |column| list << column.to_s }
      raise ArgumentError, "You must specify at least one column to be used in querying" if list.empty?
      unless list == _compiled_query_constraints
        raise NotImplementedError, "#{name}.query_constraints(#{list.join(", ")}): the compiled update, destroy and reload key on #{(_compiled_query_constraints || [primary_key]).join(", ")}"
      end
      @query_constraints_list = list
      @has_query_constraints = list
    end

    def self.has_query_constraints?
      @has_query_constraints
    end

    # A model's own list, else its base class's when the two share a
    # primary key (an STI subclass), as in Rails. A composite
    # `primary_key` is not ported, so there is no implicit list.
    def self.query_constraints_list
      return @query_constraints_list if @query_constraints_list
      return _compiled_query_constraints if composite_primary_key?
      parent = superclass
      return nil unless parent < ActiveRecord::Base
      return nil if parent.abstract_class?
      parent.primary_key == primary_key ? parent.query_constraints_list : nil
    end

    def self.composite_query_constraints_list
      query_constraints_list || [primary_key]
    end

    # The list the compiled adapter methods key on; nil when they key on
    # the primary key alone. Emitted models override it.
    def self._compiled_query_constraints
      nil
    end

    # model_schema.rb's `ignored_columns`: the model's own list, else its
    # superclass's. The compiled table already leaves out the columns the
    # class body ignores (`_compiled_ignored_columns`, emitted per model),
    # so a write raises, naming the columns, rather than being recorded
    # while the compiled SQL disagrees: one naming any other column (the
    # compiled SQL would still read and write it), and one un-ignoring a
    # column the list in effect ignores and the compiled table leaves out
    # (the compiled SQL would still skip it). The class body's own writes
    # only build up to the compiled list, so they pass.
    def self.ignored_columns
      return @ignored_columns if @ignored_columns
      parent = superclass
      parent <= ActiveRecord::Base ? parent.ignored_columns : IGNORED_COLUMNS_NONE
    end

    def self.ignored_columns=(columns)
      list = []
      columns.each { |column| list << column.to_s }
      list.freeze
      unknown = list - _compiled_ignored_columns
      unless unknown.empty?
        raise NotImplementedError, "#{name}.ignored_columns = #{list.inspect}: the compiled table leaves out only #{_compiled_ignored_columns.inspect}"
      end
      restored = (ignored_columns & _compiled_ignored_columns) - list
      unless restored.empty?
        raise NotImplementedError, "#{name}.ignored_columns = #{list.inspect}: un-ignores #{restored.inspect}, which the compiled table leaves out"
      end
      @ignored_columns = list
    end

    # The ignored columns the compiled table leaves out. Emitted models
    # override it.
    def self._compiled_ignored_columns
      IGNORED_COLUMNS_NONE
    end

    IGNORED_COLUMNS_NONE = [].freeze
    private_constant :IGNORED_COLUMNS_NONE
    
    # Read on every retypable accessor and bind, so memoized; a later
    # `attribute` or `serialize` here or in a superclass clears it. A
    # serialized column's decoration (Type::Serialized::Decoration) wraps
    # the type it would otherwise have.
    def self._declared_attribute_types
      @__declared_types_memo ||= begin
        parent = superclass
        inherited = parent <= ActiveRecord::Base ? parent._declared_attribute_types : {}
        own = {}
        (@__declared_attribute_types || {}).each do |n, (type, options)|
          own[n] = type.is_a?(Symbol) ? ActiveRecord::Type.lookup(type, **options, adapter: ActiveRecord::Type.adapter_name_from(self)) : type
        end
        types = inherited.merge(own)
        serialized = @__serialized_attributes || {} #: Hash[String, untyped]
        serialized.each { |n, decoration| types[n] = decoration.call(types[n] || _cast_type_for_spec(n, schema_column_types[n])) }
        types
      end
    end
    
    def self._reset_attribute_types
      @__attribute_types = nil
      @__declared_types_memo = nil
      subclasses.each(&:_reset_attribute_types)
      nil
    end
    
    # The compiled path of a retypable column (`_retypable_attribute_names`):
    # its writer casts, hydration deserializes and every bind serializes
    # through the declared type, as attribute.rb's FromUser / FromDatabase
    # and value_for_database do. With no type declared (the macro chose
    # not to retype the column) the value passes through as it did before.
    # A write checks the value first (attribute.rb's `with_value_from_user`
    # calls `assert_valid_value`): a serialized column's wrong class raises.
    def self._attribute_cast(name, value)
      type = _declared_attribute_types[name]
      return value unless type
      type.assert_valid_value(value)
      type.cast(value)
    end
    
    def self._attribute_deserialize(name, value)
      type = _declared_attribute_types[name]
      type ? type.deserialize(value) : value
    end
    
    def self._attribute_serialize(name, value)
      type = _declared_attribute_types[name]
      type ? _bind_type_cast(name, type.serialize(value)) : value
    end
    
    # A value from outside (a where/update_all/exists? argument): cast,
    # then serialized, as QueryAttribute#value_for_database does.
    def self._attribute_for_database(name, value)
      type = _declared_attribute_types[name]
      type ? _bind_type_cast(name, type.serialize(type.cast(value))) : value
    end
    
    # predicate_builder.rb's `force_equality?` test for a hash condition
    # on a column with a declared type: true where the type compares the
    # value whole (Type::Serialized, for its `type:` class).
    def self._attribute_force_equality?(name, value)
      type = _declared_attribute_types[name]
      type ? type.force_equality?(value) : false
    end

    # How the adapter sends a declared type's serialized value as a bind
    # (quoting.rb's `type_cast`): as text the column's own type then reads,
    # so a boolean or number declared over a varchar column stores and
    # compares as text, as in Rails, where the inlined literal would not
    # compare at all. A number is its digits; a boolean is PostgreSQL's
    # text encoding 't'/'f' of its TRUE/FALSE literal, or SQLite's 1/0
    # literal itself. Only string and text columns take the text; any
    # other keeps the value its compiled escape expects.
    def self._bind_type_cast(name, value)
      text = case value
             when true then _bind_bool_text(true)
             when false then _bind_bool_text(false)
             when Integer, Float then value.to_s
             end
      text && %i[string text].include?(schema_column_types.dig(name, 0)) ? text : value
    end

    def self._bind_bool_text(flag)
      literal = Db.escape_bool_opt(flag).to_s
      return "t" if literal == "TRUE"
      return "f" if literal == "FALSE"
      literal
    end
    
    # A key read back from a row (`ids`).
    def self._primary_key_from_database(value)
      type = _declared_attribute_types[primary_key]
      type ? type.deserialize(value) : _cast_primary_key(value)
    end
    
    # A compiled column writer's cast (emit::ruby::attribute_cast):
    # attribute.rb's FromUser, `type.cast(value)` with the column's type.
    def self._cast_column(name, value)
      type_for_attribute(name).cast(value)
    end

    # The spec's type: PostgreSQL's type map answers its OID classes for
    # a decimal, date, timestamp and uuid column.
    def self._cast_type_for_spec(name, spec)
      kind = spec[0]
      case kind
      when :integer then ActiveRecord::Type::Integer.new(limit: spec[1])
      when :float then ActiveRecord::Type::Float.new
      when :decimal then ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Decimal.new(precision: spec[1], scale: spec[2])
      when :date then ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Date.new
      when :timestamp then ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Timestamp.new(precision: spec[1])
      when :uuid then ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Uuid.new
      when :big_integer then ActiveRecord::Type::BigInteger.new
      when :string then ActiveRecord::Type::String.new(limit: spec[1])
      when :text then ActiveRecord::Type::Text.new
      when :boolean then ActiveRecord::Type::Boolean.new
      when :binary then ActiveRecord::Type::Binary.new
      when :bytea then ActiveRecord::ConnectionAdapters::PostgreSQL::OID::Bytea.new
      when :immutable_string then ActiveRecord::Type::ImmutableString.new
      when :value then ActiveRecord::Type::Value.new
      else
        raise NotImplementedError, "#{self.name}.type_for_attribute(#{name.inspect}): #{spec[1]} is not ported"
      end
    end
    
    # `record.update_column(name, value)` — write one attribute straight
    # to the row, skipping validations and callbacks. Sets the in-memory
    # value via the `[]=` indexer, then persists via the same adapter
    # path `save` uses.
    def update_column(name, value)
      self[name] = value
      _adapter_update
      true
    end

    # Rails' `Base#as_json(only:)` attribute serializer, monomorphized:
    # the corpus reaches it only as `super(only: attrs)` inside a
    # model's own `as_json`, which the as_json_super lowering rewrites
    # to this call. String-keyed like Rails.
    #
    # `only:` NARROWS the attribute set — it does not define it. Rails
    # intersects it with the record's real attributes, so a name in the
    # list that isn't a column contributes nothing. lobsters' User
    # pushes `:homepage` (a typed_store attribute living inside the
    # `settings` column) beside `:about` (a real column); Rails emits
    # only `about`, and echoing the list verbatim added a
    # `"homepage": null` to every user in /hottest's JSON.
    #
    # Values come from the `[]` indexer, which hands back the STORED
    # text — right for every column except a temporal one, which Rails
    # renders as ISO8601 with three fractional digits in the app's zone.
    # `schema_time_columns` is the emitted fact that says which those
    # are.
    def _as_json_only(only)
      h = {}
      columns = self.class.schema_columns
      time_columns = self.class.schema_time_columns
      only.each do |k|
        next unless columns.include?(k)
        h[k.to_s] = if time_columns.include?(k)
          ActiveSupport.json_time(self[k])
        else
          self[k]
        end
      end
      h
    end

    # Rails-shape `where` fallback: a lazy Relation, so dynamic
    # call-sites chain off it (`klass.where(short_id: id).exists?` in
    # lobsters' ShortId, where `klass` is a class-valued attribute no
    # static lowering can resolve). Overrides base.rb's Array-returning
    # version, which stays for the strict-target runtime transpiles
    # (no Relation class in their tables); this file is walked only
    # into the ruby-family trees. Lowered call-sites don't land here —
    # they drive a Relation or `_adapter_*` directly.
    def self.where(conditions)
      ActiveRecord::Relation.new(self).where(conditions.to_h)
    end

    # Model Hash finders use the relation's NULL / IN predicates too.
    # `.to_h` matches `where` above: Hash is a no-op (nil / Array values
    # survive); non-Hash inputs raise rather than reach Relation's SQL path.
    def self.find_by(conditions)
      ActiveRecord::Relation.new(self).find_by(conditions.to_h)
    end

    # `Model.find_sole_by(attrs)` — Rails' `where(attrs).sole`; see
    # Relation#find_sole_by. `Model.sole` is the same on the whole table.
    def self.find_sole_by(conditions)
      ActiveRecord::Relation.new(self).find_sole_by(conditions.to_h)
    end

    def self.sole
      ActiveRecord::Relation.new(self).sole
    end

    # Rails-shape `all` fallback, same story as `where` above: a lazy
    # Relation so refiner chains the lowerers left dynamic
    # (`Category.all.order("category asc, tags.tag asc")…` on lobsters'
    # filters page) chain off it instead of crashing on base.rb's
    # eager-Array version. Lowered call-sites don't land here — the
    # arel pass claims a plain `Model.all` and the scope-chain
    # normalizer re-roots recognized chains onto
    # `ActiveRecord::Relation.new(Model)` directly.
    def self.all
      ActiveRecord::Relation.new(self)
    end

    # ---- default_scope ----------------------------------------------
    #
    # Rails merges a class's `default_scope` into every relation of it
    # (`all`, `where`, `find`, `count`, its associations) and assigns the
    # scope's `where` equalities to a new record before the given
    # attributes; `unscoped` drops both. A model that declares one gets
    # `_default_scoped?`, `_apply_default_scope` (the scope's chain run on
    # the relation, in place) and `_default_scope_attributes` emitted by
    # `lower::default_scope`; these are the answers for every other model.
    # `Relation#initialize` applies the scope, so the compiled roots
    # (`ActiveRecord::Relation.new(Model)`) take it too.
    def self._default_scoped?
      false
    end

    def self._apply_default_scope(relation)
      relation
    end

    # Rails' `type_condition`: a relation on an STI subclass reads only
    # its rows and its subclasses', `unscoped` included, so a class
    # method reached through the subclass (`Dog.recent`) does too.
    # `Relation#initialize` applies it; `emit::ruby` writes each STI
    # subclass's (`sti_scope::push_type_condition_methods`), and every
    # other class adds none.
    def self._apply_type_condition(relation)
      relation
    end

    def self._default_scope_attributes
      {}
    end

    # Declared, and no `unscoped { ... }` block of this class running.
    def self._default_scope_applies?
      _default_scoped? && (@_unscoped_depth.nil? || @_unscoped_depth == 0)
    end

    # A join onto this model (`Shelf.joins(:docs)`) carries its default
    # scope's conditions in the ON clause, as Rails' join scope does:
    # `lower::scope_chain` appends this to the join it renders. Empty
    # when no default scope applies or it has no conditions.
    def self._join_scope_sql
      return "" unless _default_scope_applies?
      clauses = ActiveRecord::Relation.new(self).where_clauses
      clauses.empty? ? "" : " AND " + clauses.join(" AND ")
    end

    # `Model.unscoped` is a relation without the default scope;
    # `Model.unscoped { ... }` runs the block with the class's own finders,
    # relations and `new` unscoped.
    def self.unscoped
      return ActiveRecord::Relation.new(self, false) unless block_given?
      @_unscoped_depth = (@_unscoped_depth || 0) + 1
      begin
        yield
      ensure
        @_unscoped_depth -= 1
      end
    end

    # Called last by a default-scoped model's `initialize`: Rails' scope
    # attributes go in through the writers, and a given attribute wins.
    def _seed_default_scope_attributes(attrs)
      return unless self.class._default_scope_applies?
      self.class._default_scope_attributes.each { |name, value| self[name] = value unless attrs.key?(name) }
    end

    # base.rb's `count` / `last` / `destroy_all` read the table through
    # the adapter; a default scope reads through the relation.
    def self.count
      _default_scope_applies? ? ActiveRecord::Relation.new(self).count : _adapter_count
    end

    def self.last
      _default_scope_applies? ? ActiveRecord::Relation.new(self).last : _adapter_last
    end

    def self.destroy_all
      return ActiveRecord::Relation.new(self).destroy_all if _default_scope_applies?
      records = _adapter_all
      records.each { |r| r.destroy() }
      records
    end

    # SELECT 1 LIMIT 1. Strict targets keep COUNT in base.rb.
    def self.any?
      ActiveRecord::Relation.new(self).exists?
    end

    def self.none?
      !any?
    end

    # Rails-shape `none` fallback, same story as `where`/`all` above:
    # an empty Relation off the class. lobsters' `Search` reaches it
    # through a class-valued method (`searched_model.none`), which no
    # static lowering can resolve to one model (#132).
    def self.none
      ActiveRecord::Relation.new(self).none
    end

    # Class-side `Model.page(n)` / `Model.paginate(...)`: the same
    # page of a fresh Relation (`Relation#page` / `#paginate`).
    # Ruby-family-only for the reason `where` above is.
    def self.page(num = nil)
      ActiveRecord::Relation.new(self).page(num)
    end

    def self.paginate(num = nil, page: nil, per_page: nil)
      ActiveRecord::Relation.new(self).paginate(num, page: page, per_page: per_page)
    end

    # Rails-shape `first` fallback, same story as `where`/`all` above:
    # spec/dynamic call sites reach the class method directly
    # (`Category.first` in lobsters' specs); lowered call sites don't
    # land here. `last` lives in base.rb over `_adapter_last` — this
    # one is ruby-family-only because Relation#first already carries
    # the ORDER BY <key> ASC LIMIT 1 shape (`order_by_key`).
    def self.first
      ActiveRecord::Relation.new(self).first
    end

    # `User.take` — the Rails 8 authentication generator's tests set up
    # with it (`setup { @user = User.take }`), so the Rails Guides store
    # reaches here. No order imposed (Relation#take).
    def self.take
      ActiveRecord::Relation.new(self).take
    end

    # The Relation load path's Hash fallback for a hand-written model
    # (one with its own `instantiate` and no lowerer-emitted
    # `_hydrate_all`): the adapter's rows through `instantiate`, which
    # is what `Relation#to_a` did for every model before the typed
    # path. Ruby-family-only because `select_rows` is — the strict
    # targets' adapter contract stops at `all`/`find`/`count`, and
    # their models always carry the emitted override.
    def self._hydrate_all(sql)
      ActiveRecord.adapter.select_rows(sql).map { |row| instantiate(row) }
    end

    # Rails' `update_attribute`: one writer, then save WITHOUT
    # validations (validation callbacks skipped too) — save callbacks
    # still run. Specs use it to construct records a validation would
    # reject (lobsters' username-change history), so validating here
    # would break exactly the sites that reach for it. Enters save's
    # extracted post-validation half directly.
    def update_attribute(name, value)
      self[name] = value
      save_after_validation
    end

    # Rails' validation context: `valid?(:publish)` / `save(context:)`
    # run the validations with that context, and without one a record
    # validates in `:create` when new, `:update` otherwise. A rule's
    # `on:` reads it through `validation_context`, which is nil outside
    # a run. Ruby-family-only, beside `save`'s other Rails options.
    #
    # The run is the class's validate chain (RuntimeValidations below):
    # the compiled `validate` and whatever `validate`/`validates`
    # registered at run time, in Rails' order, under `throw :abort`.
    # Without a run-time registration it is the compiled `validate`.
    def valid?(context = nil)
      previous = @validation_context
      @validation_context = context || (new_record? ? :create : :update)
      @errors = []
      begin
        ActiveRecord::RuntimeValidations.run(self)
      ensure
        @validation_context = previous
      end
      @errors.empty?
    end

    def validation_context
      @validation_context
    end

    def save(context: nil)
      before_validation
      ok = valid?(context)
      after_validation
      return false unless ok

      save_after_validation
    end

    def save!(context: nil)
      raise RecordInvalid, self unless save(context: context)
      self
    end

    # Saved-change tracking (ActiveModel::Dirty subset) — the real
    # implementation behind base.rb's compile-surface stubs; see the
    # note there for why the diff is ruby-family-only. The snapshot
    # from the previous save (nil for a fresh instance, so a create
    # reports every attribute as [nil, value] — Rails' shape) diffs
    # against the post-write attributes; `save` calls this between the
    # row write and the after_* hooks, so callbacks observe the
    # finished save, matching Rails. A record hydrated from the DB has
    # no baseline yet, so its FIRST update over-reports;
    # baseline-at-hydration is future work.
    def __track_saved_changes(was_new)
      previous = @__last_saved_attributes
      current = _attribute_snapshot
      changes = {}
      current.each do |key, value|
        prev = previous.nil? ? nil : previous[key]
        changes[key] = [prev, value] if prev != value
      end
      @__last_saved_attributes = current
      @saved_changes = changes
      @id_previously_changed = was_new
      nil
    end

    def saved_changes
      @saved_changes || {}
    end

    # The Dirty baseline for a record that came from the DB. Without
    # it `__track_saved_changes` diffs the first update against a nil
    # snapshot and reports every column as `[nil, value]` — so
    # `<col>_previously_was` answered nil for all of them, which is how
    # campfire's `involvement_previously_was.inquiry.invisible?` found
    # this. The note above ("baseline-at-hydration is future work") is
    # what this closes.
    # The real value half: slot 0 of the `[prev, value]` pair. Bound to
    # a local before the nil test and the index, the same precaution
    # `saved_change_to_attribute?` documents. Ruby-family only — see
    # base.rb's stub for why the indexing cannot live there.
    def attribute_previously_was(name)
      pair = saved_changes[name]
      pair.nil? ? nil : pair[0]
    end

    # What Dirty compares: a compiled model's String-keyed column values
    # without the id, the temporal ones as their stored text
    # (emit::ruby::attribute_cast keeps the lowering's hash under this
    # name beside the public `attributes`). Any other record's is its
    # `attributes`.
    def _attribute_snapshot
      attributes
    end

    def _note_hydrated
      @__last_saved_attributes = _attribute_snapshot
      nil
    end

    # The row's unselected schema columns — see base.rb's empty stub.
    # Rails answers `has_attribute?(col)` false for a column a partial
    # `select` left out, and lobsters' Token guard
    # (`if new_record? || has_attribute?(:token)`) is exactly that test:
    # a `User.select(*attrs)` without `token` must not mint one. Nothing
    # is recorded when the row carries every column, so the common path
    # allocates nothing.
    def _note_unloaded(row)
      missing = []
      self.class.schema_columns.each { |c| missing << c unless row.key?(c.to_s) }
      @__unloaded_columns = missing unless missing.empty?
      nil
    end

    # attribute_methods.rb's has_attribute? (a String or Symbol name, an
    # alias read as its column) over _has_attribute? (the attribute
    # set's String key): a schema column the row carries.
    def has_attribute?(attr_name)
      attr_name = attr_name.to_s
      attr_name = self.class.attribute_aliases[attr_name] || attr_name
      _has_attribute?(attr_name)
    end

    def _has_attribute?(attr_name)
      return false unless attr_name.is_a?(String)
      column = attr_name.to_sym
      return false unless self.class.schema_columns.include?(column)
      unloaded = @__unloaded_columns
      unloaded.nil? || !unloaded.include?(column)
    end

    # The class-level forms: does the model's attribute set (its schema
    # columns) have the attribute.
    def self.has_attribute?(attr_name)
      attr_name = attr_name.to_s
      attr_name = attribute_aliases[attr_name] || attr_name
      _has_attribute?(attr_name)
    end

    def self._has_attribute?(attr_name)
      if equal?(ActiveRecord::Base) || abstract_class?
        raise ActiveRecord::TableNotSpecified, "#{self} has no table configured. Set one with #{self}.table_name="
      end
      attr_name.is_a?(String) && schema_columns.include?(attr_name.to_sym)
    end

    # The pending diff: current attributes against the baseline the
    # last save (or hydration) left, in the same `[prev, value]` shape
    # as `saved_changes`. `save` runs validations and before_* hooks
    # BEFORE `__track_saved_changes` moves the baseline, so they see
    # what is about to be written, and after_* hooks see nothing
    # pending — Rails' order (`changes_applied` precedes the after
    # callbacks).
    #
    # A new record has no baseline, so every non-nil attribute reads as
    # pending. Rails compares against the column default instead, so a
    # column still holding its default (a `karma` of 0) over-reports
    # here; nil-default columns, which is what validations guard on,
    # agree.
    def changes_to_save
      previous = @__last_saved_attributes
      changes = {}
      _attribute_snapshot.each do |key, value|
        prev = previous.nil? ? nil : previous[key]
        changes[key] = [prev, value] if prev != value
      end
      changes
    end

    # Rails' `<col>_was`: the value before the pending change, or the
    # current value when nothing is pending. Both halves are the
    # `_attribute_snapshot` form, so a datetime column answers its raw
    # stored text, as `attribute_previously_was` already does.
    def attribute_was(name)
      pair = changes_to_save[name]
      pair.nil? ? _attribute_snapshot[name] : pair[0]
    end


    # ---- Validations declared at run time ----------------------------
    #
    # A model's class-body `validates`/`validate` compile into its
    # `validate` (lower::model_to_library::validations). These are the
    # class methods for the ones code declares while it runs: a
    # concern's `included` hook (`base.validate(:check, if: :pred?)`,
    # Core's SensitiveAttributeValidations), or a class method a macro
    # calls (`validates(foreign_key, presence: true)`). They follow
    # ActiveModel::Validations::ClassMethods (activemodel 8.2.0.alpha,
    # validations.rb, validates.rb, with.rb, helper_methods.rb) and
    # ActiveRecord::Validations' helpers. RuntimeValidations below keeps
    # the registrations and runs them in `valid?`; the validators are
    # the ActiveModel and ActiveRecord ports (runtime/spinel/
    # active_model_validations.rb, active_record_validations.rb).
    def self.validate(*args, &block)
      ActiveRecord::RuntimeValidations.validate(self, args, block)
    end

    def self.validates(*attributes)
      ActiveRecord::RuntimeValidations.validates(self, attributes, false)
    end

    def self.validates!(*attributes)
      ActiveRecord::RuntimeValidations.validates(self, attributes, true)
    end

    def self.validates_with(*args, &block)
      ActiveRecord::RuntimeValidations.validates_with(self, args, block)
    end

    def self.validates_each(*attr_names, &block)
      ActiveRecord::RuntimeValidations.validates_each(self, attr_names, block)
    end

    # The `validates_<kind>_of` helpers: Active Record's validator where
    # it has one (presence, absence, length, numericality, associated,
    # uniqueness), Active Model's otherwise.
    def self.validates_absence_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "AbsenceValidator", attr_names)
    end

    def self.validates_acceptance_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "AcceptanceValidator", attr_names)
    end

    def self.validates_associated(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "AssociatedValidator", attr_names)
    end

    def self.validates_comparison_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "ComparisonValidator", attr_names)
    end

    def self.validates_confirmation_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "ConfirmationValidator", attr_names)
    end

    def self.validates_exclusion_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "ExclusionValidator", attr_names)
    end

    def self.validates_format_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "FormatValidator", attr_names)
    end

    def self.validates_inclusion_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "InclusionValidator", attr_names)
    end

    def self.validates_length_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "LengthValidator", attr_names)
    end

    def self.validates_size_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "LengthValidator", attr_names)
    end

    def self.validates_numericality_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "NumericalityValidator", attr_names)
    end

    def self.validates_presence_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "PresenceValidator", attr_names)
    end

    def self.validates_uniqueness_of(*attr_names)
      ActiveRecord::RuntimeValidations.helper(self, "UniquenessValidator", attr_names)
    end

    # The class_attribute `_validators` (attribute => validators) with
    # its readers. It lists the validators registered at run time; a
    # compiled rule has no validator object to list.
    def self._validators
      ActiveRecord::RuntimeValidations.validators_of(self)
    end

    def self.validators
      ActiveRecord::RuntimeValidations.all_validators(self)
    end

    def self.validators_on(*attributes)
      ActiveRecord::RuntimeValidations.validators_on(self, attributes)
    end

    # What ActiveModel::Error reads off a record to word its message:
    # ActiveRecord::Translation (`i18n_scope`, `lookup_ancestors`),
    # ActiveModel::Translation#human_attribute_name and
    # ActiveModel::Naming#model_name (through the ports), and
    # Validations#read_attribute_for_validation.
    def self.i18n_scope
      :activerecord
    end

    def self.lookup_ancestors
      ActiveRecord::RuntimeValidations.lookup_ancestors(self)
    end

    def self.human_attribute_name(attribute, options = {})
      ActiveRecord::RuntimeValidations.human_attribute_name(self, attribute, options)
    end

    def self.model_name
      ActiveRecord::RuntimeValidations.model_name(self)
    end

    def model_name
      ActiveRecord::RuntimeValidations.model_name(self.class)
    end

    def read_attribute_for_validation(attribute)
      ActiveRecord::RuntimeValidations.read_attribute(self, attribute)
    end

    # Reflection#_reflect_on_association, which Active Record's presence
    # and absence validators ask: the reflection a compiled model answers
    # for an association its class macros declare
    # (lower::model_to_library::macro_associations), else nil.
    def self._reflect_on_association(association)
      ActiveRecord::RuntimeValidations.reflect_on_association(self, association)
    end

    # AutosaveAssociation#marked_for_destruction?. Nothing here marks a
    # record (`mark_for_destruction` is not ported), so it is false
    # unless something set the flag.
    def marked_for_destruction?
      @marked_for_destruction == true
    end

    # The accumulator (an Array of full messages, base.rb) answering
    # ActiveModel::Errors#add, so `errors.add(:password, :blank)` and the
    # validators' adds record an ActiveModel::Error's full message
    # (RuntimeValidations::Errors).
    def errors
      errors = @errors
      if errors.nil?
        errors = []
        @errors = errors
      end
      ActiveRecord::RuntimeValidations.accumulator(errors, self)
    end
  end

  # The validate chain of ActiveModel::Validations (`define_callbacks
  # :validate, scope: :name`, ActiveSupport::Callbacks) for a compiled
  # model, the way runtime/spinel/cruby/active_record_callbacks.rb runs
  # the other chains. Each class keeps what it registered at run time;
  # a class's chain is its superclass chain from Base down, at each
  # class its registrations in order and then the compiled `validate`
  # where that class defines it (a subclass's compiled `validate` calls
  # `super()` first, so it runs the compiled rules of its ancestors
  # too). A registration names a method (`validate :check`), a block, or
  # a validator (`validates_with`), with `if:`/`unless:` conditions
  # (`on:` and `except_on:` become conditions on the validation
  # context), called as ActiveSupport::Callbacks::CallTemplate calls
  # them. `throw :abort` ends the run; the record is valid when no error
  # was added.
  #
  # Differences, at the edges:
  # - A class's compiled rules run after what the class registered at
  #   run time, wherever its body declared them; Rails keeps declaration
  #   order.
  # - A registration on a class after a subclass exists still runs
  #   before the subclass's own; Rails appends it to the end of the
  #   subclass's chain.
  # - `_validators` lists only the run-time validators, and a class
  #   copies its superclass's when first asked rather than when defined.
  # - A `validate` of kind `:after` or `:around` (`validate :after,
  #   :check`) raises NotImplementedError.
  # - On a tree without the ActiveModel ports (only the PostgreSQL CRuby
  #   tree carries them) `validates`, `validates_with`, `errors.add`,
  #   `human_attribute_name` and `model_name` raise NotImplementedError
  #   naming the port; `validate` with a method or block works anywhere.
  #
  # Each parameter is bound once to an ascribed local, so the runtime
  # inference probes follow the bodies (tests/inference_on_spinel_blog_
  # runtime*.rs).
  module RuntimeValidations
    EMPTY = [].freeze

    @generation = 0
    @context_predicates = {}

    # ActiveModel::Validations::ClassMethods#validate, and set_callback's
    # normalize_callback_params.
    def self.validate(klass, args, block)
      klass = klass #: untyped
      filters = args.dup #: Array[untyped]
      block = block #: untyped
      options = extract_options!(filters)
      symbols = true
      filters.each { |filter| symbols = false unless filter.is_a?(Symbol) }
      if symbols
        options.each_key do |key|
          unless [:on, :if, :unless, :prepend, :except_on].include?(key)
            raise ArgumentError, "Unknown key: #{key.inspect}. Valid keys are: :on, :if, :unless, :prepend, :except_on. " \
              "Perhaps you meant to call `validates` instead of `validate`?"
          end
        end
      end
      if options.key?(:on)
        conditions = [context_predicate(options[:on])] #: Array[untyped]
        given = Array(options[:if]) #: Array[untyped]
        given.each { |condition| conditions << condition }
        options = options.merge(if: conditions)
      end
      if options.key?(:except_on)
        except_on = Array(options[:except_on]) #: Array[untyped]
        conditions = [ExceptOnContext.new(except_on)] #: Array[untyped]
        given = Array(options[:unless]) #: Array[untyped]
        given.each { |condition| conditions << condition }
        options = options.merge(except_on: except_on, unless: conditions)
      end
      if [:before, :after, :around].include?(filters.first)
        kind = filters.shift
        unless kind == :before
          raise NotImplementedError, "#{klass.name}.validate(#{kind.inspect}, ...): only :before validate callbacks are ported"
        end
      end
      filters.unshift(block) if block
      if_conditions = conditionals(options[:if])
      unless_conditions = conditionals(options[:unless])
      prepend = options[:prepend] ? true : false
      entries = [] #: Array[Entry]
      filters.each do |filter|
        if filter.is_a?(String)
          raise ArgumentError, "Passing string to define a callback is not supported. See the `define_callbacks` documentation for more information"
        end
        entries << Entry.new(filter, if_conditions, unless_conditions, prepend)
      end
      # CallbackChain#prepend keeps the filters' order at the front.
      entries.reverse! if prepend
      own = registrations(klass)
      entries.each { |entry| own << entry }
      @generation += 1
      nil
    end

    # ActiveModel::Validations::ClassMethods#validates and #validates!.
    def self.validates(klass, attributes, strict)
      klass = klass #: untyped
      names = attributes.dup #: Array[untyped]
      strict = strict #: bool
      port!("ActiveModel::Validations", "validates")
      given = extract_options!(names)
      validations = {} #: Hash[Symbol, untyped]
      defaults = {} #: Hash[Symbol, untyped]
      given.each do |key, value|
        if [:if, :unless, :on, :allow_blank, :allow_nil, :strict, :except_on].include?(key)
          defaults[key] = value
        else
          validations[key] = value
        end
      end
      defaults[:strict] = true if strict
      raise ArgumentError, "You need to supply at least one attribute" if names.empty?
      raise ArgumentError, "You need to supply at least one validation" if validations.empty?
      defaults[:attributes] = names
      inflector = ::ActiveSupport::Inflector #: untyped
      validations.each do |key, options|
        name = "#{inflector.camelize(key.to_s)}Validator"
        begin
          validator = validator_class(klass, name)
        rescue NameError
          raise ArgumentError, "Unknown validator: '#{name}'"
        end
        next unless options
        klass.validates_with(validator, merge_validates_options(defaults, parse_validates_options(options)))
      end
      nil
    end

    # ActiveModel::Validations::ClassMethods#validates_with.
    def self.validates_with(klass, args, block)
      klass = klass #: untyped
      classes = args.dup #: Array[untyped]
      block = block #: untyped
      port!("ActiveModel::Validations", "validates_with")
      options = extract_options!(classes)
      options[:class] = klass
      validators = validators_of(klass)
      classes.each do |validator_class|
        validator = validator_class.new(options.dup, &block)
        if validator.respond_to?(:attributes) && !validator.attributes.empty?
          validator.attributes.each { |attribute| validators[attribute.to_sym] << validator }
        else
          validators[nil] << validator
        end
        klass.validate(validator, options)
      end
      nil
    end

    # ClassMethods#validates_each: a BlockValidator over the attributes.
    def self.validates_each(klass, attr_names, block)
      klass = klass #: untyped
      names = attr_names.dup #: Array[untyped]
      block = block #: untyped
      port!("ActiveModel::Validations", "validates_each")
      klass.validates_with(::ActiveModel::BlockValidator, merge_attributes(names), &block)
    end

    # A `validates_<kind>_of` helper: HelperMethods#_merge_attributes into
    # validates_with, with the named validator of Active Record's
    # Validations, else Active Model's.
    def self.helper(klass, name, attr_names)
      klass = klass #: untyped
      name = name #: String
      names = attr_names.dup #: Array[untyped]
      port!("ActiveModel::Validations", "validates_with(#{name})")
      validator = nil #: untyped
      validator_namespaces.each do |namespace|
        validator = namespace.const_get(name, false) if validator.nil? && namespace.const_defined?(name, false)
      end
      if validator.nil?
        raise NotImplementedError, "roundhouse: #{klass.name}: #{name} is not ported on this tree " \
          "(runtime/spinel/active_record_validations.rb)"
      end
      klass.validates_with(validator, merge_attributes(names))
    end

    # `const_get("#{key.camelize}Validator")` from the class, as Rails'
    # `validates` resolves it: the class and its ancestors, then the
    # Validations modules ActiveRecord::Base includes, then Object.
    def self.validator_class(klass, name)
      klass = klass #: untyped
      name = name #: String
      mods = klass.ancestors #: Array[untyped]
      mods.each do |mod|
        if mod.equal?(::Object)
          validator_namespaces.each { |namespace| return namespace.const_get(name, false) if namespace.const_defined?(name, false) }
          break
        end
        return mod.const_get(name, false) if mod.const_defined?(name, false)
      end
      klass.const_get(name)
    end

    def self.validator_namespaces
      namespaces = [] #: Array[untyped]
      active_record = ::ActiveRecord #: untyped
      namespaces << ::ActiveRecord::Validations if active_record.const_defined?(:Validations, false)
      namespaces << ::ActiveModel::Validations if defined?(::ActiveModel::Validations)
      namespaces
    end

    # The class's `_validators`, created on first use from its
    # superclass's.
    def self.validators_of(klass)
      klass = klass #: untyped
      own = klass.instance_variable_get(:@__runtime_validators) #: Hash[untyped, Array[untyped]]?
      return own if own
      # Hash.new { |hash, key| hash[key] = [] }
      fresh = {} #: Hash[untyped, Array[untyped]]
      table = fresh #: untyped
      validations = ActiveRecord::RuntimeValidations #: untyped
      table.default_proc = validations.method(:new_list).to_proc
      unless klass.equal?(ActiveRecord::Base)
        validators_of(klass.superclass).each { |key, list| fresh[key] = list.dup }
      end
      klass.instance_variable_set(:@__runtime_validators, fresh)
      fresh
    end

    def self.new_list(hash, key)
      lists = hash #: Hash[untyped, Array[untyped]]
      key = key #: untyped
      lists[key] = []
    end

    # ClassMethods#validators: `_validators.values.flatten.uniq`.
    def self.all_validators(klass)
      found = [] #: Array[untyped]
      validators_of(klass).each_value { |list| list.each { |validator| found << validator } }
      found.uniq
    end

    # ClassMethods#validators_on.
    def self.validators_on(klass, attributes)
      names = attributes #: Array[untyped]
      validators = validators_of(klass)
      found = [] #: Array[untyped]
      names.each { |attribute| validators[attribute.to_sym].each { |validator| found << validator } }
      found
    end

    # The class's own registrations, in order.
    def self.registrations(klass)
      klass = klass #: untyped
      own = klass.instance_variable_get(:@__runtime_validations) #: Array[Entry]?
      return own if own
      fresh = [] #: Array[Entry]
      klass.instance_variable_set(:@__runtime_validations, fresh)
      fresh
    end

    # The record's validate chain, run (Validations#run_validations!).
    def self.run(record)
      record = record #: untyped
      chain = chain(record.class)
      if chain.nil?
        record.__send__(:validate)
      else
        kernel = ::Kernel #: untyped
        kernel.catch(:abort) do
          chain.each { |entry| entry.run(record) }
        end
      end
      nil
    end

    # The chain for `klass`, rebuilt when a registration anywhere changed
    # the chains; nil when nothing in its lineage registered at run time.
    def self.chain(klass)
      klass = klass #: untyped
      return nil if @generation.zero?
      hit = klass.instance_variable_get(:@__runtime_validation_chain) #: [Integer, Array[Entry | CompiledEntry]?]?
      return hit[1] if hit && hit[0] == @generation
      built = build(klass)
      klass.instance_variable_set(:@__runtime_validation_chain, [@generation, built])
      built
    end

    def self.build(klass)
      klass = klass #: untyped
      lineage = [] #: Array[untyped]
      c = klass #: untyped
      while c
        lineage.unshift(c)
        break if c.equal?(ActiveRecord::Base)
        c = c.superclass
      end
      registered = false
      lineage.each { |k| registered = true if k.instance_variable_get(:@__runtime_validations) }
      return nil unless registered
      owner = compiled_owner(klass)
      # A `validate` a module Base includes defines, not base.rb's no-op.
      included = owner.nil? && !klass.instance_method(:validate).owner.equal?(ActiveRecord::Base)
      chain = [] #: Array[Entry | CompiledEntry]
      lineage.each do |k|
        chain << COMPILED if included && k.equal?(ActiveRecord::Base)
        registrations(k).each { |entry| apply(chain, entry) } if k.instance_variable_get(:@__runtime_validations)
        chain << COMPILED if owner.equal?(k)
      end
      chain.freeze
    end

    # CallbackChain#append_one / #prepend_one with remove_duplicates.
    def self.apply(chain, entry)
      chain = chain #: Array[Entry | CompiledEntry]
      entry = entry #: Entry
      chain.delete_if { |other| entry.duplicates?(other) }
      entry.prepend? ? chain.unshift(entry) : chain.push(entry)
      nil
    end

    # The class in `klass`'s superclass chain, below Base, that defines
    # `validate` itself or through a module it includes.
    def self.compiled_owner(klass)
      c = klass #: untyped
      until c.nil? || c.equal?(ActiveRecord::Base)
        parent = c.superclass #: untyped
        own = (parent ? c.ancestors - parent.ancestors : c.ancestors) #: Array[untyped]
        return c if own.any? { |m| m.method_defined?(:validate, false) || m.private_method_defined?(:validate, false) }
        c = parent
      end
      nil
    end

    # ActiveRecord::Translation#lookup_ancestors, over Inheritance's
    # base_class?: Base, or a class right below Base or an abstract
    # class, ends the walk.
    def self.lookup_ancestors(klass)
      c = klass #: untyped
      classes = [c] #: Array[untyped]
      return classes if c.equal?(ActiveRecord::Base)
      parent = c.superclass #: untyped
      until parent.equal?(ActiveRecord::Base) || parent.abstract_class?
        classes << parent
        parent = parent.superclass
      end
      classes
    end

    def self.human_attribute_name(klass, attribute, options)
      klass = klass #: untyped
      attribute = attribute #: untyped
      options = options #: untyped
      port!("ActiveModel::Translation", "human_attribute_name")
      translation = ::ActiveModel::Translation #: untyped
      translation.instance_method(:human_attribute_name).bind_call(klass, attribute, options)
    end

    def self.model_name(klass)
      klass = klass #: untyped
      port!("ActiveModel::Naming", "model_name")
      naming = ::ActiveModel::Naming #: untyped
      naming.instance_method(:model_name).bind_call(klass)
    end

    # Validations#read_attribute_for_validation.
    def self.read_attribute(record, attribute)
      record = record #: untyped
      attribute = attribute #: untyped
      record.respond_to?(attribute, true) ? record.send(attribute) : nil
    end

    def self.reflect_on_association(klass, association)
      klass = klass #: untyped
      association = association #: untyped
      klass.respond_to?(:reflect_on_association) ? klass.reflect_on_association(association) : nil
    end

    # Array#extract_options!.
    def self.extract_options!(args)
      list = args #: Array[untyped]
      last = list.last #: untyped
      return {} unless last.is_a?(Hash)
      hash = last #: untyped
      extractable = hash.respond_to?(:extractable_options?) ? hash.extractable_options? : hash.instance_of?(Hash)
      return {} unless extractable
      options = list.pop #: Hash[Symbol, untyped]
      options
    end

    # Callback#check_conditionals.
    def self.conditionals(value)
      value = value #: untyped
      return EMPTY if value.nil? || value == false || (value.respond_to?(:empty?) && value.empty?)
      list = Array(value) #: Array[untyped]
      list.each do |c|
        if c.is_a?(String)
          raise ArgumentError, "Passing string to be evaluated in :if and :unless conditional options is not " \
            "supported. Pass a symbol for an instance method, or a lambda, proc or block, instead."
        end
      end
      list.freeze
    end

    # ClassMethods#predicate_for_validation_context, one per context
    # list.
    def self.context_predicate(context)
      given = context #: untyped
      contexts = (given.is_a?(Array) ? given.sort : Array(given)) #: Array[untyped]
      predicates = @context_predicates
      found = predicates[contexts] #: OnContext?
      return found if found
      predicate = OnContext.new(contexts)
      predicates[contexts] = predicate
      predicate
    end

    # HelperMethods#_merge_attributes.
    def self.merge_attributes(attr_names)
      given = attr_names #: Array[untyped]
      names = given.flatten #: Array[untyped]
      options = {} #: Hash[untyped, untyped]
      extract_options!(names).each { |key, value| options[key.respond_to?(:to_sym) ? key.to_sym : key] = value }
      options[:attributes] = names
      options
    end

    # ClassMethods#_parse_validates_options.
    def self.parse_validates_options(options)
      options = options #: untyped
      if options == true
        {}
      elsif options.is_a?(Hash)
        options
      elsif options.is_a?(Range) || options.is_a?(Array)
        { in: options }
      else
        { with: options }
      end
    end

    # ClassMethods#_merge_validates_options.
    def self.merge_validates_options(defaults, validator_options)
      merged = defaults.dup #: Hash[untyped, untyped]
      given = validator_options #: Hash[untyped, untyped]
      given.each do |key, value|
        merged[key] = merged.key?(key) && (key == :if || key == :unless || key == :on) ? Array(merged[key]) + Array(value) : value
      end
      merged
    end

    def self.port!(port, what)
      port = port #: String
      what = what #: String
      ported =
        if port == "ActiveModel::Validations"
          defined?(::ActiveModel::EachValidator) && defined?(::ActiveModel::Validations::ClassMethods)
        elsif port == "ActiveModel::Naming"
          defined?(::ActiveModel::Naming)
        else
          defined?(::ActiveModel::Translation)
        end
      return nil if ported
      raise NotImplementedError, "roundhouse: ActiveRecord::Base #{what} at run time goes through #{port} " \
        "(runtime/spinel/active_model_validations.rb), which this tree does not carry"
    end

    # The record's accumulator, extended once with Errors#add.
    def self.accumulator(errors, record)
      list = errors #: untyped
      record = record #: untyped
      if ActiveRecord::RuntimeValidations.plain_array?(list)
        list.extend(ActiveRecord::RuntimeValidations::Errors)
        list.instance_variable_set(:@__roundhouse_record, record)
      end
      list
    end

    def self.plain_array?(value)
      value = value #: untyped
      value.instance_of?(Array) && !value.is_a?(ActiveRecord::RuntimeValidations::Errors) && !value.frozen?
    end

    # ActiveModel::Errors#add over the accumulator: the Error Rails builds
    # (the port's, which words its message through I18n), raised for
    # `strict:`, else its full message pushed with its attribute, short
    # message, type and options (ActiveModel::ErrorAttributes.add, where
    # the tree carries it: a tree whose code reads no error detail pushes
    # the message alone, as its compiled rules do). Answers the Error.
    def self.add_error(errors, attribute, type, options)
      list = errors #: untyped
      attribute = attribute #: untyped
      type = type #: untyped
      options = options #: Hash[Symbol, untyped]
      unless defined?(::ActiveModel::Error) && ::ActiveModel::Error.respond_to?(:generate_message)
        raise NotImplementedError, "roundhouse: errors.add(#{attribute.inspect}, #{type.inspect}) builds an ActiveModel::Error " \
          "(runtime/spinel/active_model_validations.rb), which this tree does not carry"
      end
      record = list.instance_variable_get(:@__roundhouse_record) #: untyped
      type = type.call(record, options) if type.respond_to?(:call)
      name = attribute.to_sym #: Symbol
      error = ::ActiveModel::Error.new(record, name, type, **options) #: untyped
      exception = options[:strict] #: untyped
      if exception
        exception = ::ActiveModel::StrictValidationFailed if exception == true
        raise exception, error.full_message
      end
      full = error.full_message #: untyped
      short = error.message #: untyped
      prefix = short.is_a?(String) && full.is_a?(String) && full.end_with?(short) ? full[0, full.length - short.length] : nil
      if defined?(::ActiveModel::ErrorAttributes)
        error_attributes = ::ActiveModel::ErrorAttributes #: untyped
        error_attributes.add(list, name, full, prefix, error.raw_type, error.options)
      else
        list << full
      end
      error
    end

    # What a compiled model's `errors` answers beyond the Array.
    module Errors
      def add(attribute, type = :invalid, **options)
        ActiveRecord::RuntimeValidations.add_error(self, attribute, type, options)
      end
    end

    # A validate callback (ActiveSupport::Callbacks::Callback) with its
    # CallTemplate and conditionals.
    class Entry
      def initialize(filter, if_conditions, unless_conditions, prepend)
        @filter = filter #: untyped
        @if = if_conditions #: Array[untyped]
        @unless = unless_conditions #: Array[untyped]
        @prepend = prepend #: bool
      end

      def filter
        @filter
      end

      def prepend?
        @prepend
      end

      def compiled?
        false
      end

      # Callback#duplicates?: a method name replaces its earlier entry.
      def duplicates?(other)
        other = other #: untyped
        @filter.is_a?(Symbol) && !other.compiled? && other.filter == @filter
      end

      def run(record)
        record = record #: untyped
        @if.each { |c| return nil unless Entry.call(c, record) }
        @unless.each { |c| return nil if Entry.call(c, record) }
        Entry.call(@filter, record)
        nil
      end

      # CallTemplate: a Symbol is sent to the record, a Proc
      # instance_exec'd on it (an arity above 1 wants the block a
      # validate callback does not have), anything else answers
      # `validate(record)` (`scope: :name`).
      def self.call(filter, record)
        filter = filter #: untyped
        record = record #: untyped
        if filter.is_a?(Symbol)
          record.send(filter)
        elsif filter.is_a?(::Proc)
          callable = filter #: untyped
          arity = callable.arity #: Integer
          raise ArgumentError if arity > 1
          arity > 0 ? record.instance_exec(record, &filter) : record.instance_exec(&filter)
        else
          filter.public_send(:validate, record)
        end
      end
    end

    # The `on:` predicate (`->(model) { context.include?(model.
    # validation_context) }`, any of an Array context) and `except_on:`'s
    # (`intersect?`), as condition objects: CallTemplate calls an object
    # condition's `validate(record)` (`scope: :name`).
    class OnContext
      def initialize(contexts)
        @contexts = contexts #: Array[untyped]
      end

      def validate(record)
        record = record #: untyped
        current = record.validation_context #: untyped
        return @contexts.include?(current) unless current.is_a?(Array)
        list = current #: Array[untyped]
        list.any? { |c| @contexts.include?(c) }
      end
    end

    class ExceptOnContext
      def initialize(contexts)
        @contexts = contexts #: Array[untyped]
      end

      def validate(record)
        record = record #: untyped
        current = Array(record.validation_context) #: Array[untyped]
        @contexts.intersect?(current)
      end
    end

    # The compiled `validate`, as the chain runs it.
    class CompiledEntry
      def filter
        nil
      end

      def prepend?
        false
      end

      def compiled?
        true
      end

      def duplicates?(_other)
        false
      end

      def run(record)
        record = record #: untyped
        record.__send__(:validate)
        nil
      end
    end

    COMPILED = CompiledEntry.new
  end
end
