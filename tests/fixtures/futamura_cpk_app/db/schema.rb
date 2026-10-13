ActiveRecord::Schema.define do
  create_table "orders", force: :cascade do |t|
    t.integer "shop_id"
    t.integer "region_id"
  end
  create_table "lines", force: :cascade do |t|
    t.integer "order_id"
    t.integer "shop_id"
    t.integer "region_id"
    t.string "sku"
    t.boolean "gift", default: false
  end
  create_table "parcels", force: :cascade do |t|
    t.integer "order_id"
    t.integer "shop_id"
    t.integer "region_id"
  end
  create_table "crates", force: :cascade do |t|
    t.integer "order_id"
    t.integer "shop_id"
    t.integer "region_id"
    t.string "sku"
  end
end
