import { relatedFor } from "@hraness/design-kit/portfolio";

import { relatedProduct, registeredRelationship } from "../messaging";

/** Keep shared card identities while taking current relations and copy from the approved snapshot. */
export const articleRelatedProducts = relatedFor("gobstopper").flatMap((item) => {
  const relationship = registeredRelationship("gobstopper", item.productId);
  if (relationship === null) return [];
  return [{ ...item, ...relatedProduct(item.productId), relationship }];
});
