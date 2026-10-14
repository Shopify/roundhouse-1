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
    # the relation as `self`). Only what the model answers publicly
    # (`respond_to?`) is forwarded, so a typo is still a NoMethodError.
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
end

module ActiveRecord
  # Rails' ClassSpecificRelation (relation/delegation.rb): a name the
  # relation does not define, but the model class does, runs on the class
  # with the relation as its scope. A module prepended on the Relation
  # (Core's `GenerateOid::ActiveRecordInsertPatch` asks `oid_enabled?`)
  # reads the class's own methods this way. Anything the class does not
  # answer is still a NoMethodError.
  class Relation
    def method_missing(name, *args, **kwargs, &block)
      return super unless @model.respond_to?(name)
      scoping { @model.public_send(name, *args, **kwargs, &block) }
    end

    def respond_to_missing?(name, include_private = false)
      @model.respond_to?(name) || super
    end
  end
end
