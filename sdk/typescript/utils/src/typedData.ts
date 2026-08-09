import { camelToSnake, recursiveTransform } from "./index.js";

import type {
  ArbitraryTypedData,
  EIP712Domain,
  EIP712Message,
  Json,
  Message,
  TypedData,
} from "@left-curve/types";

/**
 * @description Composes arbitrary typed data.
 *
 * @param parameters The parameters to compose the typed data.
 * @param parameters.message The typed message.
 * @param parameters.types The typed data types.
 * @param parameters.primaryType The primary type.
 * @returns The composed typed data.
 */
export function composeArbitraryTypedData(parameters: ArbitraryTypedData) {
  const { message, types, primaryType } = parameters;
  return {
    domain: {
      name: "DangoArbitraryMessage",
      chainId: 1,
      verifyingContract: "0x0000000000000000000000000000000000000000",
    },
    message: recursiveTransform(message, camelToSnake) as Record<string, unknown>,
    primaryType,
    types: {
      EIP712Domain: [
        { name: "name", type: "string" },
        { name: "chainId", type: "uint256" },
        { name: "verifyingContract", type: "address" },
      ],
      ...types,
    },
  };
}

/**
 * @description Composes the typed data for a transaction.
 *
 * @param message The typed message.
 * @param typeData The typed data parameters.
 * @returns The composed typed data
 */
export function composeTxTypedData(message: EIP712Message, domain: EIP712Domain): TypedData {
  const { messages, data, gas_limit, sender } = message;
  const { expiry } = data;

  return {
    types: {
      EIP712Domain: [
        { name: "name", type: "string" },
        { name: "chainId", type: "uint256" },
        { name: "verifyingContract", type: "address" },
      ],
      Message: [
        { name: "sender", type: "address" },
        { name: "data", type: "Metadata" },
        { name: "gas_limit", type: "uint32" },
        // EIP-712 has no sum type, so the `Message` enum can't be a struct;
        // each message is bound as its canonical JSON string. The EIP-712
        // signer (the `eip1193` connector) stringifies the message values just
        // before signing so they match this declared type and the chain's
        // reconstruction.
        { name: "messages", type: "string[]" },
      ],
      Metadata: [
        { name: "user_index", type: "uint32" },
        { name: "chain_id", type: "string" },
        { name: "nonce", type: "uint32" },
        ...(data.expiry ? [{ name: "expiry", type: "string" }] : []),
      ],
    },
    primaryType: "Message",
    domain,
    // `messages` stays as objects here: this SignDoc is shared with the raw
    // Secp256k1/Passkey signers, which sign SHA-256 over its canonical JSON
    // (message content as objects, matching the chain's SignDoc canonical
    // form). The EIP-712 signer converts them to canonical strings.
    message: {
      sender,
      data: recursiveTransform({ ...data, ...(expiry ? { expiry } : {}) }, camelToSnake) as Json,
      gas_limit,
      messages: recursiveTransform(messages, camelToSnake) as Message[],
    },
  };
}
