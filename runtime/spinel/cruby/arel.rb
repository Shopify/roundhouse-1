# frozen_string_literal: true
# Arel (Shopify/rails 5eb8c4c45bee, 8.2.0.alpha activerecord/lib/arel.rb and
# arel/*), carried over as Rails ships it: the node classes, the
# predication, math and expression mixins, the managers, the SQL string
# collectors and the ToSql / PostgreSQL / SQLite visitors, in `arel.rb`'s
# own load order with its `require "arel/…"` lines folded away. MySQL's
# visitor and the Dot visitor are not carried.
#
# Changes from the source, all mechanical:
# - `const_set(name, Class.new(Parent))` and `And = Class.new(Nary)` are
#   written as `class Name < Parent; end`, so a constant read is resolved
#   from the file's text.
# - `require "active_model/attribute"` is dropped (ActiveModel::Attribute
#   is runtime/active_model_attribute.rb, loaded before this file).
# - Rails' `Arel::VERSION` is not defined.
#
# It replaces the string-composed shim of runtime/ruby/active_record/arel.rb
# (`Arel::Table`, `Arel::Attribute`, `Arel::SelectManager`,
# `Arel::Nodes`), which the shared runtime keeps for the targets that
# have no node tree. The glue that binds it to this runtime's connection,
# Relation and models is runtime/active_record_arel.rb.
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

module Arel
  [:Table, :Attribute, :SelectManager, :Nodes].each do |shim|
    remove_const(shim) if const_defined?(shim, false)
  end
end

# ---- arel/errors.rb ----
module Arel # :nodoc: all
  class ArelError < StandardError
  end

  class EmptyJoinError < ArelError
  end

  class BindError < ArelError
    def initialize(message, sql = nil)
      if sql
        super("#{message} in: #{sql.inspect}")
      else
        super(message)
      end
    end
  end
end

# ---- arel/crud.rb ----
module Arel # :nodoc: all
  ###
  # FIXME hopefully we can remove this
  module Crud
    def compile_insert(values)
      im = create_insert
      im.insert values
      im
    end

    def create_insert
      InsertManager.new
    end

    def compile_update(values, key = nil)
      um = UpdateManager.new(source)
      um.set(values)
      um.take(limit)
      um.offset(offset)
      um.order(*orders)
      um.wheres = constraints
      um.comment(comment)
      um.key = key

      um.ast.groups = @ctx.groups
      @ctx.havings.each { |h| um.having(h) }
      um
    end

    def compile_delete(key = nil)
      dm = DeleteManager.new(source)
      dm.take(limit)
      dm.offset(offset)
      dm.order(*orders)
      dm.wheres = constraints
      dm.comment(comment)
      dm.key = key
      dm.ast.groups = @ctx.groups
      @ctx.havings.each { |h| dm.having(h) }
      dm
    end
  end
end

# ---- arel/factory_methods.rb ----
module Arel # :nodoc: all
  ###
  # Methods for creating various nodes
  module FactoryMethods
    def create_true
      Arel::Nodes::True.new
    end

    def create_false
      Arel::Nodes::False.new
    end

    def create_table_alias(relation, name)
      Nodes::TableAlias.new(relation, name)
    end

    def create_join(to, constraint = nil, klass = Nodes::InnerJoin)
      klass.new(to, constraint)
    end

    def create_string_join(to)
      create_join to, nil, Nodes::StringJoin
    end

    def create_and(clauses)
      Nodes::And.new clauses
    end

    def create_on(expr)
      Nodes::On.new expr
    end

    def grouping(expr)
      Nodes::Grouping.new expr
    end

    ###
    # Create a LOWER() function
    def lower(column)
      Nodes::NamedFunction.new "LOWER", [Nodes.build_quoted(column)]
    end

    def coalesce(*exprs)
      Nodes::NamedFunction.new "COALESCE", exprs
    end

    def cast(name, type)
      Nodes::NamedFunction.new "CAST", [name.as(type)]
    end
  end
end

# ---- arel/expressions.rb ----
module Arel # :nodoc: all
  module Expressions
    def count(distinct = false)
      Nodes::Count.new [self], distinct
    end

    def sum
      Nodes::Sum.new [self]
    end

    def maximum
      Nodes::Max.new [self]
    end

    def minimum
      Nodes::Min.new [self]
    end

    def average
      Nodes::Avg.new [self]
    end

    def extract(field)
      Nodes::Extract.new [self], field
    end
  end
end

# ---- arel/predications.rb ----
module Arel # :nodoc: all
  module Predications
    def not_eq(other)
      Nodes::NotEqual.new self, quoted_node(other)
    end

    def not_eq_any(others)
      grouping_any :not_eq, others
    end

    def not_eq_all(others)
      grouping_all :not_eq, others
    end

    def eq(other)
      Nodes::Equality.new self, quoted_node(other)
    end

    def case_sensitive_eq(other)
      Nodes::CaseSensitiveEquality.new self, quoted_node(other)
    end

    def case_insensitive_eq(other)
      Nodes::CaseInsensitiveEquality.new self, quoted_node(other)
    end

    def is_not_distinct_from(other)
      Nodes::IsNotDistinctFrom.new self, quoted_node(other)
    end

    def is_distinct_from(other)
      Nodes::IsDistinctFrom.new self, quoted_node(other)
    end

    def eq_any(others)
      grouping_any :eq, others
    end

    def eq_all(others)
      grouping_all :eq, quoted_array(others)
    end

    def between(other)
      if unboundable?(other.begin) == 1 || unboundable?(other.end) == -1
        self.in([])
      elsif open_ended?(other.begin)
        if open_ended?(other.end)
          if infinity?(other.begin) == 1 || infinity?(other.end) == -1
            self.in([])
          else
            not_in([])
          end
        elsif other.exclude_end?
          lt(other.end)
        else
          lteq(other.end)
        end
      elsif open_ended?(other.end)
        gteq(other.begin)
      elsif other.exclude_end?
        gteq(other.begin).and(lt(other.end))
      elsif other.begin == other.end
        eq(other.begin)
      else
        left = quoted_node(other.begin)
        right = quoted_node(other.end)
        Nodes::Between.new(self, Nodes::And.new([left, right]))
      end
    end

    def in(other)
      case other
      when Arel::SelectManager
        Arel::Nodes::In.new(self, other.ast)
      when Enumerable
        Nodes::In.new self, quoted_array(other)
      else
        Nodes::In.new self, quoted_node(other)
      end
    end

    def in_any(others)
      grouping_any :in, others
    end

    def in_all(others)
      grouping_all :in, others
    end

    def not_between(other)
      if unboundable?(other.begin) == 1 || unboundable?(other.end) == -1
        not_in([])
      elsif open_ended?(other.begin)
        if open_ended?(other.end)
          if infinity?(other.begin) == 1 || infinity?(other.end) == -1
            not_in([])
          else
            self.in([])
          end
        elsif other.exclude_end?
          gteq(other.end)
        else
          gt(other.end)
        end
      elsif open_ended?(other.end)
        lt(other.begin)
      else
        left = lt(other.begin)
        right = if other.exclude_end?
          gteq(other.end)
        else
          gt(other.end)
        end
        left.or(right)
      end
    end

    def not_in(other)
      case other
      when Arel::SelectManager
        Arel::Nodes::NotIn.new(self, other.ast)
      when Enumerable
        Nodes::NotIn.new self, quoted_array(other)
      else
        Nodes::NotIn.new self, quoted_node(other)
      end
    end

    def not_in_any(others)
      grouping_any :not_in, others
    end

    def not_in_all(others)
      grouping_all :not_in, others
    end

    def matches(other, escape = nil, case_sensitive = false)
      Nodes::Matches.new self, quoted_node(other), escape, case_sensitive
    end

    def matches_regexp(other, case_sensitive = true)
      Nodes::Regexp.new self, quoted_node(other), case_sensitive
    end

    def matches_any(others, escape = nil, case_sensitive = false)
      grouping_any :matches, others, escape, case_sensitive
    end

    def matches_all(others, escape = nil, case_sensitive = false)
      grouping_all :matches, others, escape, case_sensitive
    end

    def does_not_match(other, escape = nil, case_sensitive = false)
      Nodes::DoesNotMatch.new self, quoted_node(other), escape, case_sensitive
    end

    def does_not_match_regexp(other, case_sensitive = true)
      Nodes::NotRegexp.new self, quoted_node(other), case_sensitive
    end

    def does_not_match_any(others, escape = nil)
      grouping_any :does_not_match, others, escape
    end

    def does_not_match_all(others, escape = nil)
      grouping_all :does_not_match, others, escape
    end

    def gteq(right)
      Nodes::GreaterThanOrEqual.new self, quoted_node(right)
    end

    def gteq_any(others)
      grouping_any :gteq, others
    end

    def gteq_all(others)
      grouping_all :gteq, others
    end

    def gt(right)
      Nodes::GreaterThan.new self, quoted_node(right)
    end

    def gt_any(others)
      grouping_any :gt, others
    end

    def gt_all(others)
      grouping_all :gt, others
    end

    def lt(right)
      Nodes::LessThan.new self, quoted_node(right)
    end

    def lt_any(others)
      grouping_any :lt, others
    end

    def lt_all(others)
      grouping_all :lt, others
    end

    def lteq(right)
      Nodes::LessThanOrEqual.new self, quoted_node(right)
    end

    def lteq_any(others)
      grouping_any :lteq, others
    end

    def lteq_all(others)
      grouping_all :lteq, others
    end

    def when(right)
      Nodes::Case.new(self).when quoted_node(right)
    end

    def concat(other)
      Nodes::Concat.new self, other
    end

    def contains(other)
      Arel::Nodes::Contains.new self, quoted_node(other)
    end

    def overlaps(other)
      Arel::Nodes::Overlaps.new self, quoted_node(other)
    end

    def quoted_array(others)
      others.map { |v| quoted_node(v) }
    end

    private
      def grouping_any(method_id, others, *extras)
        nodes = others.map { |expr| send(method_id, expr, *extras) }
        Nodes::Grouping.new Nodes::Or.new(nodes)
      end

      def grouping_all(method_id, others, *extras)
        nodes = others.map { |expr| send(method_id, expr, *extras) }
        Nodes::Grouping.new Nodes::And.new(nodes)
      end

      def quoted_node(other)
        Nodes.build_quoted(other, self)
      end

      def infinity?(value)
        value.respond_to?(:infinite?) && value.infinite?
      end

      def unboundable?(value)
        value.respond_to?(:unboundable?) && value.unboundable?
      end

      def open_ended?(value)
        value.nil? || infinity?(value) || unboundable?(value)
      end
  end
end

# ---- arel/filter_predications.rb ----
module Arel # :nodoc: all
  module FilterPredications
    def filter(expr)
      Nodes::Filter.new(self, expr)
    end
  end
end

# ---- arel/window_predications.rb ----
module Arel # :nodoc: all
  module WindowPredications
    def over(expr = nil)
      Nodes::Over.new(self, expr)
    end
  end
end

# ---- arel/math.rb ----
module Arel # :nodoc: all
  module Math
    def *(other)
      Arel::Nodes::Multiplication.new(self, other)
    end

    def +(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::Addition.new(self, other))
    end

    def -(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::Subtraction.new(self, other))
    end

    def /(other)
      Arel::Nodes::Division.new(self, other)
    end

    def &(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::BitwiseAnd.new(self, other))
    end

    def |(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::BitwiseOr.new(self, other))
    end

    def ^(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::BitwiseXor.new(self, other))
    end

    def <<(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::BitwiseShiftLeft.new(self, other))
    end

    def >>(other)
      Arel::Nodes::Grouping.new(Arel::Nodes::BitwiseShiftRight.new(self, other))
    end

    def ~@
      Arel::Nodes::BitwiseNot.new(self)
    end
  end
end

# ---- arel/alias_predication.rb ----
module Arel # :nodoc: all
  module AliasPredication
    def as(other)
      other = other.name if other.is_a?(Symbol)

      Nodes::As.new self, Nodes::SqlLiteral.new(other, retryable: true)
    end
  end
end

# ---- arel/order_predications.rb ----
module Arel # :nodoc: all
  module OrderPredications
    def asc
      Nodes::Ascending.new self
    end

    def desc
      Nodes::Descending.new self
    end
  end
end

