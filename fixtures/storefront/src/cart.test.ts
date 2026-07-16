import { Cart } from './cart';

/** Exercises Cart#total. */
export function cartTotalsLineItems(): boolean {
  const cart = new Cart();
  cart.add({ sku: 'apple', qty: 2 });
  return cart.total() === 200;
}
