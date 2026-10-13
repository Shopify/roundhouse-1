class Order < ApplicationRecord
  # The probe widens these readers' keys to `[region_id, id, shop_id]` against
  # `[region_id, order_id, shop_id]` (tests/futamura_cpk_runtime.rb).
  has_many :lines, primary_key: :id, foreign_key: :order_id
  has_many :gifts, -> { where(gift: true).order(:id) }, class_name: "Line", primary_key: :id, foreign_key: :order_id
  has_many :parcels
  has_many :crates, primary_key: :id, foreign_key: :order_id
end
