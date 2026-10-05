import { createHash, randomBytes } from "node:crypto";

// Les jetons des liens envoyés par e-mail (vérification, réinitialisation,
// invitation) ne sont plus stockés que par leur empreinte depuis #335 : la
// suite ne peut plus les relire en base. Elle fait l'inverse — elle tire un
// jeton, pose son empreinte sur la ligne que l'API vient de créer
// (`db.ts`), et se sert du jeton. Même construction qu'apps/api
// (`auth::token`) : 32 octets aléatoires, base64url sans bourrage, SHA-256
// des octets.

export interface BearerToken {
	/** Ce que porte le lien : 43 caractères base64url. */
	token: string;
	/** Ce que garde la colonne `token_hash` : SHA-256 des 32 octets. */
	hash: Buffer;
}

export function bearerTokenFrom(bytes: Buffer): BearerToken {
	return {
		token: bytes.toString("base64url"),
		hash: createHash("sha256").update(bytes).digest(),
	};
}

export function newBearerToken(): BearerToken {
	return bearerTokenFrom(randomBytes(32));
}
