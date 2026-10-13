class Gadget < ApplicationRecord
  belongs_to :widget
  # A default scope that is an IN list: a statement of its own per length.
  default_scope { where(tenant_id: [1, 2, 3]) }
end