# ---- arel/table.rb ----
module Arel # :nodoc: all
  class Table
    include Arel::FactoryMethods
    include Arel::AliasPredication

    @engine = nil
    class << self; attr_accessor :engine; end

    attr_writer :name
    attr_reader :table_alias, :klass

    def initialize(name: nil, as: nil, klass: nil, type_caster: klass&.type_caster)
      raise ArgumentError, "A name or klass is required." unless klass || name
      name = name.name if name.is_a?(Symbol)

      @name = name
      @klass = klass
      @type_caster = type_caster

      # Sometime AR sends an :as parameter to table, to let the table know
      # that it is an Alias.  We may want to override new, and return a
      # TableAlias node?
      if as.to_s == @name
        as = nil
      end
      @table_alias = as

      ActiveSupport::Ractors.make_shareable(self)
    end

    def name
      @name || @klass&.table_name
    end

    def alias(name = "#{self.name}_2")
      Nodes::TableAlias.new(self, name)
    end

    def from
      SelectManager.new(self)
    end

    def join(relation, klass = Nodes::InnerJoin)
      return from unless relation

      case relation
      when String, Nodes::SqlLiteral
        raise EmptyJoinError if relation.empty?
        klass = Nodes::StringJoin
      end

      from.join(relation, klass)
    end

    def outer_join(relation)
      join(relation, Nodes::OuterJoin)
    end

    def group(*columns)
      from.group(*columns)
    end

    def order(*expr)
      from.order(*expr)
    end

    def where(condition)
      from.where condition
    end

    def project(*things)
      from.project(*things)
    end

    def take(amount)
      from.take amount
    end

    def skip(amount)
      from.skip amount
    end

    def having(expr)
      from.having expr
    end

    def [](name, table = self)
      name = name.name if name.is_a?(Symbol)
      name = @klass.attribute_aliases[name] || name if @klass
      Attribute.new(table, name)
    end

    def hash
      # Perf note: aliases and table alias is excluded from the hash
      #  aliases can have a loop back to this table breaking hashes in parent
      #  relations, for the vast majority of cases @name is unique to a query
      name.hash
    end

    def eql?(other)
      self.class == other.class &&
        self.name == other.name &&
        self.table_alias == other.table_alias
    end
    alias :== :eql?

    def type_cast_for_database(attr_name, value)
      type_caster.type_cast_for_database(attr_name, value)
    end

    def type_for_attribute(name)
      type_caster.type_for_attribute(name)
    end

    def able_to_type_cast?
      !type_caster.nil?
    end

    private
      attr_reader :type_caster
  end
end

# ---- arel/attributes/attribute.rb ----
module Arel # :nodoc: all
  module Attributes
    class Attribute < Struct.new :relation, :name
      include Arel::Expressions
      include Arel::Predications
      include Arel::AliasPredication
      include Arel::OrderPredications
      include Arel::Math

      def type_caster
        relation.type_for_attribute(name)
      end

      ###
      # Create a node for lowering this attribute
      def lower
        relation.lower self
      end

      def type_cast_for_database(value)
        relation.type_cast_for_database(name, value)
      end

      def able_to_type_cast?
        relation.able_to_type_cast?
      end

      def fetch_attribute(&)
        yield self
      end
    end
  end

  Attribute = Attributes::Attribute
end

# ---- arel/visitors.rb ----
module Arel # :nodoc: all
  module Visitors
  end
end

# ---- arel/collectors/plain_string.rb ----
module Arel # :nodoc: all
  module Collectors
    class PlainString
      def initialize
        @str = +""
      end

      def value
        @str
      end

      def <<(str)
        @str << str
        self
      end
    end
  end
end

# ---- arel/collectors/sql_string.rb ----
module Arel # :nodoc: all
  module Collectors
    class SQLString < PlainString
      attr_accessor :preparable, :retryable

      def initialize(*)
        super
        @bind_index = 1
      end

      def add_bind(bind, &)
        self << yield(@bind_index)
        @bind_index += 1
        self
      end

      def add_binds(binds, proc_for_binds = nil, &block)
        self << (@bind_index...@bind_index += binds.size).map(&block).join(", ")
        self
      end
    end
  end
end

# ---- arel/collectors/bind.rb ----
module Arel # :nodoc: all
  module Collectors
    class Bind
      attr_accessor :retryable

      def initialize
        @binds = []
      end

      def <<(str)
        self
      end

      def add_bind(bind, &)
        @binds << bind
        self
      end

      def add_binds(binds, proc_for_binds = nil, &)
        @binds.concat proc_for_binds ? binds.map(&proc_for_binds) : binds
        self
      end

      def value
        @binds
      end
    end
  end
end

# ---- arel/collectors/composite.rb ----
module Arel # :nodoc: all
  module Collectors
    class Composite
      attr_accessor :preparable
      attr_reader :retryable

      def initialize(left, right)
        @left = left
        @right = right
      end

      def retryable=(retryable)
        left.retryable = retryable
        right.retryable = retryable
        @retryable = retryable
      end

      def <<(str)
        left << str
        right << str
        self
      end

      def add_bind(bind, &block)
        left.add_bind bind, &block
        right.add_bind bind, &block
        self
      end

      def add_binds(binds, proc_for_binds = nil, &block)
        left.add_binds(binds, proc_for_binds, &block)
        right.add_binds(binds, proc_for_binds, &block)
        self
      end

      def value
        [left.value, right.value]
      end

      private
        attr_reader :left, :right
    end
  end
end

# ---- arel/collectors/substitute_binds.rb ----
module Arel # :nodoc: all
  module Collectors
    class SubstituteBinds
      attr_accessor :preparable, :retryable

      def initialize(quoter, delegate_collector)
        @quoter = quoter
        @delegate = delegate_collector
      end

      def <<(str)
        delegate << str
        self
      end

      def add_bind(bind, &)
        bind = bind.value_for_database if bind.respond_to?(:value_for_database)
        self << quoter.quote(bind)
      end

      def add_binds(binds, proc_for_binds = nil, &)
        self << binds.map { |bind| quoter.quote(bind) }.join(", ")
      end

      def value
        delegate.value
      end

      private
        attr_reader :quoter, :delegate
    end
  end
end

# ---- arel/tree_manager.rb ----
module Arel # :nodoc: all
  class TreeManager
    include Arel::FactoryMethods

    module StatementMethods
      def take(limit)
        @ast.limit = Nodes::Limit.new(Nodes.build_quoted(limit)) if limit
        self
      end

      def offset(offset)
        @ast.offset = Nodes::Offset.new(Nodes.build_quoted(offset)) if offset
        self
      end

      def order(*expr)
        @ast.orders = expr
        self
      end

      def key=(key)
        @ast.key = if key.is_a?(Array)
          key.map { |k| Nodes.build_quoted(k) }
        else
          Nodes.build_quoted(key)
        end
      end

      def key
        @ast.key
      end

      def wheres=(exprs)
        @ast.wheres = exprs
      end

      def where(expr)
        @ast.wheres << expr
        self
      end
    end

    attr_reader :ast

    def initialize(table = nil)
      @table = table
    end

    def to_dot
      collector = Arel::Collectors::PlainString.new
      collector = Visitors::Dot.new.accept @ast, collector
      collector.value
    end

    def to_sql(engine = nil)
      unless engine
        table = @table.is_a?(Nodes::JoinSource) ? @table.left : @table

        engine = table&.klass || Table.engine
      end

      collector = Arel::Collectors::SQLString.new
      engine.with_connection do |connection|
        connection.visitor.accept(@ast, collector).value
      end
    end

    def initialize_copy(other)
      super
      @ast = @ast.clone
    end
  end
end

# ---- arel/insert_manager.rb ----
module Arel # :nodoc: all
  class InsertManager < Arel::TreeManager
    def initialize(table = nil)
      super

      @ast = Nodes::InsertStatement.new(table)
    end

    def into(table)
      @ast.relation = table
      self
    end

    def columns; @ast.columns end
    def values=(val); @ast.values = val; end

    def select(select)
      @ast.select = select
    end

    def insert(fields)
      return if fields.empty?

      if String === fields
        @ast.values = Nodes::SqlLiteral.new(fields)
      else
        @ast.relation ||= fields.first.first.relation

        values = []

        fields.each do |column, value|
          @ast.columns << column
          values << value
        end
        @ast.values = create_values(values)
      end
      self
    end

    def returning(returning)
      @ast.returning << returning
      self
    end

    def create_values(values)
      Nodes::ValuesList.new([values])
    end

    def create_values_list(rows)
      Nodes::ValuesList.new(rows)
    end
  end
end

# ---- arel/select_manager.rb ----
module Arel # :nodoc: all
  class SelectManager < Arel::TreeManager
    include Arel::Crud

    STRING_OR_SYMBOL_CLASS = [Symbol, String].freeze

    def initialize(table = nil)
      super

      @ast = Nodes::SelectStatement.new(table)
      @ctx = @ast.cores.last
    end

    def initialize_copy(other)
      super
      @ctx = @ast.cores.last
    end

    def limit
      @ast.limit && @ast.limit.expr
    end
    alias :taken :limit

    def constraints
      @ctx.wheres
    end

    def offset
      @ast.offset && @ast.offset.expr
    end

    def skip(amount)
      if amount
        @ast.offset = Nodes::Offset.new(amount)
      else
        @ast.offset = nil
      end
      self
    end
    alias :offset= :skip

    ###
    # Produces an Arel::Nodes::Exists node
    def exists
      Arel::Nodes::Exists.new @ast
    end

    def as(other)
      create_table_alias grouping(@ast), Nodes::SqlLiteral.new(other, retryable: true)
    end

    def lock(locking = Arel.sql("FOR UPDATE"))
      case locking
      when true
        locking = Arel.sql("FOR UPDATE")
      when Arel::Nodes::SqlLiteral
      when String
        locking = Arel.sql locking
      end

      @ast.lock = Nodes::Lock.new(locking)
      self
    end

    def locked
      @ast.lock
    end

    def on(*exprs)
      @ctx.source.right.last.right = Nodes::On.new(collapse(exprs))
      self
    end

    def group(*columns)
      columns.each do |column|
        # FIXME: backwards compat
        case column
        when Nodes::SqlLiteral
        when String
          column = Nodes::SqlLiteral.new(column)
        when Symbol
          column = Nodes::SqlLiteral.new(column.name)
        end

        @ctx.groups.push Nodes::Group.new column
      end
      self
    end

    def from(table)
      table = Nodes::SqlLiteral.new(table) if String === table

      case table
      when Nodes::Join
        @ctx.source.right << table
      else
        @ctx.source.left = table
      end

      self
    end

    def froms
      @ast.cores.filter_map { |x| x.from }
    end

    def join(relation, klass = Nodes::InnerJoin)
      return self unless relation

      case relation
      when String, Nodes::SqlLiteral
        raise EmptyJoinError if relation.empty?
        klass = Nodes::StringJoin
      end

      @ctx.source.right << create_join(relation, nil, klass)
      self
    end

    def outer_join(relation)
      join(relation, Nodes::OuterJoin)
    end

    def having(expr)
      @ctx.havings << expr
      self
    end

    def window(name)
      window = Nodes::NamedWindow.new(name)
      @ctx.windows.push window
      window
    end

    def project(*projections)
      # FIXME: converting these to SQLLiterals is probably not good, but
      # rails tests require it.
      @ctx.projections.concat projections.map { |x|
        STRING_OR_SYMBOL_CLASS.include?(x.class) ? Nodes::SqlLiteral.new(x.to_s) : x
      }
      self
    end

    def projections
      @ctx.projections
    end

    def projections=(projections)
      @ctx.projections = projections
    end

    def optimizer_hints(*hints)
      unless hints.empty?
        @ctx.optimizer_hints = Arel::Nodes::OptimizerHints.new(hints)
      end
      self
    end

    def distinct(value = true)
      if value
        @ctx.set_quantifier = Arel::Nodes::Distinct.new
      else
        @ctx.set_quantifier = nil
      end
      self
    end

    def distinct_on(value)
      if value
        @ctx.set_quantifier = Arel::Nodes::DistinctOn.new(value)
      else
        @ctx.set_quantifier = nil
      end
      self
    end

    def order(*expr)
      # FIXME: We SHOULD NOT be converting these to SqlLiteral automatically
      @ast.orders.concat expr.map { |x|
        STRING_OR_SYMBOL_CLASS.include?(x.class) ? Nodes::SqlLiteral.new(x.to_s) : x
      }
      self
    end

    def orders
      @ast.orders
    end

    def where(expr)
      if Arel::TreeManager === expr
        expr = expr.ast
      end
      @ctx.wheres << expr
      self
    end

    def where_sql(engine = Table.engine)
      return if @ctx.wheres.empty?

      Nodes::SqlLiteral.new("WHERE #{Nodes::And.new(@ctx.wheres).to_sql(engine)}")
    end

    def union(operation, other = nil)
      if other
        node_class = Nodes.const_get("Union#{operation.to_s.capitalize}")
      else
        other = operation
        node_class = Nodes::Union
      end

      node_class.new self.ast, other.ast
    end

    def intersect(other)
      Nodes::Intersect.new ast, other.ast
    end

    def except(other)
      Nodes::Except.new ast, other.ast
    end
    alias :minus :except

    def lateral(table_name = nil)
      base = table_name.nil? ? ast : as(table_name)
      Nodes::Lateral.new(base)
    end

    def with(*subqueries)
      if subqueries.first.is_a? Symbol
        node_class = Nodes.const_get("With#{subqueries.shift.to_s.capitalize}")
      else
        node_class = Nodes::With
      end
      @ast.with = node_class.new(subqueries.flatten)

      self
    end

    def take(limit)
      if limit
        @ast.limit = Nodes::Limit.new(limit)
      else
        @ast.limit = nil
      end
      self
    end
    alias limit= take

    def join_sources
      @ctx.source.right
    end

    def source
      @ctx.source
    end

    def comment(*values)
      if values.any?
        @ctx.comment = Nodes::Comment.new(values)
        self
      else
        @ctx.comment
      end
    end

    private
      def collapse(exprs)
        exprs = exprs.compact
        exprs.map! { |expr|
          if String === expr
            # FIXME: Don't do this automatically
            Arel.sql(expr)
          else
            expr
          end
        }

        if exprs.length == 1
          exprs.first
        else
          create_and exprs
        end
      end
  end
