# frozen_string_literal: true

# `record.association(:name)`: the association object, ported from
# activerecord 8.2.0.alpha (Shopify/rails 5813559f, the version Core pins:
# lib/active_record/associations.rb `#association`, associations/
# association.rb, singular_association.rb, collection_association.rb) for
# the CRuby and JRuby trees. Core reads it to ask whether an association
# is already in memory (`association(:line_items).loaded?`), to read or
# seed the in-memory records (`.target`, `.target = `) and to drop them
# (`.reset`, `.reload`).
#
# Rails keeps an association's state in the association object, which
# the record memoizes. The emitted models keep the same state in two
# ivars per association, `@<name>_cache` (the target) and
# `@<name>_loaded`, which their readers, writers and preloaders already
# read and write (`lower::model_to_library::associations`). This object
# is a view over those two ivars, so a preload, a writer assignment and
# a memoizing reader are all visible to it, and its `target =` / `reset`
# are visible to them. Each model answers `association(name)` with the
# right subclass (`emit::ruby::library::apply_association_reader`), and
# raises AssociationNotFoundError for a name it does not declare, as
# Rails does.
#
# Differences, all at the edges:
# - The object is built per call, not memoized: its state is the
#   record's, so two calls answer the same, but they are not `equal?`.
# - No reflection: `reflection`, `klass`, `scope`, `options` and the
#   strict-loading/inverse hooks are not ported. `stale_target?` is not
#   either; a foreign-key write resets a memoized belongs_to instead
#   (`apply_belongs_to_memoization`), so `loaded?` reads false after it
#   where Rails keeps true and calls the target stale.
# - A has_many or has_one reader re-queries rather than memoizing, so a
#   plain `record.comments` read does not set `loaded?`; a preload, a
#   writer, `target =`, `reader` (singular), `load_target` and `reload`
#   do. A belongs_to reader memoizes, as Rails does.
module ActiveRecord
  module Associations
    class Association
      attr_reader :owner

      def initialize(owner, name)
        @owner = owner
        @name = name
        @loaded_ivar = :"@#{name}_loaded"
        @cache_ivar = :"@#{name}_cache"
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
      # Rails' ends on `@updated = false`.
      def reset
        super
        false
      end
    end

    class HasOneAssociation < SingularAssociation
    end

    class HasManyAssociation < CollectionAssociation
    end

    class HasManyThroughAssociation < HasManyAssociation
    end
  end
end
