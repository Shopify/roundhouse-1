# frozen_string_literal: true
# `ActiveSupport.blank?(value)` on the CRuby and JRuby trees: Rails'
# own dispatch, `value.blank?`.
#
# runtime/active_support_ext.rb (the shared body the spinel tree also
# compiles) answers class by class, with Rails' `Object#blank?` as the
# tail. Here every object has `blank?` (active_support_core_ext.rb
# reopens Object and String as activesupport's core_ext does), so the
# call can be the send Rails makes: an object's own `blank?` answers
# (ActiveRecord::Relation's, ActionText::Content's, an app class's), as
# does String's retry in the string's own encoding.
module ActiveSupport
  def self.blank?(value)
    value.blank?
  end
end