end

# ---- arel/update_manager.rb ----
module Arel # :nodoc: all
  class UpdateManager < Arel::TreeManager
    include TreeManager::StatementMethods

    def initialize(table = nil)
      super

      @ast = Nodes::UpdateStatement.new(table)
    end

    ###
    # UPDATE +table+
    def table(table)
      @ast.relation = table
      self
    end

    def set(values)
      case values
      when String, Nodes::BoundSqlLiteral
        @ast.values = [values]
      else
        @ast.values = values.map { |column, value|
          Nodes::Assignment.new(
            Nodes::UnqualifiedColumn.new(column),
            value
          )
        }
      end
      self
    end

    def group(columns)
      columns.each do |column|
        column = Nodes::SqlLiteral.new(column) if String === column
        column = Nodes::SqlLiteral.new(column.to_s) if Symbol === column

        @ast.groups.push Nodes::Group.new column
      end

      self
    end

    def having(expr)
      @ast.havings << expr
      self
    end

    def comment(value)
      @ast.comment = value
      self
    end

    def returning(returning)
      @ast.returning << returning
      self
    end
  end
end

# ---- arel/delete_manager.rb ----
module Arel # :nodoc: all
  class DeleteManager < Arel::TreeManager
    include TreeManager::StatementMethods

    def initialize(table = nil)
      super

      @ast = Nodes::DeleteStatement.new(table)
    end

    def from(relation)
      @ast.relation = relation
      self
    end

    def group(columns)
      columns.each do |column|
        column = Nodes::SqlLiteral.new(column) if String === column
        column = Nodes::SqlLiteral.new(column.to_s) if Symbol === column

        @ast.groups.push Nodes::Group.new column
      end

      self
    end

    def having(expr)
      @ast.havings << expr
      self
    end

    def comment(value)
      @ast.comment = value
      self
    end

    def returning(returning)
      @ast.returning << returning
      self
    end
  end
end

# ---- arel/nodes/node.rb ----
module Arel # :nodoc: all
  module Nodes
    # = Using +Arel::Nodes::Node+
    #
    # Active Record uses Arel to compose SQL statements. Instead of building SQL strings directly, it's building an
    # abstract syntax tree (AST) of the statement using various types of Arel::Nodes::Node. Each node represents a
    # fragment of an SQL statement.
    #
    # The intermediate representation allows Arel to compile the statement into the database's specific SQL dialect
    # only before sending it without having to care about the nuances of each database when building the statement.
    # It also allows easier composition of statements without having to resort to (brittle and unsafe) string manipulation.
    #
    # == Building constraints
    #
    # One of the most common use cases of Arel is generating constraints for +SELECT+ statements. To help with that,
    # most nodes include a couple of useful factory methods to create subtree structures for common constraints. For
    # a full list of those, please refer to Arel::Predications.
    #
    # The following example creates an equality constraint where the value of the name column on the users table
    # matches the value DHH.
    #
    #   users = Arel::Table.new(name: :users)
    #   constraint = users[:name].eq("DHH")
    #
    #   # => Arel::Nodes::Equality.new(
    #   #      Arel::Attributes::Attribute.new(users, "name"),
    #   #      Arel::Nodes::Casted.new(
    #   #        "DHH",
    #   #        Arel::Attributes::Attribute.new(users, "name")
    #   #      )
    #   #    )
    #
    # The resulting SQL fragment will look like this:
    #
    #   "users"."name" = 'DHH'
    #
    # The constraint fragments can be used with regular ActiveRecord::Relation objects instead of a Hash. The
    # following two examples show two ways of creating the same query.
    #
    #   User.where(name: 'DHH')
    #
    #   # SELECT "users".* FROM "users" WHERE "users"."name" = 'DHH'
    #
    #   users = User.arel_table
    #
    #   User.where(users[:name].eq('DHH'))
    #
    #   # SELECT "users".* FROM "users" WHERE "users"."name" = 'DHH'
    #
    # == Functions
    #
    # Arel comes with built-in support for SQL functions like +COUNT+, +SUM+, +MIN+, +MAX+, and +AVG+. The
    # Arel::Expressions module includes factory methods for the default functions.
    #
    #   employees = Employee.arel_table
    #
    #   Employee.select(employees[:department_id], employees[:salary].average).group(employees[:department_id])
    #
    #   # SELECT "employees"."department_id", AVG("employees"."salary")
    #   #   FROM "employees" GROUP BY "employees"."department_id"
    #
    # It’s also possible to use custom functions by using the Arel::Nodes::NamedFunction node type. It accepts a
    # function name and an array of parameters.
    #
    #   Arel::Nodes::NamedFunction.new('date_trunc', [Arel::Nodes.build_quoted('day'), User.arel_table[:created_at]])
    #
    #   # date_trunc('day', "users"."created_at")
    #
    # == Quoting & bind params
    #
    # Values that you pass to Arel nodes need to be quoted or wrapped in bind params. This ensures they are properly
    # converted into the correct format without introducing a possible SQL injection vulnerability. Most factory
    # methods (like +eq+, +gt+, +lteq+, …) quote passed values automatically. When not using a factory method, it’s
    # possible to convert a value and wrap it in an Arel::Nodes::Quoted node (if necessary) by calling +Arel::Nodes.
    # build_quoted+.
    #
    #   Arel::Nodes.build_quoted("foo") # 'foo'
    #   Arel::Nodes.build_quoted(12.3)  # 12.3
    #
    # Instead of quoting values and embedding them directly in the SQL statement, it’s also possible to create bind
    # params. This keeps the actual values outside of the statement and allows using the prepared statement feature
    # of some databases.
    #
    #   attribute = ActiveRecord::Relation::QueryAttribute.new(:name, "DHH", ActiveRecord::Type::String.new)
    #   Arel::Nodes::BindParam.new(attribute)
    #
    # When ActiveRecord runs the query, bind params are replaced by placeholders (like +$1+) and the values are passed
    # separately.
    #
    # == SQL Literals
    #
    # For cases where there is no way to represent a particular SQL fragment using Arel nodes, you can use an SQL
    # literal. SQL literals are strings that Arel will treat “as is”.
    #
    #   Arel.sql('LOWER("users"."name")').eq('dhh')
    #
    #   # LOWER("users"."name") = 'dhh'
    #
    # Please keep in mind that passing data as raw SQL literals might introduce a possible SQL injection. However,
    # `Arel.sql` supports binding parameters which will ensure proper quoting. This can be useful when you need to
    # control the exact SQL you run, but you still have potentially user-supplied values.
    #
    #   Arel.sql('LOWER("users"."name") = ?', 'dhh')
    #
    #   # LOWER("users"."name") = 'dhh'
    #
    # You can also combine SQL literals.
    #
    #   sql = Arel.sql('SELECT * FROM "users" WHERE ')
    #   sql += Arel.sql('LOWER("users"."name") = :name', name: 'dhh')
    #   sql += Arel.sql('AND "users"."age" > :age', age: 35)
    #
    #   # SELECT * FROM "users" WHERE LOWER("users"."name") = 'dhh' AND "users"."age" > '35'
    class Node
      include Arel::FactoryMethods

      ###
      # Factory method to create a Nodes::Not node that has the recipient of
      # the caller as a child.
      def not
        Nodes::Not.new self
      end

      ###
      # Factory method to create a Nodes::Grouping node that has an Nodes::Or
      # node as a child.
      def or(right)
        Nodes::Grouping.new Nodes::Or.new([self, right])
      end

      ###
      # Factory method to create an Nodes::And node.
      def and(right)
        Nodes::And.new [self, right]
      end

      def invert
        Arel::Nodes::Not.new(self)
      end

      # FIXME: this method should go away.  I don't like people calling
      # to_sql on non-head nodes.  This forces us to walk the AST until we
      # can find a node that has a "relation" member.
      #
      # Maybe we should just use `Table.engine`?  :'(
      def to_sql(engine = Table.engine)
        collector = Arel::Collectors::SQLString.new
        engine.with_connection do |connection|
          connection.visitor.accept(self, collector).value
        end
      end

      def fetch_attribute(&)
      end

      def equality?; false; end
    end
  end
end

# ---- arel/nodes/node_expression.rb ----
module Arel # :nodoc: all
  module Nodes
    class NodeExpression < Arel::Nodes::Node
      include Arel::Expressions
      include Arel::Predications
      include Arel::AliasPredication
      include Arel::OrderPredications
      include Arel::Math
    end
  end
end

# ---- arel/nodes/select_statement.rb ----
module Arel # :nodoc: all
  module Nodes
    class SelectStatement < Arel::Nodes::NodeExpression
      attr_reader :cores
      attr_accessor :limit, :orders, :lock, :offset, :with

      def initialize(relation = nil)
        super()
        @cores          = [SelectCore.new(relation)]
        @orders         = []
        @limit          = nil
        @lock           = nil
        @offset         = nil
        @with           = nil
      end

      def initialize_copy(other)
        super
        @cores  = @cores.map { |x| x.clone }
        @orders = @orders.map { |x| x.clone }
      end

      def hash
        [@cores, @orders, @limit, @lock, @offset, @with].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.cores == other.cores &&
          self.orders == other.orders &&
          self.limit == other.limit &&
          self.lock == other.lock &&
          self.offset == other.offset &&
          self.with == other.with
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/select_core.rb ----
module Arel # :nodoc: all
  module Nodes
    class SelectCore < Arel::Nodes::Node
      attr_accessor :projections, :wheres, :groups, :windows, :comment
      attr_accessor :havings, :source, :set_quantifier, :optimizer_hints

      def initialize(relation = nil)
        super()
        @source = JoinSource.new(relation)

        # https://ronsavage.github.io/SQL/sql-92.bnf.html#set%20quantifier
        @set_quantifier  = nil
        @optimizer_hints = nil
        @projections     = []
        @wheres          = []
        @groups          = []
        @havings         = []
        @windows         = []
        @comment         = nil
      end

      def from
        @source.left
      end

      def from=(value)
        @source.left = value
      end

      alias :froms= :from=
      alias :froms :from

      def initialize_copy(other)
        super
        @source      = @source.clone if @source
        @projections = @projections.clone
        @wheres      = @wheres.clone
        @groups      = @groups.clone
        @havings     = @havings.clone
        @windows     = @windows.clone
      end

      def hash
        [
          @source, @set_quantifier, @projections, @optimizer_hints,
          @wheres, @groups, @havings, @windows, @comment
        ].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.source == other.source &&
          self.set_quantifier == other.set_quantifier &&
          self.optimizer_hints == other.optimizer_hints &&
          self.projections == other.projections &&
          self.wheres == other.wheres &&
          self.groups == other.groups &&
          self.havings == other.havings &&
          self.windows == other.windows &&
          self.comment == other.comment
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/insert_statement.rb ----
module Arel # :nodoc: all
  module Nodes
    class InsertStatement < Arel::Nodes::Node
      attr_accessor :relation, :columns, :values, :select, :returning

      def initialize(relation = nil)
        super()
        @relation  = relation
        @columns   = []
        @values    = nil
        @select    = nil
        @returning = []
      end

      def initialize_copy(other)
        super
        @columns = @columns.clone
        @values =  @values.clone if @values
        @select =  @select.clone if @select
        @returning = @returning.clone if @returning
      end

      def hash
        [@relation, @columns, @values, @select, @returning].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.relation == other.relation &&
          self.columns == other.columns &&
          self.select == other.select &&
          self.values == other.values &&
          self.returning == other.returning
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/update_statement.rb ----
module Arel # :nodoc: all
  module Nodes
    class UpdateStatement < Arel::Nodes::Node
      attr_accessor :relation, :wheres, :values, :groups, :havings, :orders, :limit, :offset, :comment, :key, :returning

      def initialize(relation = nil)
        super()
        @relation  = relation
        @wheres    = []
        @values    = []
        @groups    = []
        @havings   = []
        @orders    = []
        @limit     = nil
        @offset    = nil
        @comment   = nil
        @key       = nil
        @returning = []
      end

      def initialize_copy(other)
        super
        @wheres = @wheres.clone
        @values = @values.clone
        @returning = @returning.clone if @returning
      end

      def hash
        [@relation, @wheres, @values, @orders, @limit, @offset, @comment, @key, @returning].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.relation == other.relation &&
          self.wheres == other.wheres &&
          self.values == other.values &&
          self.groups == other.groups &&
          self.havings == other.havings &&
          self.orders == other.orders &&
          self.limit == other.limit &&
          self.offset == other.offset &&
          self.comment == other.comment &&
          self.key == other.key &&
          self.returning == other.returning
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/bind_param.rb ----
module Arel # :nodoc: all
  module Nodes
    class BindParam < Node
      attr_reader :value

      def initialize(value)
        @value = value
        super()
      end

      def hash
        [self.class, self.value].hash
      end

      def eql?(other)
        other.is_a?(BindParam) &&
          value == other.value
      end
      alias :== :eql?

      def nil?
        value.nil?
      end

      def value_before_type_cast
        if value.respond_to?(:value_before_type_cast)
          value.value_before_type_cast
        else
          value
        end
      end

      def infinite?
        value.respond_to?(:infinite?) && value.infinite?
      end

      def unboundable?
        value.respond_to?(:unboundable?) && value.unboundable?
      end
    end
  end
