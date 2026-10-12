ActiveRecord::Schema.define do
  create_table "rooms", force: :cascade do |t|
    t.string "name"
    t.integer "shop_id"
  end
  create_table "messages", force: :cascade do |t|
    t.integer "room_id"
    t.integer "shop_id"
    t.string "body"
    t.string "kind"
    t.boolean "pinned", default: false
  end
  create_table "notes", force: :cascade do |t|
    t.integer "subject_id"
    t.string "subject_type"
    t.string "text"
  end
  create_table "remarks", force: :cascade do |t|
    t.integer "room_id"
    t.boolean "pinned", default: false
  end
end
