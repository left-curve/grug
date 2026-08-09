import { encodeBase64 } from "@left-curve/encoding";
import type { Address, Base64, Client, Funds, Hex, Json, Signer } from "@left-curve/types";
import { computeAddress } from "#account/address.js";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

export type StoreCodeAndInstantiateParameters = {
  sender: Address;
  codeHash: Hex;
  msg: Json;
  salt: Uint8Array;
  funds?: Funds;
  code: Base64;
  admin?: Address;
};

export type StoreCodeAndInstantiateReturnType = Promise<
  [string, Awaited<SignAndBroadcastTxReturnType>]
>;

export async function storeCodeAndInstantiate(
  client: Client<Signer>,
  parameters: StoreCodeAndInstantiateParameters,
): StoreCodeAndInstantiateReturnType {
  const { sender, msg, codeHash, funds = {}, salt, code, admin } = parameters;
  const address = computeAddress({ deployer: sender, codeHash, salt });

  const instantiateMsg = {
    instantiate: {
      codeHash,
      msg,
      salt: encodeBase64(salt),
      funds,
      admin: admin,
    },
  };

  const storeCodeMsg = { upload: { code } };

  const txData = await signAndBroadcastTx(client, {
    sender,
    messages: [storeCodeMsg, instantiateMsg],
  });

  return [address, txData];
}
