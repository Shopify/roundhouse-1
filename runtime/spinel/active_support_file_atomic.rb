# frozen_string_literal: true

# File.atomic_write (Shopify/rails 5813559f4aed, 8.2.0.alpha
# activesupport/lib/active_support/core_ext/file/atomic.rb), verbatim;
# Rails' doc comments dropped. Rails' :load_active_support initializer
# loads it with active_support/all, so Core calls it with no require
# (`File.atomic_write(path) { |f| f.write(...) }`); the emitted tree loads
# it where a call names it (`project::BOOT_LOADED_CORE_EXTENSIONS`), and a
# native gem's `require "active_support/core_ext/file/atomic"` answers
# from runtime/features (`project::FRAMEWORK_FEATURE_PORTS`).
#
# The PostgreSQL CRuby tree only (`project::apply_postgres_bootstrap`).

require "securerandom"

class File
  def self.atomic_write(file_name, temp_dir = dirname(file_name))
    old_stat = begin
      File.stat(file_name)
    rescue SystemCallError
      nil
    end

    # Names can't be longer than 255B
    tmp_suffix = ".tmp.#{SecureRandom.hex}"
    tmp_name = ".#{basename(file_name).byteslice(0, 254 - tmp_suffix.bytesize)}#{tmp_suffix}"
    tmp_path = File.join(temp_dir, tmp_name)
    open(tmp_path, RDWR | CREAT | EXCL | SHARE_DELETE | BINARY) do |temp_file|
      temp_file.binmode

      if old_stat
        # Set correct permissions on new file
        begin
          chown(old_stat.uid, old_stat.gid, temp_file.path)
          # This operation will affect filesystem ACL's
          chmod(old_stat.mode, temp_file.path)
        rescue Errno::EPERM, Errno::EACCES
          # Changing file ownership failed, moving on.
        end
      end

      return_val = yield temp_file
      temp_file.close
    rescue => error
      temp_file.close rescue nil
      unlink(temp_file.path) rescue nil
      raise error
    else
      rename(temp_file.path, file_name)
      return_val
    end
  end
end
