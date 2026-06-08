package billing

import scala.collection.mutable
import billing.tax.TaxCalculator

/** Domain model for a customer invoice. */
case class Invoice(id: String, amountCents: Long, currency: String)

trait Repository[A] {
  def get(id: String): Option[A]
  def save(a: A): Unit
}

object InvoiceService extends Repository[Invoice] {
  private val store = mutable.Map.empty[String, Invoice]

  // Look up an invoice by id.
  def get(id: String): Option[Invoice] = store.get(id)

  def save(inv: Invoice): Unit = store.put(inv.id, inv)

  def total(id: String): Long = {
    val base = get(id).map(_.amountCents).getOrElse(0L)
    TaxCalculator.withTax(base)
  }
}
