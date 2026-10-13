class Room < ApplicationRecord
  has_many :messages, inverse_of: :room
  has_many :ordered_messages, -> { order(id: :desc) }, class_name: "Message", inverse_of: :room
end
