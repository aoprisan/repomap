import { unitPrice, Priced } from './pricing';

/** A cart of priced line items. */
export class Cart {
  items: Priced[] = [];

  add(item: Priced): void {
    this.items.push(item);
  }

  total(): number {
    return this.items.reduce((sum, i) => sum + unitPrice(i.sku) * i.qty, 0);
  }
}
