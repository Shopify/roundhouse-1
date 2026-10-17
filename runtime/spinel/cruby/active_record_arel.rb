# Arel's node tree on this runtime's connection, models and Relation.
#
# runtime/arel.rb is Rails' Arel as it ships. Rails binds it to Active
# Record in three places, written out here over this runtime's pieces:
#
# - the connection: `connection.visitor` (abstract_adapter.rb) answers the
#   visitor the adapter picks, built over the adapter's quoting
#   (abstract/quoting.rb `quote`, `quote_table_name`, `quote_column_name`,
#   `cast_bound_value`, `sanitize_as_sql_comment`) and the comparison
#   builders the visitor calls (abstract_adapter.rb
#   `case_sensitive_comparison`, `case_insensitive_comparison`);
# - the model: `Model.arel_table` (core.rb), which carries the model's
#   type caster (type_caster.rb, type_caster/map.rb) so a value compared
#   with a column is serialized through the column's type;
# - the relation: `where`, `not`, `having`, `order`, `group`, `select`,
#   `joins`, `from`, `pluck` and `update_all` taking nodes
#   (relation/query_methods.rb builds the same SelectManager and visits
#   it), and `Relation#arel` (relation.rb `build_arel`), here the
#   relation's accumulated clauses as a SelectManager.
#
# This runtime's Relation composes SQL text, so a node is rendered to
# its SQL where the Relation takes it; the nodes themselves are Rails'.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require_relative "arel"

