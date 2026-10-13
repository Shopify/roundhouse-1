class Room < ApplicationRecord
  has_many :messages, inverse_of: :room
  has_many :ordered_messages, -> { order(id: :desc) }, class_name: "Message", inverse_of: :room
  has_many :strict_messages, class_name: "Message", foreign_key: :room_id, inverse_of: :room, strict_loading: true
end
