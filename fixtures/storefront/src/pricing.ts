/** Unit price in cents. */
export function unitPrice(sku: string): number {
  return sku === 'apple' ? 100 : 120;
}

/** A priced line item. */
export interface Priced {
  sku: string;
  qty: number;
}