end

# ---- arel/nodes/fragments.rb ----
module Arel # :nodoc: all
  module Nodes
    class Fragments < Arel::Nodes::Node
      attr_reader :values

      def initialize(values = [])
        super()
        @values = values
      end

      def initialize_copy(other)
        super
        @values = @values.clone
      end

      def hash
        [@values].hash
      end

      def +(other)
        raise ArgumentError, "Expected Arel node" unless Arel.arel_node?(other)

        self.class.new([*@values, other])
      end

      def eql?(other)
        self.class == other.class &&
          self.values == other.values
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/terminal.rb ----
module Arel # :nodoc: all
  module Nodes
    class Distinct < Arel::Nodes::NodeExpression
      def hash
        self.class.hash
      end

      def eql?(other)
        self.class == other.class
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/true.rb ----
module Arel # :nodoc: all
  module Nodes
    class True < Arel::Nodes::NodeExpression
      def hash
        self.class.hash
      end

      def eql?(other)
        self.class == other.class
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/false.rb ----
module Arel # :nodoc: all
  module Nodes
    class False < Arel::Nodes::NodeExpression
      def hash
        self.class.hash
      end

      def eql?(other)
        self.class == other.class
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/unary.rb ----
module Arel # :nodoc: all
  module Nodes
    class Unary < Arel::Nodes::NodeExpression
      attr_accessor :expr
      alias :value :expr

      def initialize(expr)
        super()
        @expr = expr
      end

      def hash
        @expr.hash
      end

      def eql?(other)
        self.class == other.class &&
          self.expr == other.expr
      end
      alias :== :eql?
    end

    class Bin < Unary; end
    class Cube < Unary; end
    class DistinctOn < Unary; end
    class Group < Unary; end
    class GroupingElement < Unary; end
    class GroupingSet < Unary; end
    class Lateral < Unary; end
    class Limit < Unary; end
    class Lock < Unary; end
    class Not < Unary; end
    class Offset < Unary; end
    class On < Unary; end
    class OptimizerHints < Unary; end
    class RollUp < Unary; end
  end
end

# ---- arel/nodes/grouping.rb ----
module Arel # :nodoc: all
  module Nodes
    class Grouping < Unary
      def fetch_attribute(&block)
        expr.fetch_attribute(&block)
      end
    end
  end
end

# ---- arel/nodes/homogeneous_in.rb ----
module Arel # :nodoc: all
  module Nodes
    class HomogeneousIn < Node
      attr_reader :attribute, :values, :type

      def initialize(values, attribute, type)
        @values = values
        @attribute = attribute
        @type = type
      end

      def hash
        ivars.hash
      end

      def eql?(other)
        super || (self.class == other.class && self.ivars == other.ivars)
      end
      alias :== :eql?

      def equality?
        type == :in
      end

      def invert
        Arel::Nodes::HomogeneousIn.new(values, attribute, type == :in ? :notin : :in)
      end

      def left
        attribute
      end

      def right
        attribute.quoted_array(values)
      end

      def casted_values
        type = attribute.type_caster

        casted_values = values.map do |raw_value|
          type.serialize(raw_value) if type.serializable?(raw_value)
        end

        casted_values.compact!
        casted_values
      end

      def proc_for_binds
        -> value { ActiveModel::Attribute.with_cast_value(attribute.name, value, ActiveModel::Type.default_value) }
      end

      def fetch_attribute(&block)
        attribute.fetch_attribute(&block)
      end

      protected
        def ivars
          [@attribute, @values, @type]
        end
    end
  end
end

# ---- arel/nodes/ordering.rb ----
module Arel # :nodoc: all
  module Nodes
    class Ordering < Unary
      def nulls_first
        NullsFirst.new(self)
      end

      def nulls_last
        NullsLast.new(self)
      end
    end

    class NullsFirst < Ordering
      def reverse
        NullsLast.new(expr.reverse)
      end
    end

    class NullsLast < Ordering
      def reverse
        NullsFirst.new(expr.reverse)
      end
    end
  end
end

# ---- arel/nodes/ascending.rb ----
module Arel # :nodoc: all
  module Nodes
    class Ascending < Ordering
      def reverse
        Descending.new(expr)
      end

      def direction
        :asc
      end

      def ascending?
        true
      end

      def descending?
        false
      end
    end
  end
end

# ---- arel/nodes/descending.rb ----
module Arel # :nodoc: all
  module Nodes
    class Descending < Ordering
      def reverse
        Ascending.new(expr)
      end

      def direction
        :desc
      end

      def ascending?
        false
      end

      def descending?
        true
      end
    end
  end
end

# ---- arel/nodes/unqualified_column.rb ----
module Arel # :nodoc: all
  module Nodes
    class UnqualifiedColumn < Arel::Nodes::Unary
      alias :attribute :expr
      alias :attribute= :expr=

      def relation
        @expr.relation
      end

      def column
        @expr.column
      end

      def name
        @expr.name
      end
    end
  end
end

# ---- arel/nodes/with.rb ----
module Arel # :nodoc: all
  module Nodes
    class With < Arel::Nodes::Unary
      alias children expr
    end

    class WithRecursive < With; end
  end
end

# ---- arel/nodes/binary.rb ----
module Arel # :nodoc: all
  module Nodes
    class Binary < Arel::Nodes::NodeExpression
      attr_accessor :left, :right

      def initialize(left, right)
        super()
        @left  = left
        @right = right
      end

      def initialize_copy(other)
        super
        @left  = @left.clone if @left
        @right = @right.clone if @right
      end

      def hash
        [self.class, @left, @right].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.left == other.left &&
          self.right == other.right
      end
      alias :== :eql?
    end

    module FetchAttribute
      def fetch_attribute(&)
        if left.is_a?(Arel::Attributes::Attribute)
          left.fetch_attribute(&)
        elsif right.is_a?(Arel::Attributes::Attribute)
          right.fetch_attribute(&)
        end
      end
    end

    class As < Binary
      def to_cte
        Arel::Nodes::Cte.new(left.name, right)
      end
    end

    class Between < Binary; include FetchAttribute; end

    class GreaterThan < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::LessThanOrEqual.new(left, right)
      end
    end

    class GreaterThanOrEqual < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::LessThan.new(left, right)
      end
    end

    class LessThan < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::GreaterThanOrEqual.new(left, right)
      end
    end

    class LessThanOrEqual < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::GreaterThan.new(left, right)
      end
    end

    class IsDistinctFrom < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::IsNotDistinctFrom.new(left, right)
      end
    end

    class IsNotDistinctFrom < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::IsDistinctFrom.new(left, right)
      end
    end

    class NotEqual < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::Equality.new(left, right)
      end
    end

    class NotIn < Binary
      include FetchAttribute

      def invert
        Arel::Nodes::In.new(left, right)
      end
    end

    class Assignment < Binary; end
    class Join < Binary; end
    class Union < Binary; end
    class UnionAll < Binary; end
    class Intersect < Binary; end
    class Except < Binary; end
  end
end

# ---- arel/nodes/equality.rb ----
module Arel # :nodoc: all
  module Nodes
    class Equality < Arel::Nodes::Binary
      include FetchAttribute

      def equality?; true; end

      def invert
        Arel::Nodes::NotEqual.new(left, right)
      end
    end

    class CaseSensitiveEquality < Arel::Nodes::Binary
      include FetchAttribute

      def equality?; true; end
    end

    class CaseInsensitiveEquality < Arel::Nodes::Binary
      include FetchAttribute

      def equality?; true; end
    end
  end
end

# ---- arel/nodes/filter.rb ----
module Arel # :nodoc: all
  module Nodes
    class Filter < Binary
      include Arel::WindowPredications
      include Arel::AliasPredication
    end
  end
end

# ---- arel/nodes/in.rb ----
module Arel # :nodoc: all
  module Nodes
    class In < Arel::Nodes::Binary
      include FetchAttribute

      def equality?; true; end

      def invert
        Arel::Nodes::NotIn.new(left, right)
      end
    end
  end
end

# ---- arel/nodes/join_source.rb ----
module Arel # :nodoc: all
  module Nodes
    ###
    # Class that represents a join source
    #
    #   https://www.sqlite.org/syntaxdiagrams.html#join-source

    class JoinSource < Arel::Nodes::Binary
      def initialize(single_source, joinop = [])
        super
      end

      def empty?
        !left && right.empty?
      end
    end
  end
end

# ---- arel/nodes/delete_statement.rb ----
module Arel # :nodoc: all
  module Nodes
    class DeleteStatement < Arel::Nodes::Node
      attr_accessor :relation, :wheres, :groups, :havings, :orders, :limit, :offset, :comment, :key, :returning

      def initialize(relation = nil, wheres = [])
        super()
        @relation = relation
        @wheres = wheres
        @groups = []
        @havings = []
        @orders = []
        @limit = nil
        @offset = nil
        @comment = nil
        @key = nil
        @returning = []
      end

      def initialize_copy(other)
        super
        @relation = @relation.clone if @relation
        @wheres = @wheres.clone if @wheres
        @returning = @returning.clone if @returning
      end

      def hash
        [self.class, @relation, @wheres, @orders, @limit, @offset, @comment, @key, @returning].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.relation == other.relation &&
          self.wheres == other.wheres &&
          self.orders == other.orders &&
          self.groups == other.groups &&
          self.havings == other.havings &&
          self.limit == other.limit &&
          self.offset == other.offset &&
          self.comment == other.comment &&
          self.key == other.key &&
          self.returning == other.returning
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/table_alias.rb ----
module Arel # :nodoc: all
  module Nodes
    class TableAlias < Arel::Nodes::Binary
      alias :name :right
      alias :relation :left
      alias :table_alias :name

      def [](name)
        relation.is_a?(Table) ? relation[name, self] : Attribute.new(self, name)
      end

      def table_name
        relation.respond_to?(:name) ? relation.name : name
      end

      def type_cast_for_database(attr_name, value)
        relation.type_cast_for_database(attr_name, value)
      end

      def type_for_attribute(name)
        relation.type_for_attribute(name)
      end

      def able_to_type_cast?
        relation.respond_to?(:able_to_type_cast?) && relation.able_to_type_cast?
      end

      def to_cte
        Arel::Nodes::Cte.new(name, relation)
      end
    end
  end
end

# ---- arel/nodes/infix_operation.rb ----
module Arel # :nodoc: all
  module Nodes
    class InfixOperation < Binary
      include Arel::Expressions
      include Arel::Predications
      include Arel::OrderPredications
      include Arel::AliasPredication
      include Arel::Math

      attr_reader :operator

      def initialize(operator, left, right)
        super(left, right)
        @operator = operator
      end
    end

    class Multiplication < InfixOperation
      def initialize(left, right)
        super(:*, left, right)
      end
    end

    class Division < InfixOperation
      def initialize(left, right)
        super(:/, left, right)
      end
    end

    class Addition < InfixOperation
      def initialize(left, right)
        super(:+, left, right)
      end
    end

    class Subtraction < InfixOperation
      def initialize(left, right)
        super(:-, left, right)
      end
    end

    class Concat < InfixOperation
      def initialize(left, right)
        super(:"||", left, right)
      end
    end

    class Contains < InfixOperation
      def initialize(left, right)
        super(:"@>", left, right)
      end
    end

    class Overlaps < InfixOperation
      def initialize(left, right)
        super(:"&&", left, right)
      end
    end

    class BitwiseAnd < InfixOperation
      def initialize(left, right)
        super(:&, left, right)
      end
    end

    class BitwiseOr < InfixOperation
      def initialize(left, right)
        super(:|, left, right)
      end
    end

    class BitwiseXor < InfixOperation
      def initialize(left, right)
        super(:^, left, right)
      end
    end

    class BitwiseShiftLeft < InfixOperation
      def initialize(left, right)
        super(:<<, left, right)
      end
    end

    class BitwiseShiftRight < InfixOperation
      def initialize(left, right)
        super(:>>, left, right)
      end
    end
  end
