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
#   loading hooks are not ported. `scope` is ported for a belongs_to
#   (a polymorphic one with its type set), for a has_many or has_one
#   whose compiled query is nameable (`_<name>_association_scope`: its
#   key, `as:` type and scope lambda) and for a `:through` association
#   whose reader answers its Relation; `stale_target?` for all but a
#   polymorphic belongs_to and a `:through` association. The rest raise
#   NotImplementedError, and the analyzer refuses a static call to them
#   (`analyze::association_surface`), as it does a literal name the
#   model does not have. `set_inverse_instance` is ported for a
#   non-polymorphic belongs_to with a literal `inverse_of:` or an
#   automatic inverse; a has_many or has_one's own is the model's
#   `_set_inverse_instance`.
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
          push_target(records, record) unless records.any? { |r| r.equal?(record) }
        end
        record
      end

      # A record added to a target the way Rails' `add_to_target` adds it:
      # held, not saved. The proxy's own `<<` is `concat`, which saves.
      def self.push_target(records, record)
        records.is_a?(CollectionProxy) ? records._push_target(record) : records << record
      end

      # `owner.<name> << record` / `.push(record, …)`, which the
      # scope-chain lowering writes as this for an owner it can name
      # again: Rails' CollectionAssociation#concat. Each record joins the
      # owner's target (loaded first, as `created` does) holding the
      # owner through its inverse. A new owner saves them with itself
      # (`emit::ruby::collection_autosave`); a saved one saves them now,
      # under its key (`_insert_record`), in one transaction that rolls
      # back unless every save succeeds, after the first failure trying
      # no more (`result &&= insert_record(record)`). A record that did
      # not save stays in the target, unsaved. Answers the target, as
      # the CollectionProxy answers itself. A reader that keeps no
      # target (one this owner does not memoize) gets the records pushed
      # onto what it answers, as before.
      def self.concat(owner, name, *records)
        records = records.flatten
        owner._await_preload if owner.respond_to?(:_await_preload)
        target = owner.public_send(name)
        unless owner.instance_variable_get(:"@#{name}_loaded") == true && owner.respond_to?(:_insert_record)
          records.each { |record| push_target(target, record) }
          return target
        end
        add = lambda do |record|
          owner._set_inverse_instance(name, record) if owner.respond_to?(:_set_inverse_instance)
          push_target(target, record) unless target.any? { |r| r.equal?(record) }
        end
        if owner.new_record?
          records.each { |record| add.call(record) }
        else
          within_transaction do
            result = true
            records.each do |record|
              add.call(record)
              result = owner._insert_record(name, record) if result
            end
            result
          end
        end
        target
      end

      # The owner's save, for a model whose has_many saves its records
      # with it (`emit::ruby::collection_autosave`): Rails runs `save`
      # in a transaction (`with_transaction_returning_status`) that rolls
      # back unless the save succeeded, so a record of the association
      # that fails to save (RecordInvalid out of the owner's
      # `after_create` / `after_update`) makes `save` answer false with
      # the owner's INSERT or UPDATE undone. Nested inside another such
      # save (a saved record saving its own records) it joins that
      # transaction, as Rails' joinable one does.
      def self.saving(owner)
        within_transaction do
          enroll(owner)
          yield
        end
      end

      # Rails' `remember_transaction_record_state`: a record saved inside
      # `within_transaction` gets its persisted state and id back if the
      # transaction rolls back (`restore_transaction_record_state`), so
      # an owner whose INSERT was undone is a new record again and a
      # record whose INSERT was undone with it saves anew next time.
      def self.enroll(record)
        frame = Thread.current[:__rh_association_transaction]
        return if frame.nil? || frame.any? { |entry| entry[0].equal?(record) }
        frame << [record, record.instance_variable_get(:@persisted), record.id]
        nil
      end

      def self.restore(frame)
        frame.reverse_each do |record, persisted, id|
          record.instance_variable_set(:@persisted, persisted)
          record.id = id
        end
        nil
      end

      # The block's status: false for RecordInvalid, as Rails' `save`
      # rescues it.
      def self.status_of
        yield ? true : false
      rescue ActiveRecord::RecordInvalid
        false
      end

      # Rails' `transaction { status = yield; raise Rollback unless status }`
      # on each tree's transactions. PostgreSQL's port of Rails'
      # TransactionManager (`ActiveRecord::Base.current_transaction`)
      # joins an open joinable transaction (whose rollback, if it comes,
      # restores the records) and otherwise opens one or a savepoint.
      # SQLite's flat `transaction` cannot nest: inside an open one the
      # block joins it, as Rails' does, and nothing rolls back here. The
      # JRuby driver says nothing about an open transaction, so it takes
      # a SAVEPOINT, which SQLite opens inside one or outside.
      def self.within_transaction(&block)
        return status_of(&block) unless Thread.current[:__rh_association_transaction].nil?
        frame = []
        Thread.current[:__rh_association_transaction] = frame
        status = false
        begin
          if ActiveRecord::Base.respond_to?(:current_transaction)
            ActiveRecord::Base.transaction do |transaction|
              transaction.after_rollback { restore(frame) }
              status = status_of(&block)
              raise ActiveRecord::Rollback unless status
            end
          elsif Db.current_dbh.respond_to?(:transaction_active?)
            if Db.current_dbh.transaction_active?
              status = status_of(&block)
            else
              begin
                # `transaction` swallows the Rollback, as Rails' does.
                ActiveRecord::Base.transaction do
                  status = status_of(&block)
                  raise ActiveRecord::Rollback unless status
                end
                restore(frame) unless status
              rescue StandardError
                restore(frame)
                raise
              end
            end
          else
            Db.exec("SAVEPOINT rh_association_transaction")
            begin
              status = status_of(&block)
            rescue StandardError
              Db.exec("ROLLBACK TO SAVEPOINT rh_association_transaction")
              Db.exec("RELEASE SAVEPOINT rh_association_transaction")
              restore(frame)
              raise
            end
            Db.exec("ROLLBACK TO SAVEPOINT rh_association_transaction") unless status
            Db.exec("RELEASE SAVEPOINT rh_association_transaction")
            restore(frame) unless status
          end
        ensure
          Thread.current[:__rh_association_transaction] = nil
        end
        status
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
      # owner_key: the owner column a has_many's or has_one's foreign key
      # matches (`primary_key:`), `id` for the rest.
      def initialize(owner, name, klass_name = nil, foreign_key = nil, scopable = false, owner_key = :id)
        @owner = owner
        @owner_key = owner_key
        @name = name
        @klass_name = klass_name
        @foreign_key = foreign_key
        @scopable = scopable
        @loaded_ivar = :"@#{name}_loaded"
        @cache_ivar = :"@#{name}_cache"
      end

      def klass
        @klass_name && _compute_type(@klass_name)
      end

      # reflection.rb `compute_type`: a class name without a leading `::`
      # binds relative to the owner's namespace, innermost first; a
      # candidate counts only when it is that very constant, not one the
      # lookup inherited from Object.
      def _compute_type(type_name)
        type_name = type_name.to_s
        return Object.const_get(type_name.delete_prefix("::")) if type_name.start_with?("::")
        candidates = []
        @owner.class.name.to_s.scan(/::|$/) { candidates.unshift "#{$`}::#{type_name}" } unless @owner.class.name.to_s.empty?
        candidates << type_name
        candidates.each do |candidate|
          constant = begin
            Object.const_get(candidate)
          rescue NameError => e
            raise unless e.name.to_s == candidate || candidate.split("::").include?(e.name.to_s)
            next
          end
          return constant if candidate == constant.to_s
        end
        raise NameError.new("uninitialized constant #{candidates.first}", candidates.first)
      end
      private :_compute_type

      # Rails' `association_scope`: the target class's relation narrowed
      # to the owner's records. A has_many or has_one the model compiled
      # a query for (`_<name>_association_scope`, emitted beside the
      # reader: its key, an `as:` type and its scope lambda's chain) is
      # that query; a belongs_to or an unscoped one is built from the
      # key alone.
      def scope
        query = :"_#{@name}_association_scope"
        return @owner.public_send(query) if @owner.respond_to?(query)
        unless @scopable
          raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).scope is not ported for a scoped, polymorphic or :through association"
        end
        ActiveRecord::Relation.new(klass).where(scope_condition)
      end

      # A has_many's or has_one's key is on the target.
      def scope_condition
        { @foreign_key => @owner.public_send(@owner_key) }
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

      # Rails' `inversed_from(record)`: the record this association's
      # inverse holds becomes its target.
      def inversed_from(record)
        self.target = record
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

      # `owner.build_<name>(attributes)` / `create_<name>` /
      # `create_<name>!` (singular_association.rb `build`, `create`,
      # `create!`, `_create_record`): a new target record, which
      # replaces the association's target. `create` saves the record
      # first, then replaces, as Rails' does; the bang raises on an
      # invalid one.
      def build(attributes = {}, &block)
        record = build_record(attributes, &block)
        replace(record, false)
        record
      end

      def create(attributes = {}, &block)
        _create_record(attributes, false, &block)
      end

      def create!(attributes = {}, &block)
        _create_record(attributes, true, &block)
      end

      private
        # Rails' `build_record`: the target class's constructor, then the
        # attributes the association's scope stamps (`initialize_attributes`).
        def build_record(attributes, &block)
          record = klass.new(attributes, &block)
          initialize_attributes(record)
          record
        end

        def _create_record(attributes, raise_error, &block)
          record = build_record(attributes, &block)
          saved = record.save
          replace(record, true)
          raise ActiveRecord::RecordInvalid, record if !saved && raise_error
          record
        end
    end

    class CollectionAssociation < Association
      # Rails' `set_inverse_instance(record)`: the record holds the owner
      # through its inverse belongs_to, where the owner model names one
      # (`_set_inverse_instance`, as `Collection.created` asks). Public,
      # as Core's ModelCaching calls it on each prefetched record.
      def set_inverse_instance(record)
        @owner._set_inverse_instance(@name, record) if !record.nil? && @owner.respond_to?(:_set_inverse_instance)
        record
      end

      # Rails' CollectionAssociation#scope: an owner that is not saved
      # has no records (`null_scope?`).
      def scope
        relation = super
        relation = relation.none if @owner.new_record?
        relation.instance_variable_set(:@association, self)
        relation
      end

      # Rails' collection reader is the CollectionProxy, which loads
      # when enumerated; the model's reader is the runtime's answer for
      # it, loaded or not.
      def reader
        @owner.public_send(@name)
      end

      # A collection's target is an Array before anything loads, so a
      # caller may append to it (`association(:events).target << event`)
      # and read it back.
      #
      # Rails' target is the Array under the proxy, which holds what is
      # appended without saving it; the proxy itself, as the owner's
      # cache, saves on `<<`. So a proxy answers its `target` view.
      def target
        records = super
        if records.nil?
          records = []
          @owner.instance_variable_set(@cache_ivar, records)
        end
        records.is_a?(CollectionProxy) ? records.target : records
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
      # inverse: the name of the association on the target that points
      # back at the owner (Rails' `reflection.inverse_of`), nil for none.
      def initialize(owner, name, klass_name = nil, foreign_key = nil, scopable = false, primary_key = :id, inverse = nil)
        super(owner, name, klass_name, foreign_key, scopable)
        @primary_key = primary_key
        @inverse = inverse
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

      # Rails' `set_inverse_instance(record)`: the target record's
      # inverse association holds the owner, where the belongs_to has an
      # inverse (belongs_to_association.rb `invertible_for?`: a has_one,
      # or a has_many under `has_many_inversing`, which Core's
      # load_defaults enables). A has_many inverse that is not loaded
      # holds nothing to add the owner to: the emitted reader loads its
      # own records, which include the owner when it is saved.
      def set_inverse_instance(record)
        return record if @inverse.nil? || record.nil?
        inverse = record.association(@inverse)
        if inverse.is_a?(CollectionAssociation)
          if inverse.loaded?
            records = inverse.target
            Association.push_target(records, @owner) unless records.any? { |r| r.equal?(@owner) }
          end
        else
          inverse.inversed_from(@owner)
        end
        record
      end

      # Rails' belongs_to `inversed_from(record)`: the owner's key
      # follows the record, then it is the target.
      def inversed_from(record)
        replace_keys(record)
        super
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

      # belongs_to_association.rb `replace(record)`: the record is the
      # target and its key the owner's foreign key.
      def replace(record, _save = true)
        set_inverse_instance(record) if record
        replace_keys(record)
        self.target = record
      end

      private
        def initialize_attributes(record)
          set_inverse_instance(record)
        end

        # Rails' `replace_keys(record)`: the owner's foreign key is the
        # record's key (nil for no record).
        def replace_keys(record)
          @owner.public_send(:"#{@foreign_key}=", record.nil? ? nil : record.public_send(@primary_key))
        end

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
      # as_type: the polymorphic `as:` interface's type column, which
      # holds the owner's class name; dependent: the declaration's
      # `dependent:`; inverse: the target's belongs_to that points back
      # at the owner (Rails' `reflection.inverse_of`), nil for none.
      def initialize(owner, name, klass_name = nil, foreign_key = nil, scopable = false, as_type = nil, dependent = nil, inverse = nil, owner_key = :id)
        super(owner, name, klass_name, foreign_key, scopable, owner_key)
        @as_type = as_type
        @dependent = dependent
        @inverse = inverse
      end

      # Rails' has_one_association.rb `replace(record, save = true)`: a
      # different target than the held one removes the held one, as the
      # declaration's `dependent:` says, then the record joins the owner
      # (its key, type and inverse set) and is saved where the owner is.
      def replace(record, save = true)
        previous = @owner.persisted? ? load_target : target
        return previous if previous.nil? && record.nil?
        assigning_another_record = !previous.equal?(record)
        if assigning_another_record || record.has_changes_to_save?
          save &&= @owner.persisted?
          remove_target!(previous) if previous && !previous.destroyed? && assigning_another_record
          if record
            set_owner_attributes(record)
            set_inverse_instance(record)
            if save && !record.save
              nullify_owner_attributes(record)
              set_owner_attributes(previous) if previous
              raise ActiveRecord::RecordNotSaved.new("Failed to save the new associated #{@name}.", record)
            end
          end
        end
        self.target = record
      end

      # The record holds the owner through its inverse belongs_to
      # (Rails' `set_inverse_instance`, public in association.rb: Core's
      # ModelCaching calls it on a prefetched target), in the ivars the
      # emitted belongs_to reader keeps.
      def set_inverse_instance(record)
        return record if @inverse.nil? || record.nil?
        record.instance_variable_set(:"@#{@inverse}_cache", @owner)
        record.instance_variable_set(:"@#{@inverse}_loaded", true)
        record
      end

      private
        # has_one_association.rb `_create_record`: the owner must be saved.
        def _create_record(attributes, raise_error, &block)
          unless @owner.persisted?
            raise ActiveRecord::RecordNotSaved.new("You cannot call create unless the parent is saved", @owner)
          end
          super
        end

        # association.rb `build_record` + `initialize_attributes`: the
        # record starts with the association scope's `where` equalities
        # (`scope_for_create`, the owner already bound), except the keys
        # the caller passed; the key and `as:` type are the owner's.
        def build_record(attributes, &block)
          record = klass.new(attributes, &block)
          assign_scope_attributes(record, attributes)
          initialize_attributes(record)
          record
        end

        def assign_scope_attributes(record, given)
          query = :"_#{@name}_association_scope"
          return unless @owner.respond_to?(query)
          skip = [@foreign_key.to_s, (@as_type.to_s if @as_type)]
          skip.concat(given.keys.map(&:to_s)) if given.respond_to?(:keys)
          @owner.public_send(query).scope_for_create.each do |column, value|
            record.public_send(:"#{column}=", value) unless skip.include?(column.to_s)
          end
        end

        def initialize_attributes(record)
          set_owner_attributes(record)
          set_inverse_instance(record)
        end

        def set_owner_attributes(record)
          record.public_send(:"#{@foreign_key}=", @owner.new_record? ? nil : @owner.public_send(@owner_key))
          record.public_send(:"#{@as_type}=", @owner.class.name) if @as_type
        end

        def nullify_owner_attributes(record)
          record.public_send(:"#{@foreign_key}=", nil)
          record.public_send(:"#{@as_type}=", nil) if @as_type
        end

        # has_one_association.rb `remove_target!`.
        def remove_target!(previous)
          case @dependent
          when :delete
            previous.delete
          when :destroy, :destroy_async
            previous.destroy if previous.persisted?
          else
            nullify_owner_attributes(previous)
            if previous.persisted? && @owner.persisted? && !previous.save
              set_owner_attributes(previous)
              raise ActiveRecord::RecordNotSaved.new("Failed to remove the existing associated #{@name}. The record failed to save after its foreign key was set to nil.", previous)
            end
          end
        end
    end

    class HasManyAssociation < CollectionAssociation
    end

    class HasManyThroughAssociation < HasManyAssociation
      # The reader answers the association's own Relation (the through
      # joins and the source's scope), as Rails' ThroughAssociation
      # builds its scope.
      def scope
        relation = @owner.public_send(@name)
        unless relation.is_a?(ActiveRecord::Relation)
          raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).scope is not ported for a :through association whose reader answers records"
        end
        relation = relation.none if @owner.new_record?
        relation
      end

      # Rails' through stale state reads the through record's key when
      # the through association is a belongs_to.
      def stale_target?
        raise NotImplementedError, "roundhouse: #{@owner.class.name}#association(:#{@name}).stale_target? is not ported for a :through association"
      end
    end

    # What a has_many reader answers on the CRuby and JRuby trees
    # (`emit::ruby::library::apply_collection_memoization`): Rails'
    # CollectionProxy, over the records the reader loaded. Rails' reader
    # answers the proxy, a Relation for the association that loads when
    # enumerated; the emitted reader loads at once and keeps the
    # records, so the proxy is an Array of them that also answers the
    # association's own methods. The scope-chain lowering already runs a
    # chain it can see (`owner.items.where(…)`, `.create!`) on a
    # Relation; the proxy answers the calls it cannot: a gem's
    # `object.send(:transitions).create(fields)`, `first!` on the
    # reader's records, a relation method on a reader passed around.
    #
    # The proxy is the owner's target: `wrap` stores it in the
    # association's cache, so a record the proxy creates or builds joins
    # what the next read answers (`Association.created`, Rails'
    # `add_to_target`), and the association object reads the same
    # records. Its scope is the reader's own query
    # (`_<name>_association_scope`, emitted beside the reader: the key,
    # an `as:` type, the association's scope and its extension); a
    # reader that carries none falls back to `association(name).scope`.
    #
    # Ported (activerecord 8.2.0.alpha associations/collection_proxy.rb,
    # collection_association.rb): `build` / `new`, `create` / `create!`
    # (RecordNotSaved for an unsaved owner, "You cannot call create
    # unless the parent is saved"), `first!` / `last!` / `take!` over
    # the loaded records (RecordNotFound when there are none), `find`
    # with an id, a column `sum`, `destroy_all` / `delete_all` (which
    # empty the target), `reload`, `scope`, `proxy_association`,
    # `loaded?` (always: the reader loads), and any other Relation
    # method, which runs on the scope. Array's own methods keep Array's meaning, which for a
    # loaded proxy is Rails' (`first`, `size`, `each`, `include?`);
    # `<<` / `push` / `concat` are Rails' `concat` (`Association.concat`),
    # which the lowering also writes for a statically seen
    # `owner.items << record`.
    class CollectionProxy < Array
      # The reader's records as the proxy, stored as the owner's target.
      def self.wrap(owner, name, records)
        return records if records.is_a?(CollectionProxy) && records.proxy_owner.equal?(owner) && records.proxy_name == name
        proxy = new(owner, name, records || [])
        owner.instance_variable_set(:"@#{name}_cache", proxy)
        proxy
      end

      # The records of a collection cache as the Array under any proxy.
      def self.target_of(records)
        records.is_a?(CollectionProxy) ? records.target : records
      end

      attr_reader :proxy_owner, :proxy_name

      def initialize(owner, name, records)
        super(records)
        @proxy_owner = owner
        @proxy_name = name
      end

      ARRAY_PUSH = Array.instance_method(:<<)
      ARRAY_REPLACE = Array.instance_method(:replace)

      def proxy_association
        @proxy_owner.association(@proxy_name)
      end

      # The record held, not saved (Rails' `add_to_target`).
      def _push_target(record)
        ARRAY_PUSH.bind_call(self, record)
      end

      def _replace_target(records)
        ARRAY_REPLACE.bind_call(self, records)
      end

      # `owner.items << record` / `push` / `append` / `concat(records)`:
      # Rails' CollectionProxy#concat, which saves the record at once on
      # a saved owner (under its key, in one transaction) and keeps it
      # for the owner's save on a new one. The lowering writes a
      # statically seen `owner.items << record` as `Association.concat`;
      # this is the same for a receiver it could not name (an ambiguous
      # association name, a local, a block parameter). Answers the proxy.
      def <<(record)
        Association.concat(@proxy_owner, @proxy_name, record)
        self
      end

      def push(*records)
        Association.concat(@proxy_owner, @proxy_name, *records)
        self
      end
      alias_method :append, :push

      def concat(*records)
        Association.concat(@proxy_owner, @proxy_name, *records)
        self
      end

      # Rails' `target`: the Array under the proxy, which a caller may
      # change without saving anything (`association(:items).target <<
      # item`). A copy that writes every change back to the proxy.
      def target
        TargetArray.new(self)
      end

      # Rails' CollectionProxy#records: the loaded target as an Array.
      def records
        to_a
      end

      class TargetArray < Array
        def initialize(proxy)
          super(proxy)
          @proxy = proxy
        end

        %i[<< push append concat unshift prepend insert delete delete_at delete_if reject! select! filter! keep_if
           map! collect! compact! uniq! sort! sort_by! reverse! shuffle! rotate! flatten! pop shift clear replace
           fill slice! []=].each do |mutator|
          define_method(mutator) do |*args, **kwargs, &block|
            result = super(*args, **kwargs, &block)
            @proxy._replace_target(self)
            result.equal?(self) ? self : result
          end
        end
      end

      def loaded?
        true
      end

      # The relation answers an `extending(Mod)` declaration's methods,
      # which read `@association` (Rails' AssociationRelation holds the
      # association it scopes: Core's `OrderAdjustmentsExtension` reads
      # `@association.owner`), so it holds this proxy's.
      def scope
        reader_scope = :"_#{@proxy_name}_association_scope"
        if @proxy_owner.respond_to?(reader_scope)
          relation = @proxy_owner.public_send(reader_scope)
          # A `has_many ... extend: Ext` module reads Rails' `@association`
          # (`@association.owner`); the extended scope is the proxy's.
          if relation.is_a?(ActiveRecord::Relation) && @proxy_owner.respond_to?(:association)
            relation.instance_variable_set(:@association, proxy_association)
          end
          relation
        elsif @proxy_owner.respond_to?(:association)
          proxy_association.scope
        else
          raise NotImplementedError, "roundhouse: #{@proxy_owner.class.name}##{@proxy_name}: the association's scope is not emitted"
        end
      end

      def build(attributes = {}, &block)
        return attributes.map { |attrs| build(attrs, &block) } if attributes.is_a?(Array)
        record = scope.build(attributes)
        yield record if block
        Association.created(@proxy_owner, @proxy_name, record)
      end
      alias_method :new, :build

      def create(attributes = {}, &block)
        _create_record(attributes, false, &block)
      end

      def create!(attributes = {}, &block)
        _create_record(attributes, true, &block)
      end

      def first!
        first || raise_record_not_found
      end

      def last!
        last || raise_record_not_found
      end

      def take!
        first || raise_record_not_found
      end

      # `find(id)` is a query on the association (`Enumerable#find`'s
      # block form keeps its meaning).
      def find(*args, &block)
        return super(*args, &block) if block
        scope.find(*args)
      end

      # A column sum is a query (`Enumerable#sum` keeps the block and
      # numeric forms).
      def sum(*args, &block)
        return super(*args, &block) if block || args.empty? || !(args[0].is_a?(Symbol) || args[0].is_a?(String))
        scope.sum(*args)
      end

      # `select(:id, :delta)` is the scope's column projection (Rails'
      # `Relation#select` with fields); the block form filters the
      # records held (`Enumerable#select`).
      def select(*fields, &block)
        return super(&block) if fields.empty? || block
        scope.select(*fields)
      end

      # The rows go, and so do the records the owner holds.
      def destroy_all
        destroyed = scope.destroy_all
        clear
        destroyed
      end

      def delete_all
        deleted = scope.delete_all
        clear
        deleted
      end

      # Rails' CollectionAssociation#destroy: each record (an id finds
      # its record through the association first) is destroyed, in one
      # transaction when any is saved, and leaves the owner's target.
      # Answers the records, as Rails does.
      def destroy(*records)
        records = records.flatten
        if @proxy_owner.respond_to?(:association) && proxy_association.is_a?(HasManyThroughAssociation)
          raise NotImplementedError, "roundhouse: #{@proxy_owner.class.name}##{@proxy_name}.destroy is not ported for a :through association"
        end
        records = scope.find(records) if records.any? { |record| record.is_a?(Integer) || record.is_a?(String) }
        return records if records.empty?
        existing = records.reject(&:new_record?)
        if existing.empty?
          _drop_from_target(records)
        else
          Association.within_transaction do
            existing.each(&:destroy!)
            _drop_from_target(records)
            true
          end
        end
        records
      end

      # The association read again, as Rails' `proxy.reload`.
      def reload
        @proxy_owner.instance_variable_set(:"@#{@proxy_name}_loaded", false)
        @proxy_owner.public_send(@proxy_name)
      end

      # A method the proxy does not define goes to the association's scope,
      # and from there to the target class's class methods run under that
      # scope (`Event.log` through `shop.events.log`), as Rails'
      # CollectionProxy delegates to its scope and Relation::Delegation
      # delegates `klass.respond_to?(method)` through `scoping`.
      def method_missing(method, *args, **kwargs, &block)
        relation = scope
        return relation.public_send(method, *args, **kwargs, &block) if relation.respond_to?(method)
        model = relation.model
        return super unless model.respond_to?(method)
        relation.scoping { model.public_send(method, *args, **kwargs, &block) }
      end

      def respond_to_missing?(method, include_private = false)
        return false if method.to_s.start_with?("to_")
        relation = scope
        relation.respond_to?(method) || relation.model.respond_to?(method) || super
      rescue NotImplementedError
        super
      end

      # Rails' CollectionAssociation#delete: the records leave the
      # association as the declaration's `dependent:` says (destroyed,
      # deleted, or, with none, their foreign key nulled) and leave the
      # owner's target. Answers the records. A `:through` association
      # keeps Array's in-memory `delete`.
      ARRAY_DELETE = Array.instance_method(:delete)
      def delete(*records)
        if @proxy_owner.respond_to?(:association) && proxy_association.is_a?(HasManyThroughAssociation)
          return ARRAY_DELETE.bind_call(self, *records)
        end
        records = records.flatten
        records = scope.find(records) if records.any? { |record| record.is_a?(Integer) || record.is_a?(String) }
        return records if records.empty?
        existing = records.reject(&:new_record?)
        if existing.empty?
          _drop_from_target(records)
        else
          Association.within_transaction do
            _delete_records(existing)
            _drop_from_target(records)
            true
          end
        end
        records
      end

      # Rails' CollectionAssociation#replace (`owner.items = [...]`): the
      # records not in `other` are deleted, the ones not held are
      # concatenated (saved at once on a saved owner), in one transaction.
      # A record that does not save puts the target back and raises
      # RecordNotSaved.
      def replace(other)
        other = other.to_a
        original = Array.new(self)
        if @proxy_owner.new_record?
          _replace_records(other, original)
        else
          # Records both hold: the caller's instance is the one kept.
          other.each do |record|
            at = index { |held| held == record }
            _replace_target_at(at, record) unless at.nil?
          end
          if other != original
            Association.within_transaction do
              _replace_records(other, original)
              true
            end
          end
        end
        self
      end

      private
        # `@target -= records`: the records leave what the owner holds.
        def _drop_from_target(records)
          delete_if { |held| records.any? { |gone| gone.equal?(held) || gone == held } }
        end

        def _replace_target_at(at, record)
          ARRAY_REPLACE.bind_call(self, map.with_index { |held, i| i == at ? record : held })
        end

        # has_many_association.rb `delete_records`: `:destroy` destroys
        # each record, `:delete_all` deletes the rows, and any other
        # `dependent:` (none, `:nullify`) nulls the foreign key (and the
        # `as:` type) in the rows and the records held.
        def _delete_records(existing)
          reflection = @proxy_owner.class.reflect_on_association(@proxy_name)
          dependent = reflection.options[:dependent]
          case dependent
          when :destroy
            existing.each(&:destroy!)
          when :delete_all
            existing.each(&:delete)
          else
            columns = { reflection.foreign_key.to_s => nil } #: Hash[String, untyped]
            columns[reflection.type] = nil if reflection.type
            existing.each { |record| record.update_columns(columns) }
          end
        end

        def _replace_records(new_target, original)
          gone = reject { |held| new_target.any? { |kept| kept.equal?(held) || kept == held } }
          delete(*gone) unless gone.empty?
          added = new_target.reject { |record| any? { |held| held.equal?(record) || held == record } }
          Association.concat(@proxy_owner, @proxy_name, *added) unless added.empty?
          unless @proxy_owner.new_record? || added.all?(&:persisted?)
            ARRAY_REPLACE.bind_call(self, original)
            raise ActiveRecord::RecordNotSaved.new("Failed to replace #{@proxy_name} because one or more of the new records could not be saved.", @proxy_owner)
          end
        end

        def _create_record(attributes, raise_error, &block)
          if attributes.is_a?(Array)
            return attributes.map { |attrs| _create_record(attrs, raise_error, &block) }
          end
          unless @proxy_owner.persisted?
            raise ActiveRecord::RecordNotSaved.new("You cannot call create unless the parent is saved", @proxy_owner)
          end
          record = scope.build(attributes)
          yield record if block
          if raise_error
            raise ActiveRecord::RecordInvalid, record unless record.save
          else
            record.save
          end
          Association.created(@proxy_owner, @proxy_name, record)
        end

        def raise_record_not_found
          model = scope.klass
          raise ActiveRecord::RecordNotFound.new("Couldn't find #{model.name}", model.name, model.primary_key)
        end
    end

    # `Preloader.new(records:, associations:, scope:).call`
    # (associations/preloader.rb, preloader/branch.rb, preloader/
    # association.rb): loads the named associations of records a program
    # already holds. Each name of the spec (a Symbol, an Array of specs,
    # a Hash of a name to a nested spec) loads for the records whose
    # association is not loaded yet; a loaded one keeps its target, and
    # a collection keeps its unsaved records, as Rails'
    # `associate_records_to_owner` does. A nested spec loads on the
    # targets. The loads are the model's synthesized
    # `preload_associations` (an `includes`' batch loaders); a kind with
    # no batch loader loads per record through the association object,
    # the records Rails' one query loads.
    #
    # `scope:` narrows what loads at every level, and the association
    # holds just those records, as Rails': per record, the association's
    # scope with the scope's conditions merged in (a scope of another
    # model fails in the database, as in Rails). Only the conditions are
    # merged, not an order or limit. `available_records:` spares Rails a
    # query; it is accepted and not read.
    #
    # Differences: `call` answers nil, not Rails' loader objects. A
    # nested name loads on every record's target, where Rails, when only
    # some of the records had the association loaded, loads it on the
    # rows its query returned.
    class Preloader
      attr_reader :records, :associations, :scope

      def initialize(records:, associations:, scope: nil, available_records: [], associate_by_default: true)
        @records = records
        @associations = associations
        @scope = scope
        @associate_by_default = associate_by_default
      end

      def empty?
        associations.nil? || records.length == 0
      end

      def call
        unless @associate_by_default
          raise NotImplementedError, "roundhouse: Associations::Preloader with associate_by_default: false is not ported"
        end
        preload(@records.to_a, @associations)
        nil
      end

      private
        def preload(owners, spec)
          return if owners.empty?
          if spec.is_a?(Hash)
            spec.each { |name, nested| preload_name(owners, name.to_sym, nested) }
          elsif spec.is_a?(Array)
            spec.each { |each_spec| preload(owners, each_spec) }
          elsif !spec.nil?
            preload_name(owners, spec.to_sym, nil)
          end
        end

        def preload_name(owners, name, nested)
          pending = owners.reject { |owner| owner.association(name).loaded? }
          unsaved = pending.map { |owner| unsaved_records(owner.association(name)) }
          if @scope.nil?
            pending.group_by(&:class).each { |klass, group| klass.preload_associations(group, [name]) }
          end
          pending.each_with_index do |owner, i|
            association = owner.association(name)
            if @scope.nil?
              association.load_target
            else
              found = association.scope.merge(@scope).to_a
              association.target = association.is_a?(CollectionAssociation) ? found : found.first
            end
            association.target = association.target + unsaved[i] unless unsaved[i].empty?
          end
          preload(targets(owners, name), nested) unless nested.nil?
        end

        def unsaved_records(association)
          return [] unless association.is_a?(CollectionAssociation)
          association.target.reject(&:persisted?)
        end

        def targets(owners, name)
          out = []
          owners.each do |owner|
            target = owner.association(name).target
            if target.is_a?(Array)
              out.concat(target)
            elsif !target.nil?
              out << target
            end
          end
          out.uniq(&:object_id)
        end
    end
  end
end