module ActiveRecord
  module TypeCaster
    # type_caster/map.rb: serializes a value through the type of the
    # column it is compared with.
    class Map
      def initialize(klass)
        @klass = klass
      end

      def type_cast_for_database(attr_name, value)
        type = @klass.type_for_attribute(attr_name)
        type.serialize(value)
      end

      def type_for_attribute(attr_name)
        @klass.type_for_attribute(attr_name)
      end
    end
  end

  module ConnectionAdapters
    # What `Arel::Visitors::ToSql` asks of its connection. Values quote
    # as this runtime's adapter writes them into every other condition
    # (`ActiveRecord.adapter.escape_value`), so a node and a hash condition
    # agree on a Time, a boolean or a string.
    class ArelConnection
      def initialize(adapter)
        @adapter = adapter
      end

      def quote(value)
        case value
        when nil then "NULL"
        when Symbol then ActiveRecord.adapter.escape_value(value.name)
        when Float then value.to_s
        else
          return value.to_s("F") if defined?(::BigDecimal) && value.is_a?(::BigDecimal)

          ActiveRecord.adapter.escape_value(value)
        end
      end

      def quote_table_name(name)
        @adapter.quote_table_name(name)
      end

      def quote_column_name(name)
        @adapter.quote_column_name(name)
      end

      def cast_bound_value(value) # :nodoc:
        value
      end

      def sanitize_as_sql_comment(value) # :nodoc:
        comment = value.to_s.dup
        comment.gsub!(%r{\A\s*/\*\+?\s?|\s?\*/\s*\Z}, "")
        comment.gsub!("*/", "* /")
        comment.gsub!("/*", "/ *")
        comment
      end

      def case_sensitive_comparison(attribute, value) # :nodoc:
        attribute.eq(value)
      end

      def case_insensitive_comparison(attribute, value) # :nodoc:
        attribute.lower.eq(attribute.relation.lower(value))
      end

      # The visitor asks whether to emit binds only for a prepared
      # statement; this runtime inlines every value.
      def prepared_statements
        false
      end
    end

    class AbstractAdapter
      # abstract_adapter.rb: the visitor this adapter's SQL dialect picks.
      def visitor
        @arel_visitor ||= Arel::Visitors::PostgreSQL.new(ArelConnection.new(self))
      end
    end
  end

  class Base
    # core.rb `arel_table`.
    def self.arel_table
      @arel_table ||= Arel::Table.new(name: table_name, klass: self)
    end

    # type_caster.rb (`Model.type_caster`).
    def self.type_caster # :nodoc:
      TypeCaster::Map.new(self)
    end
  end

  module ::Arel
    class Table
      # `Table#[]` under the name the shared runtime's Arel shim reads
      # it by (`src/lower/arel_attribute.rs` writes `attribute` for a
      # typed `table[:col]`, so every target calls one name).
      alias_method :attribute, :[]
    end
  end

  # table.rb `Table.engine`: the connection source a node's `to_sql`
  # compiles through (core.rb `arel_table` passes `klass:` for models).
  Arel::Table.engine = Base

  class Relation
    # Arguments that are Arel trees rather than SQL text, and how they
    # reach the text this Relation composes.
    module ArelArguments
      def where(condition = nil, *args)
        super(arel_text(condition), *args)
      end

      def not(condition = nil, *args)
        super(arel_text(condition), *args)
      end

      def having(condition, *args)
        super(arel_text(condition), *args)
      end

      # The in-place body: `order` spawns and calls it, `reorder!` clears
      # the orders and calls it, and default-scope lowering calls it.
      #
      # Rails flattens its arguments (`order([a.asc, b.desc])`, `order(*list)`
      # alike), so an Array of nodes is read term by term.
      def order!(*parts)
        parts = parts.flatten
        return super(*parts) unless parts.any? { |part| arel_tree?(part) }

        parts.each do |part|
          if arel_tree?(part)
            order_sql!(arel_text(part))
          else
            super(part)
          end
        end
        self
      end

      def group(*parts)
        super(*parts.map { |part| arel_text(part) })
      end

      def select(*specs)
        super(*specs.map { |spec| arel_text(spec) })
      end

      # `pluck(node, ...)`: each node's SQL is a result column, as
      # relation.rb `pluck` projects it. An attribute of this model
      # reads back through the column's declared type; any other node
      # answers as the database types it.
      def pluck(*cols)
        return super unless cols.any? { |col| arel_tree?(col) }

        prior = @select_sql
        names = cols.map do |col|
          if arel_tree?(col)
            col.is_a?(Arel::Attributes::Attribute) && col.relation.name.to_s == @table.to_s ? col.name.to_s : nil
          else
            @model._attribute_name(col.to_s)
          end
        end
        @select_sql = cols.each_with_index.map do |col, i|
          expression = arel_tree?(col) ? arel_text(col) : "#{@table}.#{names[i]}"
          "#{expression} AS v#{i}"
        end.join(", ")
        rows = ActiveRecord.adapter.select_rows(to_sql)
        @select_sql = prior
        read = lambda do |row, i|
          names[i] ? @model._attribute_deserialize(names[i], row["v#{i}"]) : row["v#{i}"]
        end
        return rows.map { |row| read.call(row, 0) } if cols.length == 1

        rows.map { |row| (0...cols.length).map { |i| read.call(row, i) } }
      end

      def joins(spec)
        super(arel_join_spec(spec))
      end

      def left_outer_joins(spec)
        super(arel_join_spec(spec))
      end

      def from(source, subquery_name = nil)
        super(arel_text(source), subquery_name)
      end

      # `update_all(col => node)`: a node's SQL stands where the value's
      # literal would, as the SET clause of the UPDATE AST holds it
      # (relation.rb `update_all`).
      def update_all(updates)
        return super unless updates.is_a?(Hash) && updates.any? { |_, value| arel_tree?(value) }

        set_sql = updates.map do |key, value|
          literal = if arel_tree?(value)
            arel_text(value)
          else
            ActiveRecord.adapter.escape_value(@model._attribute_for_database(key.to_s, value))
          end
          "#{key} = #{literal}"
        end.join(", ")
        ActiveRecord.adapter.execute_ddl("UPDATE #{@table} SET #{set_sql}#{scoped_write_where}")
        ActiveRecord.adapter.changes
      end

      # relation.rb `table`: the model's Arel table.
      def table
        @model.arel_table
      end

      # relation.rb `arel` / `build_arel`: the accumulated clauses as a
      # SelectManager, which `where`, `project`, `join` and the rest then
      # extend, and `exists`, `union` and `to_sql` read. The clause texts
      # are the ones this Relation composes, so the manager renders the
      # query the Relation would run.
      def arel(aliases = nil)
        unless @ctes.empty?
          raise NotImplementedError, "roundhouse: Relation#arel on a relation with a WITH clause is not ported: " \
            "the clause is held as SQL text, with no Arel::Nodes::With to carry it"
        end
        table = @model.arel_table
        manager = Arel::SelectManager.new(table)
        manager.from(@from) unless @from.nil?
        manager.project(@select_sql.nil? ? table[Arel.star] : Arel.sql(@select_sql))
        manager.distinct if @distinct
        @joins.each { |join| manager.join(Arel.sql(join)) }
        @wheres.each { |where| manager.where(Arel.sql(where)) }
        @groups.each { |group| manager.group(Arel.sql(group)) }
        @havings.each { |having| manager.having(Arel.sql(having)) }
        @orders.each { |order| manager.order(Arel.sql(order)) }
        manager.take(@limit) unless @limit.nil?
        manager.skip(@offset) unless @offset.nil?
        manager.lock(@lock) unless @lock.nil?
        manager
      end

      private

      # A tree this module renders: a node, an attribute, a manager.
      # SQL text (a String or `Arel.sql` literal) is what the Relation
      # already takes.
      def arel_tree?(value)
        value.is_a?(Arel::Nodes::Node) || value.is_a?(Arel::Attributes::Attribute) ||
          value.is_a?(Arel::TreeManager) || value.is_a?(Arel::Table)
      end

      def arel_text(value)
        return value unless arel_tree?(value)

        # relation.rb `to_sql`: the tree's binds (a QueryAttribute in a
        # VALUES row, a BindParam) are substituted as quoted literals,
        # since the Relation holds this as text and lifts the literals to
        # its own numbered binds when it runs. Compiled with the plain
        # collector their `$n` placeholders would number from 1 again
        # inside the Relation's own binds.
        ActiveRecord::Base.with_connection do |connection|
          collector = Arel::Collectors::SubstituteBinds.new(
            ActiveRecord::ConnectionAdapters::ArelConnection.new(connection), Arel::Collectors::SQLString.new
          )
          connection.visitor.compile(value, collector)
        end
      end

      def arel_join_spec(spec)
        return spec.map { |part| arel_join_spec(part) } if spec.is_a?(Array)

        arel_text(spec)
      end
    end

    prepend ArelArguments

    # The models a relation's association joins reach. `joins` records the
    # SQL of each hop and not the model, while Rails' predicate builder
    # types a nested table the model's own reflections do not name from the
    # join dependencies (`where(condition_responders: { responder_id: id })`
    # after `joins(condition: [:condition_responders])`:
    # relation/query_methods.rb `lookup_reflection_from_join_dependencies`,
    # a joined reflection by its name or its table). A `model_uuid_attributes`
    # column of that model then serializes its integer to a uuid, where an
    # untyped bind reaches the uuid column as an integer's text.
    # type_caster/connection.rb: the types of a plain table's columns, from
    # the database (`where(context_driver_group_responders: { shop_id: 1 })`
    # over a table only a `through` join reaches). A uuid column serializes
    # its value as the adapter's uuid type does (Core's
    # PostgreSQLUuidIntegerCast gives an Integer its uuid); any other column,
    # and a name that is no table, binds as written.
    class TableTypeCaster
      def initialize(model, table_name)
        @model = model
        @table_name = table_name
      end

      def _attribute_force_equality?(_name, _value)
        false
      end

      # No model, so no `alias_attribute`: the key is the column.
      def _attribute_name(name)
        name
      end

      def _attribute_for_database(name, value)
        column = uuid_column(name)
        column.nil? ? value : column.cast_type.serialize(value)
      end

      private

      def uuid_column(name)
        return nil unless @model.respond_to?(:connection)

        cache = @model.connection.schema_cache
        return nil unless cache.data_source_exists?(@table_name)

        column = cache.columns_hash(@table_name)[name.to_s]
        column.nil? || column.type != :uuid ? nil : column
      end
    end

    module JoinedTables
      def joins!(spec)
        result = super
        note_joined_models(@model, spec)
        result
      end

      def left_outer_joins!(spec)
        result = super
        note_joined_models(@model, spec)
        result
      end

      private

      def nested_table(key)
        pair = super
        return pair unless pair[1].nil?

        joined = (@join_models || {})[pair[0]]
        return [pair[0], joined] unless joined.nil?

        # A table no association of the model or its joins explains is typed
        # by its columns (type_caster/connection.rb), as a model's own are.
        [pair[0], TableTypeCaster.new(@model, pair[0])]
      end

      def note_joined_models(owner, spec)
        case spec
        when Symbol
          reflection = owner.respond_to?(:reflect_on_association) ? owner.reflect_on_association(spec) : nil
          return nil if reflection.nil? || !reflection.respond_to?(:polymorphic?) || reflection.polymorphic?

          klass = reflection.klass
          # An earlier join of the name keeps it (Rails takes the first).
          @join_models = { reflection.name.to_s => klass, klass.table_name.to_s => klass }.merge(@join_models || {})
          klass
        when Array
          spec.each { |part| note_joined_models(owner, part) }
          nil
        when Hash
          spec.each do |name, nested|
            klass = note_joined_models(owner, name)
            note_joined_models(klass, nested) unless klass.nil?
          end
          nil
        end
      end
    end

    prepend JoinedTables
  end
end

module ActiveRecord
  # `select_all(Arel.sql(sql, *params))`: an Arel tree given as the SQL of
  # `_with_binds` has its own binds (`Arel.sql("a = ?", 1)`, the apps
  # runtime's `Statement#params`) written in as quoted literals, as the
  # unprepared `to_sql_and_binds` does. Compiled plainly by `to_sql` they
  # would be `$n` placeholders no bind answers.
  module ArelSubstitutedBinds
    def _with_binds(sql, binds)
      if sql.is_a?(::Arel::Nodes::Node)
        sql = ActiveRecord::Base.with_connection do |connection|
          collector = ::Arel::Collectors::SubstituteBinds.new(
            ActiveRecord::ConnectionAdapters::ArelConnection.new(connection), ::Arel::Collectors::SQLString.new
          )
          connection.visitor.compile(sql, collector)
        end
      end
      super(sql, binds)
    end
  end
  Connection.prepend(ArelSubstitutedBinds)
end
