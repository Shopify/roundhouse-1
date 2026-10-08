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

    # `Model.transaction { ... }` — the block inside BEGIN/COMMIT, with
    # ROLLBACK + re-raise on any exception. Flat transactions only: the
    # corpus never nests (a nested BEGIN would error in SQLite rather
    # than silently join, which is the honest failure).
    def self.transaction
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
    
    def self._cast_type_for_spec(name, spec)
      kind = spec[0]
      case kind
      when :integer then ActiveRecord::Type::Integer.new(limit: spec[1])
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
    def valid?(context = nil)
      previous = @validation_context
      @validation_context = context || (new_record? ? :create : :update)
      @errors = []
      validate
      ok = @errors.empty?
      @validation_context = previous
      ok
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
      current = attributes
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

    def _note_hydrated
      @__last_saved_attributes = attributes
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

    def has_attribute?(name)
      return false unless self.class.schema_columns.include?(name)
      unloaded = @__unloaded_columns
      unloaded.nil? || !unloaded.include?(name)
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
      attributes.each do |key, value|
        prev = previous.nil? ? nil : previous[key]
        changes[key] = [prev, value] if prev != value
      end
      changes
    end

    # Rails' `<col>_was`: the value before the pending change, or the
    # current value when nothing is pending. Both halves are the
    # `attributes` form, so a datetime column answers its raw stored
    # text, as `attribute_previously_was` already does.
    def attribute_was(name)
      pair = changes_to_save[name]
      pair.nil? ? attributes[name] : pair[0]
    end
  end
end
