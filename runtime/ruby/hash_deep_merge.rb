module ActiveSupport
  # Shared implementation used by the typed Hash#deep_merge call lowering.
  # Recursively combine only Hash/Hash value pairs; any other pair is the
  # right-hand value, or what the conflict block answers for (key,
  # left value, right value), as Rails 8.1.4's Hash#deep_merge does.
  def self.deep_merge(left, other_hash, &block)
    deep_merge!(left.dup, other_hash, &block)
  end

  # The mutating form: `left` takes the merge and is answered.
  def self.deep_merge!(left, other_hash, &block)
    left.merge!(other_hash) do |key, this_value, other_value|
      if this_value.is_a?(Hash) && other_value.is_a?(Hash)
        ActiveSupport.deep_merge(this_value, other_value, &block)
      elsif block
        block.call(key, this_value, other_value)
      else
        other_value
      end
    end
  end
end
