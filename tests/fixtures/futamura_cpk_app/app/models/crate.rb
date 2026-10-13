class Crate < ApplicationRecord
  belongs_to :order
  # A tenant condition on a column the order's composite key names too.
  default_scope { where(shop_id: 1).where.not(sku: "hidden") }
end
