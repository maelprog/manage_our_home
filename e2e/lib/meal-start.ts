import type { BrowserContext } from "@playwright/test";

// The first questions about meals (#820) open from the recipe list once per
// member, until `enhance.js` finds their mark in localStorage
// (`mhome.depart-repas.<user id>`). Every member a suite registers is new,
// so a suite about something else would land on them instead of the list.
// Its pages read the mark as present — the key carries an id the suite does
// not know beforehand, hence `getItem` rather than a stored value. The
// questions themselves are tested in tests/meal-start.spec.ts, without it.
export async function skipMealStart(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    const getItem = Storage.prototype.getItem;
    Storage.prototype.getItem = function (this: Storage, key: string) {
      return key.startsWith("mhome.depart-repas.") ? "1" : getItem.call(this, key);
    };
  });
}
