# PostgreSQL SQLSTATE codes to the ActiveRecord exceptions apps rescue,
# shared by the PostgreSQL Db shims (roundhouse#91). Both drivers (the pg
# gem and spinel-pg) expose the code the same way:
#
#   rescue PG::Error => e
#     PgErrors.raise_mapped(e.result.error_field(PG::PG_DIAG_SQLSTATE).to_s, e.message)
#     raise
#
# The rescue must be typed (`PG::Error`), not bare: on Spinel a generic
# rescue dispatches `result` to the wrong accessor (spinel-pg README).
#
# The table is Rails 8.1's PostgreSQL adapter translation, limited to the
# classes this runtime defines (runtime/ruby/active_record/errors.rb).
# A code with no class here re-raises the driver's own PG::Error, which
# is what Rails does for codes it does not translate apart from wrapping
# them in StatementInvalid, a class this runtime does not have yet.
module PgErrors
  UNIQUE_VIOLATION = "23505"
  STRING_DATA_RIGHT_TRUNCATION = "22001"

  # Raises the mapped exception for `sqlstate`, or returns nil.
  def self.raise_mapped(sqlstate, message)
    if sqlstate == UNIQUE_VIOLATION
      raise ActiveRecord::RecordNotUnique, message
    elsif sqlstate == STRING_DATA_RIGHT_TRUNCATION
      raise ActiveRecord::ValueTooLong, message
    end
    nil
  end
end
