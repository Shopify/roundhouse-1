# frozen_string_literal: true
require_relative "active_record/errors"
# The rest of Rails' exception hierarchy, Ruby-family only (not on the
# strict-target transpile tables, unlike errors.rb).
require_relative "active_record/error_classes"
require_relative "active_record/encryption_errors"
require_relative "active_record/connection_pool"
require_relative "active_record/registry"
require_relative "active_record/base"
# After base: connection.rb reopens Base with the raw-SQL surface
# (`connection`/`transaction`); loading it first would define an empty
# Base the real one then clobbers.
require_relative "active_record/connection"
# Signed ids (`record.signed_id` / `Model.find_signed`) — like
# connection.rb a reopen-tier file the strict targets do not stage: it
# calls into ActionController::MessageVerifier, whose PBKDF2/HMAC
# primitives ship only with the ruby-family trees.
require_relative "active_record/signed_id"
# has_secure_password's reset token — same tier as signed_id.
require_relative "active_record/token_for"
# friendly_id's finder rule (Relation#friendly_find).
require_relative "active_record/friendly_id"
require_relative "active_record/arel"
require_relative "active_record/relation"
require_relative "active_record/preloading"

# activerecord gem_version.rb and version.rb, from Shopify/rails
# 5813559f4aed (8.2.0.alpha), which is the Rails Core runs
# (docs/RAILS_VERSION.md). Gems branch on `ActiveRecord.version` in
# their module bodies. softer_delete 0.9.0's common.rb does
# `if ActiveRecord.version >= "7.2.0.alpha"`, and kafka-shopify's
# transaction tracker does `return if ::ActiveRecord.version < "7.2"`.
# So boot calls it as each carried gem loads, and a missing reader stops
# the boot. The value Core's Rails reports is the one to answer, so each
# branch goes the way it does natively. A `Gem::Version` compares with a
# version String. A natively loaded gem can call these readers too, so
# the text shake (emit::ruby::shake) must keep them: the names
# `version` and `gem_version` in this comment count as uses.
module ActiveRecord
  # Returns the currently loaded version of Active Record as a +Gem::Version+.
  def self.gem_version
    Gem::Version.new VERSION::STRING
  end

  module VERSION
    MAJOR = 8
    MINOR = 2
    TINY  = 0
    PRE   = "alpha"

    STRING = [MAJOR, MINOR, TINY, PRE].compact.join(".")
  end

  # Returns the currently loaded version of Active Record as a +Gem::Version+.
  def self.version
    gem_version
  end
end
require_relative "active_record/types"
# Database tasks (`db:schema:load`, `prepare_all`, `migrate`). After
# connection.rb: the task runner calls `Connection`. The readers load
# before `tasks.rb`, which prepends the app rake file onto the methods
# they defined.
require_relative "rake/task"
require_relative "active_record/schema_reader"
require_relative "active_record/migration_runner"
require_relative "active_record/tasks"
