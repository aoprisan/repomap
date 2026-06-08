package billing.tax

object TaxCalculator {
  val rate: Double = 0.2
  def withTax(cents: Long): Long = cents + (cents * rate).toLong
}
