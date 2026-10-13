ActiveRecord::Schema.define do
  create_table "rooms", force: :cascade do |t|
    t.string "name"
  end
  create_table "messages", force: :cascade do |t|
    t.integer "room_id"
    t.string "body"
  end
end
