def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message]
  end
  puts "#{label}: #{value.inspect}"
end
NAMES = %w[
  ActiveRecordError SubclassNotFound AssociationTypeMismatch SerializationTypeMismatch
  AdapterNotSpecified TableNotSpecified AdapterNotFound AdapterError ConnectionNotEstablished
  ConnectionTimeoutError ConnectionNotDefined DatabaseConnectionError ExclusiveConnectionTimeoutError
  ReadOnlyError ShardSwapProhibitedError RecordNotFound RecordNotSaved RecordNotDestroyed
  SoleRecordExceeded StatementInvalid WrappedDatabaseException RecordNotUnique InvalidForeignKey
  MismatchedForeignKey NotNullViolation CheckViolation ExclusionViolation ValueTooLong RangeError
  SQLWarning PreparedStatementInvalid NoDatabaseError DatabaseAlreadyExists PreparedStatementCacheExpired
  StaleObjectError ConfigurationError ReadOnlyRecord StrictLoadingViolationError Rollback
  DangerousAttributeError AttributeAssignmentError MultiparameterAssignmentErrors UnknownPrimaryKey
  UnmodifiableRelation TransactionIsolationError TransactionRollbackError
  AsynchronousQueryInsideTransactionError SerializationFailure Deadlocked MissingRequiredOrderError
  IrreversibleOrderError QueryAborted LockWaitTimeout StatementTimeout QueryCanceled AdapterTimeout
  ConnectionFailed UnknownAttributeReference DatabaseVersionError DeprecatedAssociationError
  AssociationNotFoundError RecordInvalid
]
NAMES.each do |name|
  klass = ActiveRecord.const_get(name, false)
  chain = klass.ancestors.select { |m| m.is_a?(Class) }.take_while { |c| c != StandardError }.map(&:name)
  puts "#{name}: #{chain.join(" < ")}"
end
NAMES.reject { |n| %w[RecordInvalid MismatchedForeignKey].include?(n) }.each do |name|
  seen("#{name}.new.message") { ActiveRecord.const_get(name, false).new.message }
end
seen("RecordNotFound is_a ActiveRecordError") { ActiveRecord::RecordNotFound.new.is_a?(ActiveRecord::ActiveRecordError) }
seen("ConnectionTimeoutError < ConnectionNotEstablished") { ActiveRecord::ConnectionTimeoutError < ActiveRecord::ConnectionNotEstablished }
seen("RecordNotSaved record") { e = ActiveRecord::RecordNotSaved.new("m", :r); [e.message, e.record] }
seen("RecordNotDestroyed record") { e = ActiveRecord::RecordNotDestroyed.new("m", :r); [e.message, e.record] }
seen("RecordNotFound readers") { e = ActiveRecord::RecordNotFound.new("m", "Post", "id", 3); [e.message, e.model, e.primary_key, e.id] }
seen("AdapterError pool") { e = ActiveRecord::AdapterError.new("m", connection_pool: :pool); [e.message, e.connection_pool] }
seen("ConnectionNotEstablished pool") { e = ActiveRecord::ConnectionNotEstablished.new("m", connection_pool: :pool); [e.message, e.connection_pool] }
seen("set_pool") { e = ActiveRecord::ConnectionNotEstablished.new("m"); [e.set_pool(:a).equal?(e), e.set_pool(:b).connection_pool] }
seen("ConnectionNotDefined readers") { e = ActiveRecord::ConnectionNotDefined.new("m", connection_name: "primary", role: :writing, shard: :default); [e.message, e.connection_name, e.role, e.shard, e.connection_pool] }
seen("StatementInvalid statement") { e = ActiveRecord::StatementInvalid.new("m", sql: "SELECT 1", binds: [1], connection_pool: :pool); [e.message, e.sql, e.binds, e.connection_pool] }
seen("set_query") { e = ActiveRecord::RecordNotUnique.new("m"); e.set_query("S1", [1]); e.set_query("S2", [2]); [e.sql, e.binds] }
seen("RecordNotUnique keywords") { e = ActiveRecord::RecordNotUnique.new("dup", sql: "INSERT", binds: [], connection_pool: :p); [e.message, e.sql, e.connection_pool] }
seen("NoDatabaseError pool") { e = ActiveRecord::NoDatabaseError.new(connection_pool: :p); [e.message, e.connection_pool] }
seen("NoDatabaseError.db_error") { ActiveRecord::NoDatabaseError.db_error("core_test").message }
seen("DatabaseConnectionError.hostname_error") { ActiveRecord::DatabaseConnectionError.hostname_error("db.local").message }
seen("DatabaseConnectionError.username_error") { ActiveRecord::DatabaseConnectionError.username_error("root").message }
seen("SQLWarning") { e = ActiveRecord::SQLWarning.new("w", "01000", "Warning", "SELECT", :p); e.sql = "S2"; [e.message, e.code, e.level, e.sql, e.connection_pool] }
seen("MismatchedForeignKey") { ActiveRecord::MismatchedForeignKey.new(message: "orig").message }
seen("StaleObjectError") { ActiveRecord::StaleObjectError.new("rec", "update").message }
seen("SoleRecordExceeded") { ActiveRecord::SoleRecordExceeded.new(String).message }
seen("AttributeAssignmentError") { e = ActiveRecord::AttributeAssignmentError.new("m", :exc, "title"); [e.message, e.exception, e.attribute] }
seen("MultiparameterAssignmentErrors") { e = ActiveRecord::MultiparameterAssignmentErrors.new([1]); [e.message, e.errors] }
seen("AssociationNotFoundError") { e = ActiveRecord::AssociationNotFoundError.new("rec", :posts); [e.message, e.record, e.association_name] }
seen("rescue TransactionRollbackError") do
  begin
    raise ActiveRecord::Deadlocked, "deadlock detected"
  rescue ActiveRecord::TransactionRollbackError => e
    "caught #{e.class}: #{e.message}"
  end
end
seen("rescue AdapterError") do
  begin
    raise ActiveRecord::ConnectionFailed, "gone"
  rescue ActiveRecord::AdapterError => e
    "caught #{e.class}"
  end
end
seen("rescue ActiveRecordError") do
  begin
    raise ActiveRecord::ReadOnlyRecord
  rescue ActiveRecord::ActiveRecordError => e
    "caught #{e.class}: #{e.message}"
  end
end
seen("granular ConnectionTimedout") { [ActiveRecord::ConnectionTimedout.superclass, ActiveRecord::ConnectionTimedout.new("t").is_a?(ActiveRecord::AdapterError)] }
seen("granular TransactionTooLargeError") { [ActiveRecord::TransactionTooLargeError.superclass, ActiveRecord::TransactionTooLargeError.ancestors.include?(ActiveRecord::ActiveRecordError)] }
seen("granular rescue") do
  begin
    raise ActiveRecord::TransactionTooLargeError, "too large"
  rescue ActiveRecord::AdapterError => e
    "caught #{e.class}: #{e.message}"
  end
end
