import { encodeBase64, encodeUtf8 } from "@left-curve/encoding";
import type { Address, Client, Funds, Hex, Json, Signer } from "@left-curve/types";
import { computeAddress } from "#account/address.js";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

export type InstantiateParameters = {
  sender: Address;
  codeHash: Hex;
  msg: Json;
  salt: Uint8Array | string;
  funds?: Funds;
  admin?: Address;
  gasLimit?: number;
};

export type InstantiateReturnType = Promise<[string, Awaited<SignAndBroadcastTxReturnType>]>;

export async function instantiate(
  client: Client<Signer>,
  parameters: InstantiateParameters,
): InstantiateReturnType {
  const { sender, msg, codeHash, salt: _salt_, admin, gasLimit, funds = {} } = parameters;

  const salt = typeof _salt_ === "string" ? encodeUtf8(_salt_) : _salt_;

  const address = computeAddress({ deployer: sender, codeHash, salt });

  const instantiateMsg = {
    instantiate: {
      codeHash,
      msg,
      salt: encodeBase64(salt),
      funds,
      admin,
    },
  };

  const txHash = await signAndBroadcastTx(client, {
    sender,
    messages: [instantiateMsg],
    gasLimit,
  });

  return [address, txHash];
}
