class Finder
  #: (Array[Integer]) -> untyped
  def self.by_ids(ids)
    Widget.where(id: ids).order(:id).to_a
  end

  #: (Integer, Array[Integer]) -> untyped
  def self.shop_ids(shop_id, ids)
    Widget.where(shop_id: shop_id, id: ids).order(:id).to_a
  end

  #: (Array[String]) -> untyped
  def self.by_codes(codes)
    Widget.where(code: codes).order(:id).to_a
  end

  #: (Array[Integer]) -> untyped
  def self.by_ranks(ranks)
    Widget.where(rank: ranks).order(:id).to_a
  end

  #: (Array[Time]) -> untyped
  def self.by_stamps(stamps)
    Widget.where(stamp: stamps).order(:id).to_a
  end

  #: (Array[Integer]) -> untyped
  def self.not_ids(ids)
    Widget.where.not(id: ids).order(:id).to_a
  end

  #: (Array[Integer]) -> untyped
  def self.unscoped_ids(ids)
    Widget.unscoped.where(id: ids).order(:id).to_a
  end

  #: (Array[Integer], Integer) -> untyped
  def self.first_of(ids, shop_id)
    Widget.where(shop_id: shop_id, id: ids).order(:id).first
  end

  #: (Widget, Array[Integer]) -> untyped
  def self.gadgets_of(widget, ids)
    widget.gadgets.where(id: ids).order(:id).to_a
  end

  #: (Array[Integer], Array[String]) -> untyped
  def self.two_lists(ids, codes)
    Widget.where(id: ids, code: codes).order(:id).to_a
  end

  #: (String) -> untyped
  def self.sprockets(code)
    Sprocket.where(code: code).order(:id).to_a
  end

  #: (String) -> untyped
  def self.sprockets_nested(code)
    Sprocket.where(code: code).preload(widget: :gadgets).order(:id).to_a
  end

  #: (Array[String]) -> untyped
  def self.sprockets_listed(codes)
    Sprocket.where(code: codes).order(:id).to_a
  end
end
