class Note < ApplicationRecord
  belongs_to :subject, polymorphic: true
end