end

# ---- arel/nodes/unary_operation.rb ----
module Arel # :nodoc: all
  module Nodes
    class UnaryOperation < Unary
      attr_reader :operator

      def initialize(operator, operand)
        super(operand)
        @operator = operator
      end
    end

    class BitwiseNot < UnaryOperation
      def initialize(operand)
        super(:~, operand)
      end
    end
  end
end

# ---- arel/nodes/over.rb ----
module Arel # :nodoc: all
  module Nodes
    class Over < Binary
      include Arel::AliasPredication

      def initialize(left, right = nil)
        super(left, right)
      end

      def operator; "OVER" end
    end
  end
end

# ---- arel/nodes/matches.rb ----
module Arel # :nodoc: all
  module Nodes
    class Matches < Binary
      attr_reader :escape
      attr_accessor :case_sensitive

      def initialize(left, right, escape = nil, case_sensitive = false)
        super(left, right)
        @escape = escape && Nodes.build_quoted(escape)
        @case_sensitive = case_sensitive
      end
    end

    class DoesNotMatch < Matches; end
  end
end

# ---- arel/nodes/regexp.rb ----
module Arel # :nodoc: all
  module Nodes
    class Regexp < Binary
      attr_accessor :case_sensitive

      def initialize(left, right, case_sensitive = true)
        super(left, right)
        @case_sensitive = case_sensitive
      end
    end

    class NotRegexp < Regexp; end
  end
end

# ---- arel/nodes/cte.rb ----
module Arel # :nodoc: all
  module Nodes
    class Cte < Arel::Nodes::Binary
      alias :name :left
      alias :relation :right
      attr_reader :materialized

      def initialize(name, relation, materialized: nil)
        super(name, relation)
        @materialized = materialized
      end

      def hash
        [name, relation, materialized].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.name == other.name &&
          self.relation == other.relation &&
          self.materialized == other.materialized
      end
      alias :== :eql?

      def to_cte
        self
      end

      def to_table
        Arel::Table.new(name: name)
      end
    end
  end
end

# ---- arel/nodes/nary.rb ----
module Arel # :nodoc: all
  module Nodes
    class Nary < Arel::Nodes::NodeExpression
      attr_reader :children

      def initialize(children)
        super()
        @children = children
      end

      def left
        children.first
      end

      def right
        children[1]
      end

      def fetch_attribute(&block)
        children.any? && children.all? { |child| child.fetch_attribute(&block) }
      end

      def hash
        [self.class, children].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.children == other.children
      end
      alias :== :eql?
    end

    class And < Nary; end
    class Or < Nary; end
  end
end

# ---- arel/nodes/function.rb ----
module Arel # :nodoc: all
  module Nodes
    class Function < Arel::Nodes::NodeExpression
      include Arel::WindowPredications
      include Arel::FilterPredications

      attr_accessor :expressions, :distinct

      def initialize(expr)
        super()
        @expressions = expr
        @distinct    = false
      end

      def hash
        [@expressions, @distinct].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.expressions == other.expressions &&
          self.distinct == other.distinct
      end
      alias :== :eql?
    end

    class Sum < Function; end
    class Exists < Function; end
    class Max < Function; end
    class Min < Function; end
    class Avg < Function; end
  end
end

# ---- arel/nodes/count.rb ----
module Arel # :nodoc: all
  module Nodes
    class Count < Arel::Nodes::Function
      def initialize(expr, distinct = false)
        super(expr)
        @distinct = distinct
      end
    end
  end
end

# ---- arel/nodes/extract.rb ----
module Arel # :nodoc: all
  module Nodes
    class Extract < Arel::Nodes::Unary
      attr_accessor :field

      def initialize(expr, field)
        super(expr)
        @field = field
      end

      def hash
        super ^ @field.hash
      end

      def eql?(other)
        super &&
          self.field == other.field
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/values_list.rb ----
module Arel # :nodoc: all
  module Nodes
    class ValuesList < Unary
      alias :rows :expr
    end
  end
end

# ---- arel/nodes/named_function.rb ----
module Arel # :nodoc: all
  module Nodes
    class NamedFunction < Arel::Nodes::Function
      attr_accessor :name

      def initialize(name, expr)
        super(expr)
        @name = name
      end

      def hash
        super ^ @name.hash
      end

      def eql?(other)
        super && self.name == other.name
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/window.rb ----
module Arel # :nodoc: all
  module Nodes
    class Window < Arel::Nodes::Node
      attr_accessor :orders, :framing, :partitions

      def initialize
        @orders = []
        @partitions = []
        @framing = nil
      end

      def order(*expr)
        # FIXME: We SHOULD NOT be converting these to SqlLiteral automatically
        @orders.concat expr.map { |x|
          String === x || Symbol === x ? Nodes::SqlLiteral.new(x.to_s) : x
        }
        self
      end

      def partition(*expr)
        # FIXME: We SHOULD NOT be converting these to SqlLiteral automatically
        @partitions.concat expr.map { |x|
          String === x || Symbol === x ? Nodes::SqlLiteral.new(x.to_s) : x
        }
        self
      end

      def frame(expr)
        @framing = expr
      end

      def rows(expr = nil)
        if @framing
          Rows.new(expr)
        else
          frame(Rows.new(expr))
        end
      end

      def range(expr = nil)
        if @framing
          Range.new(expr)
        else
          frame(Range.new(expr))
        end
      end

      def initialize_copy(other)
        super
        @orders = @orders.map { |x| x.clone }
      end

      def hash
        [@orders, @framing].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.orders == other.orders &&
          self.framing == other.framing &&
          self.partitions == other.partitions
      end
      alias :== :eql?
    end

    class NamedWindow < Window
      attr_accessor :name

      def initialize(name)
        super()
        @name = name
      end

      def initialize_copy(other)
        super
        @name = other.name.clone
      end

      def hash
        super ^ @name.hash
      end

      def eql?(other)
        super && self.name == other.name
      end
      alias :== :eql?
    end

    class Rows < Unary
      def initialize(expr = nil)
        super(expr)
      end
    end

    class Range < Unary
      def initialize(expr = nil)
        super(expr)
      end
    end

    class CurrentRow < Node
      def hash
        self.class.hash
      end

      def eql?(other)
        self.class == other.class
      end
      alias :== :eql?
    end

    class Preceding < Unary
      def initialize(expr = nil)
        super(expr)
      end
    end

    class Following < Unary
      def initialize(expr = nil)
        super(expr)
      end
    end
  end
end

# ---- arel/nodes/case.rb ----
module Arel # :nodoc: all
  module Nodes
    class Case < Arel::Nodes::NodeExpression
      attr_accessor :case, :conditions, :default

      def initialize(expression = nil, default = nil)
        @case = expression
        @conditions = []
        @default = default
      end

      def when(condition, expression = nil)
        @conditions << When.new(Nodes.build_quoted(condition), expression)
        self
      end

      def then(expression)
        @conditions.last.right = Nodes.build_quoted(expression)
        self
      end

      def else(expression)
        @default = Else.new Nodes.build_quoted(expression)
        self
      end

      def initialize_copy(other)
        super
        @case = @case.clone if @case
        @conditions = @conditions.map { |x| x.clone }
        @default = @default.clone if @default
      end

      def hash
        [@case, @conditions, @default].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.case == other.case &&
          self.conditions == other.conditions &&
          self.default == other.default
      end
      alias :== :eql?
    end

    class When < Binary # :nodoc:
    end

    class Else < Unary # :nodoc:
    end
  end
end

# ---- arel/nodes/full_outer_join.rb ----
module Arel # :nodoc: all
  module Nodes
    class FullOuterJoin < Arel::Nodes::Join
    end
  end
end

# ---- arel/nodes/inner_join.rb ----
module Arel # :nodoc: all
  module Nodes
    class InnerJoin < Arel::Nodes::Join
    end
  end
end

# ---- arel/nodes/outer_join.rb ----
module Arel # :nodoc: all
  module Nodes
    class OuterJoin < Arel::Nodes::Join
    end
  end
end

# ---- arel/nodes/right_outer_join.rb ----
module Arel # :nodoc: all
  module Nodes
    class RightOuterJoin < Arel::Nodes::Join
    end
  end
end

# ---- arel/nodes/string_join.rb ----
module Arel # :nodoc: all
  module Nodes
    class StringJoin < Arel::Nodes::Join
      def initialize(left, right = nil)
        super
      end
    end
  end
end

# ---- arel/nodes/leading_join.rb ----
module Arel # :nodoc: all
  module Nodes
    class LeadingJoin < Arel::Nodes::InnerJoin
    end
  end
end

# ---- arel/nodes/comment.rb ----
module Arel # :nodoc: all
  module Nodes
    class Comment < Arel::Nodes::Node
      attr_reader :values

      def initialize(values)
        super()
        @values = values
      end

      def initialize_copy(other)
        super
        @values = @values.clone
      end

      def hash
        [@values].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.values == other.values
      end
      alias :== :eql?
    end
  end
end

# ---- arel/nodes/sql_literal.rb ----
module Arel # :nodoc: all
  module Nodes
    class SqlLiteral < String
      include Arel::Expressions
      include Arel::Predications
      include Arel::AliasPredication
      include Arel::OrderPredications

      attr_reader :retryable

      def initialize(string, retryable: false)
        @retryable = retryable
        super(string)
      end

      def encode_with(coder)
        coder.scalar = self.to_s
      end

      def fetch_attribute(&)
      end

      def +(other)
        raise ArgumentError, "Expected Arel node" unless Arel.arel_node?(other)

        Fragments.new([self, other])
      end
    end
  end
end

# ---- arel/nodes/bound_sql_literal.rb ----
module Arel # :nodoc: all
  module Nodes
    class BoundSqlLiteral < NodeExpression
      attr_reader :sql_with_placeholders, :positional_binds, :named_binds

      def initialize(sql_with_placeholders, positional_binds, named_binds)
        has_positional = !(positional_binds.nil? || positional_binds.empty?)
        has_named = !(named_binds.nil? || named_binds.empty?)

        if has_positional
          if has_named
            raise BindError.new("cannot mix positional and named binds", sql_with_placeholders)
          end
          if positional_binds.size != (expected = sql_with_placeholders.count("?"))
            raise BindError.new("wrong number of bind variables (#{positional_binds.size} for #{expected})", sql_with_placeholders)
          end
        elsif has_named
          tokens_in_string = sql_with_placeholders.scan(/:(?<!::)([a-zA-Z]\w*)/).flatten.map(&:to_sym).uniq
          tokens_in_hash = named_binds.keys.map(&:to_sym).uniq

          if !(missing = (tokens_in_string - tokens_in_hash)).empty?
            if missing.size == 1
              raise BindError.new("missing value for #{missing.first.inspect}", sql_with_placeholders)
            else
              raise BindError.new("missing values for #{missing.inspect}", sql_with_placeholders)
            end
          end
        end

        @sql_with_placeholders = sql_with_placeholders
        if has_positional
          @positional_binds = positional_binds
          @named_binds = nil
        else
          @positional_binds = nil
          @named_binds = named_binds
        end
      end

      def hash
        [self.class, sql_with_placeholders, positional_binds, named_binds].hash
      end

      def eql?(other)
        self.class == other.class &&
          sql_with_placeholders == other.sql_with_placeholders &&
          positional_binds == other.positional_binds &&
          named_binds == other.named_binds
      end
      alias :== :eql?

      def +(other)
        raise ArgumentError, "Expected Arel node" unless Arel.arel_node?(other)

        Fragments.new([self, other])
      end

      def inspect
        "#<#{self.class.name} #{sql_with_placeholders.inspect} #{(named_binds || positional_binds).inspect}>"
      end
    end
  end
end

# ---- arel/nodes/casted.rb ----

module Arel # :nodoc: all
  module Nodes
    class Casted < Arel::Nodes::NodeExpression # :nodoc:
      attr_reader :value, :attribute
      alias :value_before_type_cast :value

      def initialize(value, attribute)
        @value     = value
        @attribute = attribute
        super()
      end

      def nil?; value.nil?; end

      def value_for_database
        if attribute.able_to_type_cast?
          attribute.type_cast_for_database(value)
        else
          value
        end
      end

      def hash
        [self.class, value, attribute].hash
      end

      def eql?(other)
        self.class == other.class &&
          self.value == other.value &&
          self.attribute == other.attribute
      end
      alias :== :eql?
    end

    class Quoted < Arel::Nodes::Unary # :nodoc:
      alias :value_for_database :value
      alias :value_before_type_cast :value

      def nil?; value.nil?; end

      def infinite?
        value.respond_to?(:infinite?) && value.infinite?
      end
    end

    def self.build_quoted(other, attribute = nil)
      case other
      when Arel::Nodes::Node, Arel::Attributes::Attribute, Arel::Table, Arel::SelectManager, Arel::Nodes::SqlLiteral, ActiveModel::Attribute
        other
      else
        case attribute
        when Arel::Attributes::Attribute
          Casted.new other, attribute
        else
          Quoted.new other
        end
      end
    end
  end
