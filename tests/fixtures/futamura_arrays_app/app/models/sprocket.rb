class Sprocket < ApplicationRecord
  belongs_to :widget
  # A default scope that only preloads: the same tree at every call.
  default_scope { preload(:widget) }
end
