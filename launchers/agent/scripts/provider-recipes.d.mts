export function recipeHash(recipe: Record<string, unknown>): string;
export function providerRecipes(bytes: Buffer): { table: unknown; table_hash: string; recipe_hashes: Record<string, string> };
