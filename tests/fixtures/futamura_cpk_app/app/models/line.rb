class Line < ApplicationRecord
  belongs_to :order
  default_scope { where.not(sku: "hidden") }
end