end

# ---- arel/visitors/visitor.rb ----
module Arel # :nodoc: all
  module Visitors
    class Visitor
      def initialize
        @dispatch = get_dispatch_cache
      end

      def accept(object, collector = nil)
        visit object, collector
      end

      private
        attr_reader :dispatch

        def self.dispatch_cache
          caches = ActiveSupport::Ractors.store_if_absent(:arel_visitor_dispatch_caches) do
            {}.compare_by_identity
          end
          caches[self] ||= build_dispatch_cache
        end

        def self.build_dispatch_cache
          Hash.new do |hash, klass|
            hash[klass] = :"visit_#{(klass.name || "").gsub("::", "_")}"
          end.compare_by_identity
        end

        def get_dispatch_cache
          self.class.dispatch_cache
        end

        def visit(object, collector = nil)
          dispatch_method = dispatch[object.class]
          if collector
            send dispatch_method, object, collector
          else
            send dispatch_method, object
          end
        rescue NoMethodError => e
          raise e if respond_to?(dispatch_method, true)
          superklass = object.class.ancestors.find { |klass|
            respond_to?(dispatch[klass], true)
          }
          raise(TypeError, "Cannot visit #{object.class}") unless superklass
          dispatch[object.class] = dispatch[superklass]
          retry
        end
    end
  end
end

