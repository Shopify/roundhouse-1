class Message < ApplicationRecord
  belongs_to :room
  scope :visible, -> { where(kind: "visible") }
end
