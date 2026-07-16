# Flat per-zone shipping rates.
class Rates
  ZONES = { "eu" => 500, "us" => 700 }.freeze

  def self.for_zone(zone)
    ZONES.fetch(zone, 900)
  end
end