# ---- arel/visitors/to_sql.rb ----
module Arel # :nodoc: all
  module Visitors
    class UnsupportedVisitError < StandardError
      def initialize(object)
        super "Unsupported argument type: #{object.class.name}. Construct an Arel node instead."
      end
    end

    class ToSql < Arel::Visitors::Visitor
      def initialize(connection)
        super()
        @connection = connection
      end

      def compile(node, collector = Arel::Collectors::SQLString.new)
        accept(node, collector).value
      end

      private
        def visit_Arel_Nodes_DeleteStatement(o, collector)
          collector.retryable = false
          o = prepare_delete_statement(o)

          if has_join_sources?(o)
            collector << "DELETE "
            visit o.relation.left, collector
            collector << " FROM "
          else
            collector << "DELETE FROM "
          end
          collector = visit o.relation, collector

          collect_nodes_for o.wheres, collector, " WHERE ", " AND "
          collect_nodes_for o.orders, collector, " ORDER BY "
          maybe_visit o.limit, collector
          maybe_visit o.comment, collector

          if o.returning.empty?
            collector
          else
            collector << " RETURNING "
            visit o.returning, collector
          end
        end

        def visit_Arel_Nodes_UpdateStatement(o, collector)
          collector.retryable = false
          o = prepare_update_statement(o)

          collector << "UPDATE "
          collector = visit o.relation, collector
          collect_nodes_for o.values, collector, " SET "

          collect_nodes_for o.wheres, collector, " WHERE ", " AND "
          collect_nodes_for o.orders, collector, " ORDER BY "
          maybe_visit o.limit, collector
          maybe_visit o.comment, collector

          if o.returning.empty?
            collector
          else
            collector << " RETURNING "
            visit o.returning, collector
          end
        end

        def visit_Arel_Nodes_InsertStatement(o, collector)
          collector.retryable = false
          collector << "INSERT INTO "
          collector = visit o.relation, collector

          unless o.columns.empty?
            collector << " ("
            o.columns.each_with_index do |x, i|
              collector << ", " unless i == 0
              collector << quote_column_name(x.name)
            end
            collector << ")"
          end

          if o.values
            maybe_visit o.values, collector
          elsif o.select
            maybe_visit o.select, collector
          end

          if o.returning.empty?
            collector
          else
            collector << " RETURNING "
            visit o.returning, collector
          end
        end

        def visit_Arel_Nodes_Exists(o, collector)
          collector << "EXISTS ("
          visit(o.expressions, collector) << ")"
        end

        def visit_Arel_Nodes_Casted(o, collector)
          collector << quote(o.value_for_database).to_s
        end
        alias :visit_Arel_Nodes_Quoted :visit_Arel_Nodes_Casted

        def visit_Arel_Nodes_True(o, collector)
          collector << "TRUE"
        end

        def visit_Arel_Nodes_False(o, collector)
          collector << "FALSE"
        end

        def visit_Arel_Nodes_ValuesList(o, collector)
          collector << "VALUES "

          o.rows.each_with_index do |row, i|
            collector << ", " unless i == 0
            collector << "("
            row.each_with_index do |value, k|
              collector << ", " unless k == 0
              case value
              when Nodes::SqlLiteral, Nodes::BindParam, ActiveModel::Attribute
                collector = visit(value, collector)
              else
                collector << quote(value).to_s
              end
            end
            collector << ")"
          end
          collector
        end

        def visit_Arel_Nodes_SelectStatement(o, collector)
          if o.with
            collector = visit o.with, collector
            collector << " "
          end

          collector = o.cores.inject(collector) { |c, x|
            visit_Arel_Nodes_SelectCore(x, c)
          }

          unless o.orders.empty?
            collector << " ORDER BY "
            o.orders.each_with_index do |x, i|
              collector << ", " unless i == 0
              collector = visit(x, collector)
            end
          end

          visit_Arel_Nodes_SelectOptions(o, collector)
        end

        # The Oracle enhanced adapter uses this private method,
        # see https://github.com/rsim/oracle-enhanced/issues/2186
        def visit_Arel_Nodes_SelectOptions(o, collector)
          collector = maybe_visit o.limit, collector
          collector = maybe_visit o.offset, collector
          maybe_visit o.lock, collector
        end

        def visit_Arel_Nodes_SelectCore(o, collector)
          collector << "SELECT"

          collector = collect_optimizer_hints(o, collector)
          collector = maybe_visit o.set_quantifier, collector

          collect_nodes_for o.projections, collector, " "

          if o.source && !o.source.empty?
            collector << " FROM "
            collector = visit o.source, collector
          end

          collect_nodes_for o.wheres, collector, " WHERE ", " AND "
          collect_nodes_for o.groups, collector, " GROUP BY "
          collect_nodes_for o.havings, collector, " HAVING ", " AND "
          collect_nodes_for o.windows, collector, " WINDOW "

          maybe_visit o.comment, collector
        end

        def visit_Arel_Nodes_OptimizerHints(o, collector)
          hints = o.expr.map { |v| sanitize_as_sql_comment(v) }.join(" ")
          collector << "/*+ #{hints} */"
        end

        def visit_Arel_Nodes_Comment(o, collector)
          collector << o.values.map { |v| "/* #{sanitize_as_sql_comment(v)} */" }.join(" ")
        end

        def collect_nodes_for(nodes, collector, spacer, connector = ", ")
          unless nodes.empty?
            collector << spacer
            inject_join nodes, collector, connector
          end
        end

        def visit_Arel_Nodes_Bin(o, collector)
          visit o.expr, collector
        end

        def visit_Arel_Nodes_Distinct(o, collector)
          collector << "DISTINCT"
        end

        def visit_Arel_Nodes_DistinctOn(o, collector)
          raise NotImplementedError, "DISTINCT ON not implemented for this db"
        end

        def visit_Arel_Nodes_With(o, collector)
          collector << "WITH "
          collect_ctes(o.children, collector)
        end

        def visit_Arel_Nodes_WithRecursive(o, collector)
          collector << "WITH RECURSIVE "
          collect_ctes(o.children, collector)
        end

        def visit_Arel_Nodes_Union(o, collector)
          infix_value_with_paren(o, collector, " UNION ")
        end

        def visit_Arel_Nodes_UnionAll(o, collector)
          infix_value_with_paren(o, collector, " UNION ALL ")
        end

        def visit_Arel_Nodes_Intersect(o, collector)
          collector << "( "
          infix_value(o, collector, " INTERSECT ") << " )"
        end

        def visit_Arel_Nodes_Except(o, collector)
          collector << "( "
          infix_value(o, collector, " EXCEPT ") << " )"
        end

        def visit_Arel_Nodes_NamedWindow(o, collector)
          collector << quote_column_name(o.name)
          collector << " AS "
          visit_Arel_Nodes_Window o, collector
        end

        def visit_Arel_Nodes_Window(o, collector)
          collector << "("

          collect_nodes_for o.partitions, collector, "PARTITION BY "

          if o.orders.any?
            collector << " " if o.partitions.any?
            collector << "ORDER BY "
            collector = inject_join o.orders, collector, ", "
          end

          if o.framing
            collector << " " if o.partitions.any? || o.orders.any?
            collector = visit o.framing, collector
          end

          collector << ")"
        end

        def visit_Arel_Nodes_Filter(o, collector)
          visit o.left, collector
          collector << " FILTER (WHERE "
          visit o.right, collector
          collector << ")"
        end

        def visit_Arel_Nodes_Rows(o, collector)
          if o.expr
            collector << "ROWS "
            visit o.expr, collector
          else
            collector << "ROWS"
          end
        end

        def visit_Arel_Nodes_Range(o, collector)
          if o.expr
            collector << "RANGE "
            visit o.expr, collector
          else
            collector << "RANGE"
          end
        end

        def visit_Arel_Nodes_Preceding(o, collector)
          collector = if o.expr
            visit o.expr, collector
          else
            collector << "UNBOUNDED"
          end

          collector << " PRECEDING"
        end

        def visit_Arel_Nodes_Following(o, collector)
          collector = if o.expr
            visit o.expr, collector
          else
            collector << "UNBOUNDED"
          end

          collector << " FOLLOWING"
        end

        def visit_Arel_Nodes_CurrentRow(o, collector)
          collector << "CURRENT ROW"
        end

        def visit_Arel_Nodes_Over(o, collector)
          case o.right
          when nil
            visit(o.left, collector) << " OVER ()"
          when Arel::Nodes::SqlLiteral
            infix_value o, collector, " OVER "
          when String, Symbol
            visit(o.left, collector) << " OVER #{quote_column_name o.right.to_s}"
          else
            infix_value o, collector, " OVER "
          end
        end

        def visit_Arel_Nodes_Offset(o, collector)
          collector << "OFFSET "
          visit o.expr, collector
        end

        def visit_Arel_Nodes_Limit(o, collector)
          collector << "LIMIT "
          visit o.expr, collector
        end

        def visit_Arel_Nodes_Lock(o, collector)
          visit o.expr, collector
        end

        def visit_Arel_Nodes_Grouping(o, collector)
          if o.expr.is_a? Nodes::Grouping
            visit(o.expr, collector)
          else
            collector << "("
            visit(o.expr, collector) << ")"
          end
        end

        def visit_Arel_Nodes_HomogeneousIn(o, collector)
          collector.preparable = false

          visit o.left, collector

          if o.type == :in
            collector << " IN ("
          else
            collector << " NOT IN ("
          end

          values = o.casted_values

          if values.empty?
            collector << @connection.quote(nil)
          elsif o.attribute.respond_to?(:comparison_expression)
            # Comparison values need SQL around each bind, so they cannot use
            # the collector's bulk bind path.
            binds = values.map(&o.proc_for_binds)
            expressions = binds.map { |bind| o.attribute.comparison_expression(bind) }
            collector = inject_join(expressions, collector, ", ")
          else
            collector.add_binds(values, o.proc_for_binds, &bind_block)
          end

          collector << ")"
        end

        def visit_Arel_SelectManager(o, collector)
          collector << "("
          visit(o.ast, collector) << ")"
        end

        def visit_Arel_Nodes_Ascending(o, collector)
          visit(o.expr, collector) << " ASC"
        end

        def visit_Arel_Nodes_Descending(o, collector)
          visit(o.expr, collector) << " DESC"
        end

        # NullsFirst is available on all but MySQL, where it is redefined.
        def visit_Arel_Nodes_NullsFirst(o, collector)
          visit o.expr, collector
          collector << " NULLS FIRST"
        end

        def visit_Arel_Nodes_NullsLast(o, collector)
          visit o.expr, collector
          collector << " NULLS LAST"
        end

        def visit_Arel_Nodes_Group(o, collector)
          visit o.expr, collector
        end

        def visit_Arel_Nodes_NamedFunction(o, collector)
          collector.retryable = false
          collector << o.name
          collector << "("
          collector << "DISTINCT " if o.distinct
          inject_join(o.expressions, collector, ", ") << ")"
        end

        def visit_Arel_Nodes_Extract(o, collector)
          collector << "EXTRACT(#{o.field.to_s.upcase} FROM "
          visit(o.expr, collector) << ")"
        end

        def visit_Arel_Nodes_Count(o, collector)
          aggregate "COUNT", o, collector
        end

        def visit_Arel_Nodes_Sum(o, collector)
          aggregate "SUM", o, collector
        end

        def visit_Arel_Nodes_Max(o, collector)
          aggregate "MAX", o, collector
        end

        def visit_Arel_Nodes_Min(o, collector)
          aggregate "MIN", o, collector
        end

        def visit_Arel_Nodes_Avg(o, collector)
          aggregate "AVG", o, collector
        end

        def visit_Arel_Nodes_TableAlias(o, collector)
          collector = visit o.relation, collector
          collector << " "
          collector << quote_table_name(o.name)
        end

        def visit_Arel_Nodes_Between(o, collector)
          collector = visit o.left, collector
          collector << " BETWEEN "
          visit o.right, collector
        end

        def visit_Arel_Nodes_GreaterThanOrEqual(o, collector)
          case unboundable?(o.right)
          when 1
            return collector << "1=0"
          when -1
            return collector << "1=1"
          end
          collector = visit o.left, collector
          collector << " >= "
          visit o.right, collector
        end

        def visit_Arel_Nodes_GreaterThan(o, collector)
          case unboundable?(o.right)
          when 1
            return collector << "1=0"
          when -1
            return collector << "1=1"
          end
          collector = visit o.left, collector
          collector << " > "
          visit o.right, collector
        end

        def visit_Arel_Nodes_LessThanOrEqual(o, collector)
          case unboundable?(o.right)
          when 1
            return collector << "1=1"
          when -1
            return collector << "1=0"
          end
          collector = visit o.left, collector
          collector << " <= "
          visit o.right, collector
        end

        def visit_Arel_Nodes_LessThan(o, collector)
          case unboundable?(o.right)
          when 1
            return collector << "1=1"
          when -1
            return collector << "1=0"
          end
          collector = visit o.left, collector
          collector << " < "
          visit o.right, collector
        end

        def visit_Arel_Nodes_Matches(o, collector)
          collector = visit o.left, collector
          collector << " LIKE "
          collector = visit o.right, collector
          if o.escape
            collector << " ESCAPE "
            visit o.escape, collector
          else
            collector
          end
        end

        def visit_Arel_Nodes_DoesNotMatch(o, collector)
          collector = visit o.left, collector
          collector << " NOT LIKE "
          collector = visit o.right, collector
          if o.escape
            collector << " ESCAPE "
            visit o.escape, collector
          else
            collector
          end
        end

        def visit_Arel_Nodes_JoinSource(o, collector)
          if o.left
            collector = visit o.left, collector
          end
          if o.right.any?
            collector << " " if o.left
            collector = inject_join o.right, collector, " "
          end
          collector
        end

        def visit_Arel_Nodes_Regexp(o, collector)
          raise NotImplementedError, "~ not implemented for this db"
        end

        def visit_Arel_Nodes_NotRegexp(o, collector)
          raise NotImplementedError, "!~ not implemented for this db"
        end

        def visit_Arel_Nodes_StringJoin(o, collector)
          visit o.left, collector
        end

        def visit_Arel_Nodes_FullOuterJoin(o, collector)
          collector << "FULL OUTER JOIN "
          collector = visit o.left, collector
          collector << " "
          visit o.right, collector
        end

        def visit_Arel_Nodes_OuterJoin(o, collector)
          collector << "LEFT OUTER JOIN "
          collector = visit o.left, collector
          collector << " "
          visit o.right, collector
        end

        def visit_Arel_Nodes_RightOuterJoin(o, collector)
          collector << "RIGHT OUTER JOIN "
          collector = visit o.left, collector
          collector << " "
          visit o.right, collector
        end

        def visit_Arel_Nodes_InnerJoin(o, collector)
          collector << "INNER JOIN "
          collector = visit o.left, collector
          if o.right
            collector << " "
            visit(o.right, collector)
          else
            collector
          end
        end

        def visit_Arel_Nodes_On(o, collector)
          collector << "ON "
          visit o.expr, collector
        end

        def visit_Arel_Nodes_Not(o, collector)
          collector << "NOT ("
          visit(o.expr, collector) << ")"
        end

        def visit_Arel_Table(o, collector)
          if Arel::Nodes::Node === o.name
            visit o.name, collector
          else
            collector << quote_table_name(o.name)
          end

          if o.table_alias
            collector << " " << quote_table_name(o.table_alias)
          end

          collector
        end

        def visit_Arel_Nodes_In(o, collector)
          attr, values = o.left, o.right

          if Array === values
            collector.preparable = false

            unless values.empty?
              values.delete_if { |value| unboundable?(value) }
            end

            return collector << "1=0" if values.empty?
          end

          visit(attr, collector) << " IN ("
          visit(values, collector) << ")"
        end

        def visit_Arel_Nodes_NotIn(o, collector)
          attr, values = o.left, o.right

          if Array === values
            collector.preparable = false

            unless values.empty?
              values.delete_if { |value| unboundable?(value) }
            end

            return collector << "1=1" if values.empty?
          end

          visit(attr, collector) << " NOT IN ("
          visit(values, collector) << ")"
        end

        def visit_Arel_Nodes_And(o, collector)
          inject_join o.children, collector, " AND "
        end

        def visit_Arel_Nodes_Or(o, collector)
          inject_join o.children, collector, " OR "
        end

        def visit_Arel_Nodes_Assignment(o, collector)
          case o.right
          when Arel::Nodes::Node, Arel::Attributes::Attribute, ActiveModel::Attribute
            collector = visit o.left, collector
            collector << " = "
            visit o.right, collector
          else
            collector = visit o.left, collector
            collector << " = "
            collector << quote(o.right).to_s
          end
        end

        def visit_Arel_Nodes_Equality(o, collector)
          right = o.right

          return collector << "1=0" if unboundable?(right)

          collector = visit o.left, collector

          if right.nil?
            collector << " IS NULL"
          else
            collector << " = "
            visit right, collector
          end
        end

        def visit_Arel_Nodes_CaseSensitiveEquality(o, collector)
          visit @connection.case_sensitive_comparison(o.left, o.right), collector
        end

        def visit_Arel_Nodes_CaseInsensitiveEquality(o, collector)
          visit @connection.case_insensitive_comparison(o.left, o.right), collector
        end

        def visit_Arel_Nodes_IsNotDistinctFrom(o, collector)
          if o.right.nil?
            collector = visit o.left, collector
            collector << " IS NULL"
          else
            collector = is_distinct_from(o, collector)
            collector << " = 0"
          end
        end

        def visit_Arel_Nodes_IsDistinctFrom(o, collector)
          if o.right.nil?
            collector = visit o.left, collector
            collector << " IS NOT NULL"
          else
            collector = is_distinct_from(o, collector)
            collector << " = 1"
          end
        end

        def visit_Arel_Nodes_NotEqual(o, collector)
          right = o.right

          return collector << "1=1" if unboundable?(right)

          collector = visit o.left, collector

          if right.nil?
            collector << " IS NOT NULL"
          else
            collector << " != "
            visit right, collector
          end
        end

        def visit_Arel_Nodes_As(o, collector)
          collector = visit o.left, collector
          collector << " AS "
          visit o.right, collector
        end

        def visit_Arel_Nodes_Case(o, collector)
          collector << "CASE "
          if o.case
            visit o.case, collector
            collector << " "
          end
          o.conditions.each do |condition|
            visit condition, collector
            collector << " "
          end
          if o.default
            visit o.default, collector
            collector << " "
          end
          collector << "END"
        end

        def visit_Arel_Nodes_When(o, collector)
          collector << "WHEN "
          visit o.left, collector
          collector << " THEN "
          visit o.right, collector
        end

        def visit_Arel_Nodes_Else(o, collector)
          collector << "ELSE "
          visit o.expr, collector
        end

        def visit_Arel_Nodes_UnqualifiedColumn(o, collector)
          collector << quote_column_name(o.name)
        end

        def visit_Arel_Nodes_Cte(o, collector)
          collector << quote_table_name(o.name)
          collector << " AS "

          case o.materialized
          when true
            collector << "MATERIALIZED "
          when false
            collector << "NOT MATERIALIZED "
          end

          visit o.relation, collector
        end

        def visit_Arel_Attributes_Attribute(o, collector)
          join_name = o.relation.table_alias || o.relation.name
          collector << quote_table_name(join_name) << "." << quote_column_name(o.name)
        end

        BIND_BLOCK = ActiveSupport::Ractors.shareable_proc { "?" }
        private_constant :BIND_BLOCK

        def bind_block; BIND_BLOCK; end

        def visit_ActiveModel_Attribute(o, collector)
          collector.add_bind(o, &bind_block)
        end

        def visit_ActiveRecord_PredicateBuilder_ComparisonAttribute(o, collector)
          visit o.expression, collector
        end

        def visit_ActiveRecord_PredicateBuilder_ComparisonValue(o, collector)
          visit o.expression, collector
        end

        def visit_Arel_Nodes_BindParam(o, collector)
          collector.add_bind(o.value, &bind_block)
        end

        def visit_Arel_Nodes_SqlLiteral(o, collector)
          collector.preparable = false
          collector.retryable &&= o.retryable
          collector << o.to_s
        end

        def visit_Arel_Nodes_BoundSqlLiteral(o, collector)
          collector.retryable = false
          bind_index = 0

          new_bind = lambda do |value|
            if Arel.arel_node?(value)
              visit value, collector
            elsif value.is_a?(Array)
              if value.empty?
                collector << @connection.quote(nil)
              else
                if value.none? { |v| Arel.arel_node?(v) }
                  collector.add_binds(value.map { |v| @connection.cast_bound_value(v) }, &bind_block)
                else
                  value.each_with_index do |v, i|
                    collector << ", " unless i == 0
                    if Arel.arel_node?(v)
                      visit v, collector
                    else
                      collector.add_bind(@connection.cast_bound_value(v), &bind_block)
                    end
                  end
                end
              end
            else
              collector.add_bind(@connection.cast_bound_value(value), &bind_block)
            end
          end

          if o.positional_binds
            o.sql_with_placeholders.scan(/\?|([^?]+)/) do
              if $1
                collector << $1
              else
                value = o.positional_binds[bind_index]
                bind_index += 1

                new_bind.call(value)
              end
            end
          else
            o.sql_with_placeholders.scan(/:(?<!::)([a-zA-Z]\w*)|([^:]+|.)/) do
              if $2
                collector << $2
              else
                value = o.named_binds[$1.to_sym]
                new_bind.call(value)
              end
            end
          end

          collector
        end

        def visit_Integer(o, collector)
          collector << o.to_s
        end

        def unsupported(o, collector)
          raise UnsupportedVisitError.new(o)
        end

        alias :visit_ActiveSupport_Multibyte_Chars :unsupported
        alias :visit_ActiveSupport_StringInquirer  :unsupported
        alias :visit_BigDecimal                    :unsupported
        alias :visit_Class                         :unsupported
        alias :visit_Date                          :unsupported
        alias :visit_DateTime                      :unsupported
        alias :visit_FalseClass                    :unsupported
        alias :visit_Float                         :unsupported
        alias :visit_Hash                          :unsupported
        alias :visit_NilClass                      :unsupported
        alias :visit_String                        :unsupported
        alias :visit_Symbol                        :unsupported
        alias :visit_Time                          :unsupported
        alias :visit_TrueClass                     :unsupported

        def visit_Arel_Nodes_InfixOperation(o, collector)
          collector = visit o.left, collector
          collector << " #{o.operator} "
          visit o.right, collector
        end

        def visit_Arel_Nodes_UnaryOperation(o, collector)
          collector << " #{o.operator} "
          visit o.expr, collector
        end

        def visit_Array(o, collector)
          inject_join o, collector, ", "
        end
        alias :visit_Set :visit_Array

        def visit_Arel_Nodes_Fragments(o, collector)
          inject_join o.values, collector, " "
        end

        def quote(value)
          return value if Arel::Nodes::SqlLiteral === value
          @connection.quote value
        end

        def quote_table_name(name)
          return name if Arel::Nodes::SqlLiteral === name
          @connection.quote_table_name(name)
        end

        def quote_column_name(name)
          return name if Arel::Nodes::SqlLiteral === name
          @connection.quote_column_name(name)
        end

        def sanitize_as_sql_comment(value)
          return value if Arel::Nodes::SqlLiteral === value
          @connection.sanitize_as_sql_comment(value)
        end

        def collect_optimizer_hints(o, collector)
          maybe_visit o.optimizer_hints, collector
        end

        def maybe_visit(thing, collector)
          return collector unless thing
          collector << " "
          visit thing, collector
        end

        def inject_join(list, collector, join_str)
          list.each_with_index do |x, i|
            collector << join_str unless i == 0
            collector = visit(x, collector)
          end
          collector
        end

        def unboundable?(value)
          value.respond_to?(:unboundable?) && value.unboundable?
        end

        def has_join_sources?(o)
          o.relation.is_a?(Nodes::JoinSource) && !o.relation.right.empty?
        end

        def has_limit_or_offset_or_orders?(o)
          o.limit || o.offset || !o.orders.empty?
        end

        def has_group_by_and_having?(o)
          !o.groups.empty? && !o.havings.empty?
        end

        # The default strategy for an UPDATE with joins is to use a subquery. This doesn't work
        # on MySQL (even when aliasing the tables), but MySQL allows using JOIN directly in
        # an UPDATE statement, so in the MySQL visitor we redefine this to do that.
        def prepare_update_statement(o)
          if o.key && (has_limit_or_offset_or_orders?(o) || has_join_sources?(o) || has_group_by_and_having?(o))
            stmt = o.clone
            stmt.limit = nil
            stmt.offset = nil
            stmt.orders = []
            columns = Arel::Nodes::Grouping.new(o.key)
            stmt.wheres = [Nodes::In.new(columns, [build_subselect(o.key, o)])]
            stmt.relation = o.relation.left if has_join_sources?(o)
            stmt.groups = o.groups unless o.groups.empty?
            stmt.havings = o.havings unless o.havings.empty?
            stmt
          else
            o
          end
        end
        alias :prepare_delete_statement :prepare_update_statement

        # Used by dialects that support `UPDATE ... FROM` (PostgreSQL, SQLite).
        # Join clauses cannot reference the target table, so alias the updated
        # table, place the entire relation in the FROM clause, and add a
        # self-join (which requires the primary key).
        def prepare_update_statement_with_self_join(o)
          stmt = o.clone
          stmt.relation, stmt.wheres = o.relation.clone, o.wheres.clone
          stmt.relation.right = [stmt.relation.left, *stmt.relation.right]
          stmt.relation.left = stmt.relation.left.alias("__active_record_update_alias")
          Array.wrap(o.key).each do |key|
            stmt.wheres << key.eq(stmt.relation.left[key.name])
          end
          stmt
        end

        # FIXME: we should probably have a 2-pass visitor for this
        def build_subselect(key, o)
          stmt             = Nodes::SelectStatement.new
          core             = stmt.cores.first
          core.froms       = o.relation
          core.wheres      = o.wheres
          core.projections = [key]
          core.groups      = o.groups unless o.groups.empty?
          core.havings     = o.havings unless o.havings.empty?
          stmt.limit       = o.limit
          stmt.offset      = o.offset
          stmt.orders      = o.orders
          stmt
        end

        def infix_value(o, collector, value)
          collector = visit o.left, collector
          collector << value
          visit o.right, collector
        end

        def infix_value_with_paren(o, collector, value, suppress_parens = false)
          collector << "( " unless suppress_parens
          collector = if o.left.class == o.class
            infix_value_with_paren(o.left, collector, value, true)
          else
            grouping_parentheses o.left, collector, false
          end
          collector << value
          collector = if o.right.class == o.class
            infix_value_with_paren(o.right, collector, value, true)
          else
            grouping_parentheses o.right, collector, false
          end
          collector << " )" unless suppress_parens
          collector
        end

        # Used by some visitors to enclose select queries in parentheses
        def grouping_parentheses(o, collector, always_wrap_selects = true)
          if o.is_a?(Nodes::SelectStatement) && (always_wrap_selects || require_parentheses?(o))
            collector << "("
            visit o, collector
            collector << ")"
            collector
          else
            visit o, collector
          end
        end

        def require_parentheses?(o)
          !o.orders.empty? || o.limit || o.offset
        end

        def aggregate(name, o, collector)
          collector << "#{name}("
          if o.distinct
            collector << "DISTINCT "
          end
          inject_join(o.expressions, collector, ", ") << ")"
        end

        def is_distinct_from(o, collector)
          collector << "CASE WHEN "
          collector = visit o.left, collector
          collector << " = "
          collector = visit o.right, collector
          collector << " OR ("
          collector = visit o.left, collector
          collector << " IS NULL AND "
          collector = visit o.right, collector
          collector << " IS NULL)"
          collector << " THEN 0 ELSE 1 END"
        end

        def collect_ctes(children, collector)
          children.each_with_index do |child, i|
            collector << ", " unless i == 0
            visit child.to_cte, collector
          end

          collector
        end
    end
  end
