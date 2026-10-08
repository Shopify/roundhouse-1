# frozen_string_literal: true

# `record.association(:name)`: the association object, ported from
# activerecord 8.2.0.alpha (Shopify/rails 5813559f, the version Core pins:
# lib/active_record/associations.rb `#association`, associations/
# association.rb, singular_association.rb, collection_association.rb,
# belongs_to_association.rb, belongs_to_polymorphic_association.rb) for
# the CRuby and JRuby trees. Core reads it to ask whether an association
# is already in memory (`association(:line_items).loaded?`), to read or
# seed the in-memory records (`.target`, `.target = `), to drop them
# (`.reset`, `.reload`), and for the target class, the association's
# query and whether a foreign-key write left the target behind
# (`.klass`, `.scope`, `.stale_target?`).
#
# Rails keeps an association's state in the association object, which
# the record memoizes. The emitted models keep the same state in two
# ivars per association, `@<name>_cache` (the target) and
# `@<name>_loaded`, which their readers, writers and preloaders already
# read and write (`lower::model_to_library::associations`). This object
# is a view over those two ivars, so a preload, a writer assignment and
# a memoizing reader are all visible to it, and its `target =` / `reset`
# are visible to them. Each model answers `association(name)` with the
# right subclass, its target class and its key
# (`emit::ruby::library::apply_association_reader`), for every
# association it has (declared, spliced from a concern, or inherited
# from an abstract base or an STI parent), and raises
# AssociationNotFoundError for a name it does not have, as Rails does.
#
# Readers memoize as Rails' do: a belongs_to, polymorphic or not
# (`apply_belongs_to_memoization`), and a has_many or has_one
# (`apply_collection_memoization`) keep what they load, and a reader
# that answers a Relation (`:through`) records what its own query loads
# (`Association.through`). So `loaded?` is true after a read that loads
# and false after one Rails answers without loading (`first`, `count`,
# `size`, `empty?`, a narrowed chain). A record created through a
# loaded association joins its target (`Association.created`); the
# CollectionProxy's `reload` / `reset` run here (`Association.proxy`).
#
# Differences, all at the edges:
# - The object is built per call, not memoized: its state is the
#   record's, so two calls answer the same, but they are not `equal?`.
# - No reflection object: `reflection`, `options` and the strict-
#   loading/inverse hooks are not ported. `scope` is ported for a
#   belongs_to (a polymorphic one with its type set) and an unscoped,
#   non-`as:`, non-`:through` has_many or has_one; `stale_target?` for
#   all but a polymorphic belongs_to and a `:through` association. The
#   rest raise NotImplementedError, and the analyzer refuses a static
#   call to them (`analyze::association_surface`), as it does a literal
#   name the model does not have.
# - `loaded?`, `target` and `stale_target?` answer as Rails'.
module ActiveRecord
  module Associations
    class Association
      attr_reader :owner

      # `owner.<name>.build/create/create!(…)`, which the scope-chain
      # lowering writes as the target class's constructor with the
      # foreign key preset: the record joins the owner's loaded target,
      # as Rails' `add_to_target` adds it (saved or not). An owner that
      # has not loaded the association reads it from the database later,
      # which finds the record if it saved; an unsaved one (`build`) it
      # would not find, so the owner loads the association first (its
      # reader, which keeps what it loads) and the record joins that:
      # what Rails' next read answers, the rows and the built record.
      # The record holds the owner through its inverse belongs_to, as
      # Rails' `set_inverse_instance` sets it, where the owner model
      # names one (`emit::ruby::collection_autosave`).
      def self.created(owner, name, record)
        owner._await_preload if owner.respond_to?(:_await_preload)
        owner._set_inverse_instance(name, record) if owner.respond_to?(:_set_inverse_instance)
        if owner.instance_variable_get(:"@#{name}_loaded") != true && record.new_record?
          owner.public_send(name)
        end
        if owner.instance_variable_get(:"@#{name}_loaded") == true
          records = owner.instance_variable_get(:"@#{name}_cache")
          if records.nil?
            records = []
            owner.instance_variable_set(:"@#{name}_cache", records)
          end
          records << record unless records.any? { |r| r.equal?(record) }
        end
        record
      end

      # `owner.<name>.reload` / `.reset` on a collection reader, which
      # `lower::assoc_loaded` marks (`IrHint::AssociationProxy`): Rails'
      # CollectionProxy reloads or resets its association. `reload`
      # answers the reloaded records, as the proxy does once loaded;
      # `reset` answers the owner's association, unloaded (the analyzer
      # refuses a call chained on it, which Rails would make on the
      # proxy).
      def self.proxy(owner, name, call)
        association = owner.association(name)
        if call == :reload
          association.reload
          association.reader
        else
          association.reset
          association
        end
      end

      # A has_many :through reader answers a Relation, not the
      # CollectionProxy Rails memoizes, so loading it (`each`, `map`,
      # `to_a`) would leave the association unloaded. The reader's
      # relation (`apply_through_target_tracking`) records what it
      # loads as the association's target, as the proxy does: only for
      # the reader's own query, so a narrowed chain (`where`, `first`'s
      # limit) does not.
      def self.through(owner, name, relation)
        sql = relation.to_sql
        loaded_ivar = :"@#{name}_loaded"
        cache_ivar = :"@#{name}_cache"
        remember = lambda do |rel, records|
          if owner.instance_variable_get(loaded_ivar) != true && rel.to_sql == sql
            owner.instance_variable_set(cache_ivar, records)
            owner.instance_variable_set(loaded_ivar, true)
          end
        end
        relation.define_singleton_method(:loaded_records) do
          records = super()
          remember.call(self, records.dup)
          records
        end
        relation.define_singleton_method(:to_a) do
          records = super()
          remember.call(self, records.dup)
          records
        end
        relation
      end

      # klass_name: the target class's name, looked up when asked as
      # Rails' reflection does (nil for a polymorphic belongs_to, which
      # reads it from the record). foreign_key: the column the
      # association's query matches (on the target for has_many/has_one,
      # on the owner for belongs_to). scopable: `scope` can be built from
      # those two alone (no scope lambda, `as:` or `:through`).
      def initialize(owner, name, klass_name = nil, foreign_key = nil, scopable = false)
        @owner = owner
        @name = name
        @klass_name = klass_name
        @foreign_key = foreign_key
        @scopable = scopable
        @loaded_ivar = :"@#{name}_loaded"
        @cache_ivar = :"@#{name}_cache"
      end

      def klass
        @klass_name && Object.const_get(@klass_name)
      end

      # Rails' `association_scope`: the target class's relation narrowed
      # to the owner's records.
      def scope
        unless @scopable
          raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).scope is not ported for a scoped, polymorphic or :through association"
        end
        ActiveRecord::Relation.new(klass).where(scope_condition)
      end

      # A has_many's or has_one's key is on the target.
      def scope_condition
        { @foreign_key => @owner.id }
      end

      # Rails compares the stale state recorded at load with the
      # current one; a has_many or has_one that is not `:through` has
      # none, so its target is never stale.
      def stale_target?
        false
      end

      def target
        await_preload
        @owner.instance_variable_get(@cache_ivar)
      end

      def reset
        await_preload
        @owner.instance_variable_set(@loaded_ivar, false)
        nil
      end

      def reload(force = false)
        reset
        load_target
        self unless target.nil?
      end

      def loaded?
        await_preload
        @owner.instance_variable_get(@loaded_ivar) == true
      end

      def loaded!
        await_preload
        @owner.instance_variable_set(@loaded_ivar, true)
        nil
      end

      def target=(target)
        await_preload
        @owner.instance_variable_set(@cache_ivar, target)
        loaded!
      end

      private
        # A Relation's `includes` runs when a record first reads an
        # association (`lower::deferred_preload`); Rails has run it by
        # the time the records exist, so every member runs it first.
        def await_preload
          @owner._await_preload if @owner.respond_to?(:_await_preload)
        end

        # The model's own reader, with the loaded guard down: the query
        # Rails' `find_target` runs.
        def find_target
          @owner.instance_variable_set(@loaded_ivar, false)
          @owner.public_send(@name)
        end
    end

    class SingularAssociation < Association
      def reader
        reload unless loaded?
        target
      end

      def reset
        super
        @owner.instance_variable_set(@cache_ivar, nil)
        nil
      end

      def load_target
        unless loaded?
          self.target = find_target
        end
        target
      end
    end

    class CollectionAssociation < Association
      # Rails' collection reader is the CollectionProxy, which loads
      # when enumerated; the model's reader is the runtime's answer for
      # it, loaded or not.
      def reader
        @owner.public_send(@name)
      end

      # A collection's target is an Array before anything loads, so a
      # caller may append to it (`association(:events).target << event`)
      # and read it back.
      def target
        records = super
        if records.nil?
          records = []
          @owner.instance_variable_set(@cache_ivar, records)
        end
        records
      end

      def reset
        super
        @owner.instance_variable_set(@cache_ivar, [])
        nil
      end

      def load_target
        unless loaded?
          self.target = find_target.to_a
        end
        target
      end
    end

    class BelongsToAssociation < SingularAssociation
      def initialize(owner, name, klass_name = nil, foreign_key = nil, scopable = false, primary_key = :id)
        super(owner, name, klass_name, foreign_key, scopable)
        @primary_key = primary_key
        @stale_loaded_ivar = :"@#{name}_stale_loaded"
        @stale_state_ivar = :"@#{name}_stale_state"
      end

      def scope_condition
        { @primary_key => @owner.public_send(@foreign_key) }
      end

      # A foreign-key write on a loaded belongs_to drops the reader's
      # loaded flag so the next read re-queries
      # (`apply_belongs_to_memoization`), and records that the target
      # was loaded and the key it was loaded for
      # (`@<name>_stale_loaded`, `@<name>_stale_state`). Rails keeps
      # the target loaded and calls it stale while the key differs.
      def loaded?
        super || stale_loaded?
      end

      def stale_target?
        return false if super_loaded?
        stale_loaded? && @owner.instance_variable_get(@stale_state_ivar) != stale_state
      end

      # Rails' ends on `@updated = false`.
      def reset
        super
        @owner.instance_variable_set(@stale_loaded_ivar, false)
        false
      end

      def target=(target)
        @owner.instance_variable_set(@stale_loaded_ivar, false)
        super
      end

      private
        def super_loaded?
          await_preload
          @owner.instance_variable_get(@loaded_ivar) == true
        end

        def stale_loaded?
          @owner.instance_variable_get(@stale_loaded_ivar) == true
        end

        def stale_state
          @owner.public_send(@foreign_key)
        end
    end

    # `belongs_to :x, polymorphic: true`: the target class is the one
    # the record's `<x>_type` names.
    class BelongsToPolymorphicAssociation < BelongsToAssociation
      def initialize(owner, name, foreign_key, foreign_type, primary_key = :id)
        super(owner, name, nil, foreign_key, true, primary_key)
        @foreign_type = foreign_type
      end

      def klass
        type = @owner.public_send(@foreign_type)
        type.nil? || type.to_s.empty? ? nil : Object.const_get(type.to_s)
      end

      def scope
        if klass.nil?
          raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).scope with no #{@foreign_type} is not ported"
        end
        super
      end

      def stale_target?
        raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).stale_target? is not ported for a polymorphic belongs_to"
      end
    end

    class HasOneAssociation < SingularAssociation
    end

    class HasManyAssociation < CollectionAssociation
    end

    class HasManyThroughAssociation < HasManyAssociation
      # Rails' through stale state reads the through record's key when
      # the through association is a belongs_to.
      def stale_target?
        raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).stale_target? is not ported for a :through association"
      end
    end
  end
end
