class Remark < ApplicationRecord
  default_scope { where(pinned: false) }
end
