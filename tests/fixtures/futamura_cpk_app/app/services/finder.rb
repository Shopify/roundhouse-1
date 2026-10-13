class Finder
  #: (Order) -> untyped
  def self.lines(order)
    order.lines.order(:id).to_a
  end

  #: (Order) -> untyped
  def self.gifts(order)
    order.gifts.to_a
  end

  #: (Order) -> untyped
  def self.parcels(order)
    order.parcels.order(:id).to_a
  end

  #: (String) -> untyped
  def self.by_sku(sku)
    Line.where(sku: sku).order(:id).to_a
  end

  #: (Integer) -> untyped
  def self.of_order(order_id)
    Line.where(order_id: order_id).order(:id).to_a
  end

  #: (Order) -> untyped
  def self.crates(order)
    order.crates.order(:id).to_a
  end
end
