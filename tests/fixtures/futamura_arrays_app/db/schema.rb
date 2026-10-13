ActiveRecord::Schema.define do
  create_table "widgets", force: :cascade do |t|
    t.integer "shop_id"
    t.string "code"
    t.integer "rank"
    t.datetime "stamp"
    t.string "state"
  end
  create_table "gadgets", force: :cascade do |t|
    t.integer "widget_id"
    t.integer "tenant_id"
    t.string "name"
  end
  create_table "sprockets", force: :cascade do |t|
    t.integer "widget_id"
    t.string "code"
  end
end
