import type { Address, Base64, Client, Signer } from "@left-curve/types";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

export type StoreCodeParameters = {
  sender: Address;
  code: Base64;
};

export type StoreCodeReturnType = Promise<SignAndBroadcastTxReturnType>;

export async function storeCode(
  client: Client<Signer>,
  parameters: StoreCodeParameters,
): StoreCodeReturnType {
  const { sender, code } = parameters;
  const storeCodeMsg = { upload: { code } };

  return await signAndBroadcastTx(client, { sender, messages: [storeCodeMsg] });
}