end

# ---- arel/visitors/sqlite.rb ----
module Arel # :nodoc: all
  module Visitors
    class SQLite < Arel::Visitors::ToSql
      private
        def visit_Arel_Nodes_UpdateStatement(o, collector)
          collector.retryable = false
          o = prepare_update_statement(o)

          collector << "UPDATE "

          # UPDATE with JOIN is in the form of:
          #
          #   UPDATE t1 AS __active_record_update_alias
          #   SET ..
          #   FROM t1 JOIN t2 ON t2.join_id = t1.join_id ..
          #   WHERE t1.id = __active_record_update_alias.id AND ..
          if has_join_sources?(o)
            collector = visit o.relation.left, collector
            collect_nodes_for o.values, collector, " SET "
            collector << " FROM "
            collector = inject_join o.relation.right, collector, " "
          else
            collector = visit o.relation, collector
            collect_nodes_for o.values, collector, " SET "
          end

          collect_nodes_for o.wheres, collector, " WHERE ", " AND "
          collect_nodes_for o.orders, collector, " ORDER BY "
          maybe_visit o.limit, collector
          maybe_visit o.comment, collector
        end

        def prepare_update_statement(o)
          # Sqlite need to be built with the SQLITE_ENABLE_UPDATE_DELETE_LIMIT compile-time option
          # to support LIMIT/OFFSET/ORDER in UPDATE and DELETE statements.
          if o.key && has_join_sources?(o) && !has_group_by_and_having?(o) && !has_limit_or_offset_or_orders?(o)
            prepare_update_statement_with_self_join(o)
          else
            super
          end
        end

        def visit_Arel_Nodes_TableAlias(o, collector)
          # "AS" is not optional in "{UPDATE | DELETE} table AS alias ..."
          collector = visit o.relation, collector
          collector << " AS "
          collector << quote_table_name(o.name)
        end

        # Locks are not supported in SQLite
        def visit_Arel_Nodes_Lock(o, collector)
          collector
        end

        def visit_Arel_Nodes_SelectStatement(o, collector)
          o.limit = Arel::Nodes::Limit.new(-1) if o.offset && !o.limit
          super
        end

        def visit_Arel_Nodes_IsNotDistinctFrom(o, collector)
          collector = visit o.left, collector
          collector << " IS "
          visit o.right, collector
        end

        def visit_Arel_Nodes_IsDistinctFrom(o, collector)
          collector = visit o.left, collector
          collector << " IS NOT "
          visit o.right, collector
        end

        # Queries used in UNION should not be wrapped by parentheses,
        # because it is an invalid syntax in SQLite.
        def infix_value_with_paren(o, collector, value, suppress_parens = false)
          collector << "( " unless suppress_parens

          left = o.left.is_a?(Nodes::Grouping) ? o.left.expr : o.left
          collector = if left.class == o.class
            infix_value_with_paren(left, collector, value, true)
          else
            grouping_parentheses left, collector, false
          end

          collector << value

          right = o.right.is_a?(Nodes::Grouping) ? o.right.expr : o.right
          collector = if right.class == o.class
            infix_value_with_paren(right, collector, value, true)
          else
            grouping_parentheses right, collector, false
          end

          collector << " )" unless suppress_parens
          collector
        end
    end
  end
end

# ---- arel/visitors/postgresql.rb ----
module Arel # :nodoc: all
  module Visitors
    class PostgreSQL < Arel::Visitors::ToSql
      private
        def visit_Arel_Nodes_UpdateStatement(o, collector)
          collector.retryable = false
          o = prepare_update_statement(o)

          collector << "UPDATE "

          # UPDATE with JOIN is in the form of:
          #
          #   UPDATE t1 AS __active_record_update_alias
          #   SET ..
          #   FROM t1 JOIN t2 ON t2.join_id = t1.join_id ..
          #   WHERE t1.id = __active_record_update_alias.id AND ..
          if has_join_sources?(o)
            collector = visit o.relation.left, collector
            collect_nodes_for o.values, collector, " SET "
            collector << " FROM "
            collector = inject_join o.relation.right, collector, " "
          else
            collector = visit o.relation, collector
            collect_nodes_for o.values, collector, " SET "
          end

          collect_nodes_for o.wheres, collector, " WHERE ", " AND "
          collect_nodes_for o.orders, collector, " ORDER BY "
          maybe_visit o.limit, collector
          maybe_visit o.comment, collector

          if o.returning.empty?
            collector
          else
            collector << " RETURNING "
            visit o.returning, collector
          end
        end

        # In the simple case, PostgreSQL allows us to place FROM or JOINs directly into the UPDATE
        # query. However, this does not allow for LIMIT, OFFSET and ORDER. To support
        # these, we must use a subquery.
        def prepare_update_statement(o)
          if o.key && has_join_sources?(o) && !has_group_by_and_having?(o) && !has_limit_or_offset_or_orders?(o)
            prepare_update_statement_with_self_join(o)
          else
            super
          end
        end

        def visit_Arel_Nodes_Matches(o, collector)
          op = o.case_sensitive ? " LIKE " : " ILIKE "
          collector = infix_value o, collector, op
          if o.escape
            collector << " ESCAPE "
            visit o.escape, collector
          else
            collector
          end
        end

        def visit_Arel_Nodes_DoesNotMatch(o, collector)
          op = o.case_sensitive ? " NOT LIKE " : " NOT ILIKE "
          collector = infix_value o, collector, op
          if o.escape
            collector << " ESCAPE "
            visit o.escape, collector
          else
            collector
          end
        end

        def visit_Arel_Nodes_Regexp(o, collector)
          op = o.case_sensitive ? " ~ " : " ~* "
          infix_value o, collector, op
        end

        def visit_Arel_Nodes_NotRegexp(o, collector)
          op = o.case_sensitive ? " !~ " : " !~* "
          infix_value o, collector, op
        end

        def visit_Arel_Nodes_DistinctOn(o, collector)
          collector << "DISTINCT ON ( "
          visit(o.expr, collector) << " )"
        end

        def visit_Arel_Nodes_GroupingElement(o, collector)
          collector << "( "
          visit(o.expr, collector) << " )"
        end

        def visit_Arel_Nodes_Cube(o, collector)
          collector << "CUBE"
          grouping_array_or_grouping_element o, collector
        end

        def visit_Arel_Nodes_RollUp(o, collector)
          collector << "ROLLUP"
          grouping_array_or_grouping_element o, collector
        end

        def visit_Arel_Nodes_GroupingSet(o, collector)
          collector << "GROUPING SETS"
          grouping_array_or_grouping_element o, collector
        end

        def visit_Arel_Nodes_Lateral(o, collector)
          collector << "LATERAL "
          grouping_parentheses o.expr, collector
        end

        def visit_Arel_Nodes_InnerJoin(o, collector)
          return super if o.right
          collector << "CROSS JOIN "
          visit o.left, collector
        end

        def visit_Arel_Nodes_IsNotDistinctFrom(o, collector)
          collector = visit o.left, collector
          collector << " IS NOT DISTINCT FROM "
          visit o.right, collector
        end

        def visit_Arel_Nodes_IsDistinctFrom(o, collector)
          collector = visit o.left, collector
          collector << " IS DISTINCT FROM "
          visit o.right, collector
        end

        BIND_BLOCK = ActiveSupport::Ractors.shareable_proc { |i| "$#{i}" }
        private_constant :BIND_BLOCK

        def bind_block; BIND_BLOCK; end

        # Utilized by GroupingSet, Cube & RollUp visitors to
        # handle grouping aggregation semantics
        def grouping_array_or_grouping_element(o, collector)
          if o.expr.is_a? Array
            collector << "( "
            visit o.expr, collector
            collector << " )"
          else
            visit o.expr, collector
          end
        end
    end
  end
end

# ---- arel.rb ----
module Arel
  def self.sql(sql_string, *positional_binds, retryable: false, **named_binds)
    if Arel::Nodes::SqlLiteral === sql_string
      sql_string
    elsif positional_binds.empty? && named_binds.empty?
      Arel::Nodes::SqlLiteral.new(sql_string, retryable: retryable)
    else
      Arel::Nodes::BoundSqlLiteral.new sql_string, positional_binds, named_binds
    end
  end

  def self.star # :nodoc:
    sql("*", retryable: true)
  end

  def self.arel_node?(value) # :nodoc:
    value.is_a?(Arel::Nodes::Node) || value.is_a?(Arel::Attribute) || value.is_a?(Arel::Nodes::SqlLiteral)
  end

  def self.fetch_attribute(value, &block) # :nodoc:
    unless String === value
      value.fetch_attribute(&block)
    end
  end
end
