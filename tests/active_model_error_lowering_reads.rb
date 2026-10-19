# What the lowered `error.type` read (`lower::errors_index`) asks of an
# ActiveModel::Error the ActiveModel::Validations port builds: `symbolic`
# and `kind`. `ErrorAttributes.objects` answers a record whose `errors` is
# the port's ActiveModel::Errors with those Errors (component/schema.rb's
# validator reads them off `errors.dup`), and the lowered read is
# `error.symbolic ? error.kind.to_sym : error.kind`, which must give
# Rails' `error.type`.
#
#   ruby tests/active_model_error_lowering_reads.rb
runtime = File.expand_path("../runtime/spinel", __dir__)
require File.join(runtime, "scaffold/ruby_overlay/runtime/active_support_core_ext")
require File.join(runtime, "module_delegate")
require File.join(runtime, "active_support_inflector")
require File.join(runtime, "active_support_i18n")
require File.join(runtime, "active_support_string_inflections")
require File.join(runtime, "rails_framework_locales")
require File.join(runtime, "active_support_concern")
require File.join(runtime, "active_model_validations")
require File.join(runtime, "active_model_errors")
I18n.enforce_available_locales = false

class Widget
  include ActiveModel::Validations
  attr_accessor :name, :code
  validates :name, presence: true
  validate { errors.add(:code, "is weird") }
end

widget = Widget.new
widget.valid?
copy = widget.errors.dup
objects = ActiveModel::ErrorAttributes.objects(copy)
raise "expected Rails' own Errors, got #{objects.map(&:class).uniq}" unless objects.all? { |e| e.instance_of?(ActiveModel::Error) }

# The lowered read of `error.type`, as `lower::errors_index` writes it.
lowered = objects.map { |error| [error.attribute.to_sym, error.symbolic ? error.kind.to_sym : error.kind] }
expected = objects.map { |error| [error.attribute.to_sym, error.type] }
raise "lowered #{lowered.inspect} != #{expected.inspect}" unless lowered == expected
raise "unexpected types #{lowered.inspect}" unless lowered == [[:name, :blank], [:code, "is weird"]]
raise "symbolic" unless objects[0].symbolic && !objects[1].symbolic
puts "ok #{lowered.inspect}"
