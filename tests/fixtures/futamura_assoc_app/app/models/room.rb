class Room < ApplicationRecord
  has_many :messages, dependent: :destroy, inverse_of: :room
  has_many :pinned, -> { where(pinned: true).order(:id) }, class_name: "Message"
  has_many :newest, -> { order("id DESC") }, class_name: "Message", foreign_key: :room_id
  has_many :notes, as: :subject
  has_many :scoped_messages, ->(room) { where(shop_id: room.shop_id) }, class_name: "Message"
  has_many :remarks, -> { where(pinned: true) }
  has_many :same_column, -> { where(room_id: 1) }, class_name: "Message"
end
