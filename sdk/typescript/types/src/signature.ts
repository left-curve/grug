import type { Base64 } from "./encoding.js";
import type { JsonValue } from "./encoding.js";
import type { Credential } from "./credential.js";
import type { ArbitraryTypedData, TypedData } from "./typedData.js";

export type SignDoc = TypedData;

export type ArbitraryDoc<T extends JsonValue = JsonValue> = ArbitraryTypedData<T>;

export type SignatureOutcome = {
  credential: Credential;
  signed: SignDoc;
};

export type ArbitrarySignatureOutcome = {
  credential: Credential;
  signed: JsonValue;
};

export type Signature =
  | { secp256k1: Secp256k1Signature }
  | { passkey: PasskeySignature }
  | { eip712: Eip712Signature };

export type Secp256k1Signature = Base64;

export type PasskeySignature = {
  sig: Base64;
  client_data: Base64;
  authenticator_data: Base64;
};

export type Eip712Signature = {
  sig: Base64;
  typed_data: Base64;
};

export type RawSignature = {
  r: string;
  s: string;
  v: number;
};
