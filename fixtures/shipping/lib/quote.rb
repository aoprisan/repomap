require_relative "rates"

# Quotes a shipment from its zone and weight.
class Quote
  def total(zone, kilos)
    Rates.for_zone(zone) * kilos
  end
end
